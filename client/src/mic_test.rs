//! Проверка микрофона до входа в комнату: несколько секунд записи, затем
//! записанное проигрывается обратно. Запись и воспроизведение не идут
//! одновременно, поэтому без наушников не будет свиста из динамиков.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ringbuf::traits::{Consumer, Observer, Producer};
use vwf_proto::{FRAME_SAMPLES, SAMPLE_RATE};

use crate::audio::{self, Wakeup};
use crate::resample::Resampler;

/// Сколько ждать звука от устройства, прежде чем проверить флаг остановки.
const WAKE_TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Recording,
    Playing,
    Done,
    Failed,
}

impl Phase {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Recording,
            1 => Self::Playing,
            2 => Self::Done,
            _ => Self::Failed,
        }
    }
}

#[derive(Default)]
struct Shared {
    phase: AtomicU8,
    level_db_bits: AtomicU32,
    /// Доля пройденного этапа, 0..1000.
    progress: AtomicU32,
    error: Mutex<Option<String>>,
}

/// Идущая проверка. Пока значение живо, проверка идёт; при удалении — останавливается.
pub struct MicTest {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Состояние проверки для интерфейса.
pub struct MicTestStatus {
    pub phase: Phase,
    /// Громкость микрофона во время записи, dBFS.
    pub level_db: f32,
    /// Доля пройденного этапа (записи или воспроизведения), 0..1.
    pub progress: f32,
    pub error: Option<String>,
}

impl MicTest {
    /// Начинает проверку: `seconds` секунд записи с `input`, затем
    /// воспроизведение на `output` (`None` — устройство по умолчанию).
    pub fn start(input: Option<String>, output: Option<String>, seconds: f32) -> Self {
        let shared = Arc::new(Shared::default());
        shared.level_db_bits.store((-120f32).to_bits(), Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_shared = shared.clone();
        let thread_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("vwf-mic-test".into())
            .spawn(move || {
                let result = run(&thread_shared, &thread_stop, input, output, seconds);
                thread_shared.level_db_bits.store((-120f32).to_bits(), Ordering::Relaxed);
                let phase = match result {
                    Ok(()) => Phase::Done,
                    Err(e) => {
                        *thread_shared.error.lock().unwrap() = Some(format!("{e:#}"));
                        Phase::Failed
                    }
                };
                thread_shared.phase.store(phase as u8, Ordering::Relaxed);
            })
            .ok();
        if thread.is_none() {
            *shared.error.lock().unwrap() = Some("не удалось запустить проверку".into());
            shared.phase.store(Phase::Failed as u8, Ordering::Relaxed);
        }
        Self { shared, stop, thread }
    }

    pub fn status(&self) -> MicTestStatus {
        MicTestStatus {
            phase: Phase::from_u8(self.shared.phase.load(Ordering::Relaxed)),
            level_db: f32::from_bits(self.shared.level_db_bits.load(Ordering::Relaxed)),
            progress: self.shared.progress.load(Ordering::Relaxed) as f32 / 1000.0,
            error: self.shared.error.lock().unwrap().clone(),
        }
    }
}

impl Drop for MicTest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    shared: &Shared,
    stop: &AtomicBool,
    input: Option<String>,
    output: Option<String>,
    seconds: f32,
) -> anyhow::Result<()> {
    let wakeup = Arc::new(Wakeup::default());
    wakeup.register();
    let total = (seconds * SAMPLE_RATE as f32) as usize;
    let mut recorded = Vec::with_capacity(total);

    // Запись.
    {
        let (_capture, mut mic) = audio::start_capture(input.as_deref(), wakeup.clone())?;
        let mut frame = [0f32; FRAME_SAMPLES];
        while recorded.len() < total {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if mic.occupied_len() < FRAME_SAMPLES {
                wakeup.wait(WAKE_TIMEOUT);
                continue;
            }
            mic.pop_slice(&mut frame);
            shared.level_db_bits.store(level_db(&frame).to_bits(), Ordering::Relaxed);
            recorded.extend_from_slice(&frame);
            shared.progress.store((recorded.len() * 1000 / total) as u32, Ordering::Relaxed);
        }
    }
    shared.level_db_bits.store((-120f32).to_bits(), Ordering::Relaxed);
    shared.progress.store(0, Ordering::Relaxed);
    shared.phase.store(Phase::Playing as u8, Ordering::Relaxed);

    // Воспроизведение записанного.
    let (playback, mut speaker) = audio::start_playback(output.as_deref(), wakeup.clone())?;
    let mut resampler = Resampler::new(SAMPLE_RATE, playback.config.sample_rate);
    let mut next = 0;
    let mut pending = Vec::new();
    while next < recorded.len() || !pending.is_empty() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if pending.is_empty() {
            let end = (next + FRAME_SAMPLES).min(recorded.len());
            for &s in &recorded[next..end] {
                resampler.push(s, |out| pending.push(out));
            }
            next = end;
            shared.progress.store((next * 1000 / recorded.len()) as u32, Ordering::Relaxed);
        }
        let pushed = speaker.push_slice(&pending);
        pending.drain(..pushed);
        if !pending.is_empty() {
            wakeup.wait(WAKE_TIMEOUT);
        }
    }
    // Дожидаемся, пока устройство доиграет то, что осталось в буфере.
    while speaker.occupied_len() > 0 && !stop.load(Ordering::Relaxed) {
        wakeup.wait(WAKE_TIMEOUT);
    }
    thread::sleep(Duration::from_millis(100));
    Ok(())
}

/// Громкость кадра, dBFS (среднеквадратичная).
fn level_db(frame: &[f32]) -> f32 {
    let energy = frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32;
    if energy <= 0.0 { -120.0 } else { (10.0 * energy.log10()).max(-120.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_minus_120_db() {
        assert_eq!(level_db(&[0.0; 960]), -120.0);
    }

    #[test]
    fn full_scale_square_is_0_db() {
        assert!(level_db(&[1.0; 960]).abs() < 0.01);
    }

    #[test]
    fn phase_roundtrips() {
        for phase in [Phase::Recording, Phase::Playing, Phase::Done, Phase::Failed] {
            assert_eq!(Phase::from_u8(phase as u8), phase);
        }
    }
}

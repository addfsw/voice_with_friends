//! Голосовой конвейер.
//!
//! Передача:  микрофон → [кольцевой буфер] → кадры по 20 мс → VAD → Opus → отправка
//! Приём:     пакет → джиттер-буфер участника → декодер Opus → микшер → [кольцевой буфер] → динамики
//!
//! Обе половины работают в своих потоках и общаются с аудиоколбэками
//! только через lock-free кольцевые буферы. Аудиоколбэк после каждой порции
//! звука будит свой поток (`Wakeup`), поэтому кадр кодируется сразу, как только
//! набрался, а микшер доливает звук сразу, как только карта его забрала.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context;
use ringbuf::traits::{Consumer, Observer, Producer};
use ringbuf::{HeapCons, HeapProd};
use vwf_proto::{FRAME_MS, FRAME_SAMPLES, PeerId, SAMPLE_RATE};

use crate::audio::Wakeup;
use crate::jitter::{Delay, JitterBuffer, Pop};
use crate::resample::Resampler;

/// Рабочие потоки просыпаются по сигналу из аудиоколбэка; этот таймаут —
/// только страховка на случай, если сигнал не придёт (например, устройство отвалилось).
const WAKE_TIMEOUT: Duration = Duration::from_millis(20);
/// Максимальный размер Opus-пакета (с запасом; реальные — 60–150 байт).
const MAX_PACKET: usize = 1275;

// ---------------------------------------------------------------------------
// Передача
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct TxSettings {
    /// Битрейт Opus, бит/с. Для голоса 24–32 кбит/с — уже отличное качество.
    pub bitrate: i32,
    /// Порог активации голосом в dBFS; `None` — передавать всегда.
    pub vad_threshold_db: Option<f32>,
}

/// Общее состояние передатчика, которое можно читать и менять из других потоков.
#[derive(Default)]
pub struct TxState {
    pub muted: AtomicBool,
    /// Идёт ли сейчас передача (для индикатора).
    pub transmitting: AtomicBool,
    /// Громкость последнего кадра в dBFS (f32, упакованный в биты u32).
    level_db_bits: AtomicU32,
    stop: AtomicBool,
}

impl TxState {
    pub fn level_db(&self) -> f32 {
        f32::from_bits(self.level_db_bits.load(Ordering::Relaxed))
    }
}

/// Простейший детектор речи по громкости с «хвостом»: после того как голос
/// стих, передача продолжается ещё `HANGOVER_FRAMES` кадров, чтобы не обрезать
/// окончания слов.
struct Vad {
    threshold_db: Option<f32>,
    hangover_left: u32,
}

const HANGOVER_FRAMES: u32 = 300 / FRAME_MS; // 300 мс

impl Vad {
    fn is_active(&mut self, level_db: f32) -> bool {
        let Some(threshold) = self.threshold_db else { return true };
        if level_db >= threshold {
            self.hangover_left = HANGOVER_FRAMES;
            true
        } else if self.hangover_left > 0 {
            self.hangover_left -= 1;
            true
        } else {
            false
        }
    }
}

fn level_db(frame: &[f32]) -> f32 {
    let mean_square = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
    10.0 * mean_square.max(1e-12).log10()
}

pub struct Transmitter {
    pub state: Arc<TxState>,
    thread: Option<JoinHandle<()>>,
}

impl Transmitter {
    /// Запускает поток кодирования. `send(seq, opus)` вызывается для каждого
    /// готового пакета. `seq` — номер 20-мс кадра: он растёт с каждым кадром
    /// с микрофона, даже если кадр не отправлен (тишина, mute). Так получатель
    /// по номеру знает, когда кадр был записан, и может измерять джиттер.
    /// `wakeup` — тот же, что передан в `audio::start_capture`.
    pub fn spawn(
        mut capture: HeapCons<f32>,
        wakeup: Arc<Wakeup>,
        settings: TxSettings,
        mut send: impl FnMut(u32, &[u8]) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let mut encoder =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)
                .context("не удалось создать кодер Opus")?;
        encoder.set_bitrate(opus::Bitrate::Bits(settings.bitrate))?;
        // In-band FEC: в каждый пакет добавляется сжатая копия предыдущего кадра,
        // и одиночную потерю получатель может восстановить по следующему пакету.
        encoder.set_inband_fec(true)?;
        encoder.set_packet_loss_perc(10)?;

        let state = Arc::new(TxState::default());
        let thread_state = state.clone();
        let thread = thread::Builder::new().name("vwf-encoder".into()).spawn(move || {
            let mut vad = Vad { threshold_db: settings.vad_threshold_db, hangover_left: 0 };
            let mut frame = [0f32; FRAME_SAMPLES];
            let mut packet = [0u8; MAX_PACKET];
            let mut next_seq = 0u32;
            wakeup.register();
            // Пока запускались вывод и микшер, микрофон уже что-то записал.
            // Это старый звук: отправлять его — значит начать разговор с задержкой.
            capture.clear();
            while !thread_state.stop.load(Ordering::Relaxed) {
                if capture.occupied_len() < FRAME_SAMPLES {
                    wakeup.wait(WAKE_TIMEOUT);
                    continue;
                }
                capture.pop_slice(&mut frame);
                let seq = next_seq;
                next_seq = next_seq.wrapping_add(1);

                let level = level_db(&frame);
                thread_state.level_db_bits.store(level.to_bits(), Ordering::Relaxed);
                let active = !thread_state.muted.load(Ordering::Relaxed) && vad.is_active(level);
                thread_state.transmitting.store(active, Ordering::Relaxed);
                if !active {
                    continue;
                }

                match encoder.encode_float(&frame, &mut packet) {
                    Ok(len) => send(seq, &packet[..len]),
                    Err(e) => eprintln!("ошибка кодирования Opus: {e}"),
                }
            }
        })?;
        Ok(Self { state, thread: Some(thread) })
    }
}

impl Drop for Transmitter {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Приём и микширование
// ---------------------------------------------------------------------------

/// Длительность плавного нарастания звука в начале фразы.
const FADE_IN_SAMPLES: usize = (SAMPLE_RATE / 1000 * 5) as usize; // 5 мс

/// Всё, что относится к одному собеседнику: свой джиттер-буфер и свой декодер
/// (декодер Opus хранит состояние между кадрами, его нельзя делить между людьми).
struct Speaker {
    jitter: JitterBuffer,
    decoder: opus::Decoder,
    volume: f32,
    /// Играл ли звук этого собеседника в прошлом кадре.
    active: bool,
}

impl Speaker {
    /// Декодирует следующий кадр в `out`. `false` — человек молчит.
    ///
    /// Резкий переход между тишиной и звуком слышен как щелчок, поэтому начало
    /// фразы плавно нарастает, а в конце фразы декодер «договаривает» один кадр
    /// (PLC), который плавно затухает до нуля.
    fn next_frame(&mut self, out: &mut [f32; FRAME_SAMPLES], now: Instant) -> bool {
        let result = match self.jitter.pop(now) {
            Pop::Idle => {
                if !std::mem::take(&mut self.active) {
                    return false;
                }
                if self.decoder.decode_float(&[], out, false).is_err() {
                    return false;
                }
                let n = out.len() as f32;
                for (i, s) in out.iter_mut().enumerate() {
                    *s *= 1.0 - i as f32 / n;
                }
                return true;
            }
            Pop::Packet(p) => self.decoder.decode_float(&p, out, false),
            // Восстанавливаем потерянный кадр из FEC-копии в следующем пакете...
            Pop::Lost { next: Some(next) } => self.decoder.decode_float(&next, out, true),
            // ...или, если его нет, просим декодер «додумать» звук (packet loss concealment).
            Pop::Lost { next: None } => self.decoder.decode_float(&[], out, false),
        };
        if let Err(e) = result {
            eprintln!("ошибка декодирования Opus: {e}");
            return false;
        }
        if !std::mem::replace(&mut self.active, true) {
            for (i, s) in out.iter_mut().take(FADE_IN_SAMPLES).enumerate() {
                *s *= i as f32 / FADE_IN_SAMPLES as f32;
            }
        }
        true
    }
}

/// Принимает пакеты от всех собеседников и сводит их в один поток.
pub struct Mixer {
    speakers: Mutex<HashMap<PeerId, Speaker>>,
    delay: Delay,
}

impl Mixer {
    /// `delay` — как джиттер-буферы выбирают задержку.
    pub fn new(delay: Delay) -> Self {
        Self { speakers: Mutex::new(HashMap::new()), delay }
    }

    /// Кладёт пришедший голосовой пакет в джиттер-буфер отправителя.
    pub fn push_packet(&self, from: PeerId, seq: u32, opus: &[u8]) {
        self.push_packet_at(from, seq, opus, Instant::now());
    }

    fn push_packet_at(&self, from: PeerId, seq: u32, opus: &[u8], now: Instant) {
        let mut speakers = self.speakers.lock().unwrap();
        let speaker = match speakers.entry(from) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                match opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono) {
                    Ok(decoder) => e.insert(Speaker {
                        jitter: JitterBuffer::new(self.delay, now),
                        decoder,
                        volume: 1.0,
                        active: false,
                    }),
                    Err(err) => {
                        eprintln!("не удалось создать декодер Opus: {err}");
                        return;
                    }
                }
            }
        };
        speaker.jitter.push(seq, opus.to_vec(), now);
    }

    /// Собеседник ушёл — забываем его состояние.
    pub fn remove_speaker(&self, id: PeerId) {
        self.speakers.lock().unwrap().remove(&id);
    }

    /// Громкость конкретного собеседника (1.0 — как есть).
    pub fn set_volume(&self, id: PeerId, volume: f32) {
        if let Some(s) = self.speakers.lock().unwrap().get_mut(&id) {
            s.volume = volume;
        }
    }

    /// Текущая задержка джиттер-буфера каждого собеседника, мс.
    pub fn delays_ms(&self) -> Vec<(PeerId, f64)> {
        let speakers = self.speakers.lock().unwrap();
        speakers.iter().map(|(id, s)| (*id, s.jitter.delay_ms())).collect()
    }

    /// Сводит по одному кадру от каждого собеседника.
    pub fn mix_frame(&self, out: &mut [f32; FRAME_SAMPLES]) {
        self.mix_frame_at(out, Instant::now());
    }

    fn mix_frame_at(&self, out: &mut [f32; FRAME_SAMPLES], now: Instant) {
        out.fill(0.0);
        let mut voice = [0f32; FRAME_SAMPLES];
        let mut speakers = self.speakers.lock().unwrap();
        for speaker in speakers.values_mut() {
            if speaker.next_frame(&mut voice, now) {
                for (o, v) in out.iter_mut().zip(voice.iter()) {
                    *o += v * speaker.volume;
                }
            }
        }
        // Если несколько человек кричат одновременно, сумма может выйти за [-1, 1].
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
    }

    /// Запускает поток, который поддерживает буфер воспроизведения заполненным.
    ///
    /// Темп задаёт сама звуковая карта: новый кадр микшируется, только когда
    /// она «съела» достаточно звука. Поэтому часы микшера никогда не расходятся
    /// с часами карты, а расхождение с часами собеседника гасит джиттер-буфер.
    ///
    /// `lead_ms` — сколько звука держать готовым для карты. Меньше — меньше
    /// задержка, но если поток микшера не успеет проснуться, будет щелчок
    /// (его покажет счётчик сбоев вывода). `wakeup` — тот же, что передан
    /// в `audio::start_playback`.
    pub fn spawn_output(
        self: &Arc<Self>,
        mut playback: HeapProd<f32>,
        device_rate: u32,
        wakeup: Arc<Wakeup>,
        lead_ms: u32,
    ) -> anyhow::Result<MixerOutput> {
        let mixer = self.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let lead = (device_rate * lead_ms / 1000) as usize;
        let thread = thread::Builder::new().name("vwf-mixer".into()).spawn(move || {
            let mut resampler = Resampler::new(SAMPLE_RATE, device_rate);
            let mut frame = [0f32; FRAME_SAMPLES];
            wakeup.register();
            while !thread_stop.load(Ordering::Relaxed) {
                while playback.occupied_len() < lead {
                    mixer.mix_frame(&mut frame);
                    for &s in &frame {
                        resampler.push(s, |x| {
                            let _ = playback.try_push(x);
                        });
                    }
                }
                wakeup.wait(WAKE_TIMEOUT);
            }
        })?;
        Ok(MixerOutput { stop, thread: Some(thread) })
    }
}

/// Работающий поток микшера; останавливается при удалении.
pub struct MixerOutput {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for MixerOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_frame(start: usize) -> [f32; FRAME_SAMPLES] {
        std::array::from_fn(|i| {
            let t = (start + i) as f32 / SAMPLE_RATE as f32;
            0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
        })
    }

    #[test]
    fn vad_holds_after_speech() {
        let mut vad = Vad { threshold_db: Some(-40.0), hangover_left: 0 };
        assert!(!vad.is_active(-60.0));
        assert!(vad.is_active(-20.0));
        for _ in 0..HANGOVER_FRAMES {
            assert!(vad.is_active(-60.0));
        }
        assert!(!vad.is_active(-60.0));
    }

    #[test]
    fn level_of_full_scale_sine_is_about_minus_3_db() {
        let frame: Vec<f32> = sine_frame(0).iter().map(|s| s * 2.0).collect();
        assert!((level_db(&frame) + 3.0).abs() < 0.2);
    }

    /// Кодируем синус, теряем один пакет и проверяем, что на выходе микшера
    /// звук есть везде — потеря закрыта FEC/PLC, а не тишиной.
    #[test]
    fn opus_through_mixer_survives_packet_loss() {
        let mut encoder =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        encoder.set_inband_fec(true).unwrap();
        encoder.set_packet_loss_perc(10).unwrap();

        // Пакеты приходят по одному на каждый 20-мс кадр воспроизведения,
        // как в реальной жизни (время виртуальное); пакет 10 теряется.
        let mixer = Mixer::new(Delay::Fixed(40.0));
        let start = Instant::now();
        let at = |frame: u32| start + Duration::from_millis(frame as u64 * FRAME_MS as u64);
        let mut out = [0f32; FRAME_SAMPLES];
        for seq in 0..30u32 {
            let packet = encoder
                .encode_vec_float(&sine_frame(seq as usize * FRAME_SAMPLES), MAX_PACKET)
                .unwrap();
            if seq != 10 {
                mixer.push_packet_at(1, seq, &packet, at(seq));
            }
            mixer.mix_frame_at(&mut out, at(seq));
            let level = level_db(&out);
            match seq {
                // Задержка 40 мс — первые два кадра играет тишина.
                0 | 1 => assert_eq!(level, -120.0, "кадр {seq}"),
                _ => assert!(level > -20.0, "кадр {seq}: {level} dBFS"),
            }
        }
        // Буфер доигрывает остаток и затухающий кадр, потом снова тишина.
        for frame in 30..33 {
            mixer.mix_frame_at(&mut out, at(frame));
            assert!(level_db(&out) > -40.0);
        }
        mixer.mix_frame_at(&mut out, at(33));
        assert!(out.iter().all(|s| *s == 0.0), "после конца речи — тишина");
    }
}

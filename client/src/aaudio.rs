//! Звук на Android: напрямую через AAudio, в режиме «разговор».
//!
//! cpal открывает микрофон как для распознавания речи, без обработки. Нам же
//! нужен режим звонка (`VOICE_COMMUNICATION`): тогда телефон сам вычитает из
//! микрофона то, что играет его динамик (эхоподавление), и приглушает шум, —
//! как в обычном звонке или мессенджере. Звук собеседников при этом идёт как
//! голос в разговоре: куда именно (громкий динамик, динамик у уха,
//! гарнитура), решает система (см. `MainActivity.kt`).
//!
//! Микрофон и динамик здесь всегда системные: устройство выбирает Android,
//! поэтому длинного списка «микрофонов» и «динамиков» на телефоне нет.
//!
//! Когда система переключает звук (вставили наушники, подключилась
//! Bluetooth-гарнитура), AAudio закрывает поток с ошибкой «отключён». Тогда
//! поток открывается заново в отдельном потоке — звонок не прерывается.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::Context;
use ndk::audio::{
    AudioCallbackResult, AudioContentType, AudioDirection, AudioError, AudioFormat,
    AudioInputPreset, AudioPerformanceMode, AudioSharingMode, AudioStream, AudioStreamBuilder,
    AudioUsage,
};
use vwf_proto::SAMPLE_RATE;

/// Обработчик порции звука (моно). Для микрофона — читает сэмплы, для
/// динамика — заполняет их. Вызывается в потоке реального времени: без
/// блокировок и выделения памяти.
pub(crate) type Process = Box<dyn FnMut(&mut [f32]) + Send>;

/// Самая большая порция за один вызов, в кадрах. AAudio обычно отдаёт по
/// 2–20 мс; больше — обрезаем.
const MAX_FRAMES: usize = 16_384;

/// Открытый поток AAudio. Пока значение живо — звук идёт.
pub(crate) struct Stream {
    shared: Arc<Shared>,
}

struct Shared {
    input: bool,
    /// Частота и число каналов, которые дала система при первом открытии.
    sample_rate: AtomicU32,
    channels: AtomicU32,
    process: Mutex<Option<Process>>,
    stream: Mutex<Option<SendStream>>,
    closed: AtomicBool,
    reopening: AtomicBool,
}

/// AAudio разрешает запускать, останавливать и закрывать поток из любого
/// потока (кроме его собственных колбэков).
struct SendStream(AudioStream);
// SAFETY: см. выше; сам указатель на поток не меняется.
unsafe impl Send for SendStream {}

impl Stream {
    /// Открывает системный микрофон (`input`) или вывод. Звук не идёт, пока
    /// не вызван [`Stream::start`].
    pub(crate) fn open(input: bool) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            input,
            sample_rate: AtomicU32::new(SAMPLE_RATE),
            channels: AtomicU32::new(1),
            process: Mutex::new(None),
            stream: Mutex::new(None),
            closed: AtomicBool::new(false),
            reopening: AtomicBool::new(false),
        });
        let stream = open_stream(&shared).with_context(|| {
            if input {
                "не удалось открыть микрофон"
            } else {
                "не удалось открыть динамик"
            }
        })?;
        // Частоту и число каналов система может выбрать свои; при повторном
        // открытии просим уже их.
        shared.sample_rate.store(stream.sample_rate().max(1) as u32, Ordering::Relaxed);
        shared.channels.store(stream.channel_count().max(1) as u32, Ordering::Relaxed);
        *shared.stream.lock().unwrap() = Some(SendStream(stream));
        Ok(Self { shared })
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.shared.sample_rate.load(Ordering::Relaxed)
    }

    pub(crate) fn channels(&self) -> u16 {
        self.shared.channels.load(Ordering::Relaxed) as u16
    }

    /// Запускает звук: `process` получает (микрофон) или заполняет (динамик)
    /// моно-сэмплы на частоте [`Stream::sample_rate`].
    pub(crate) fn start(&self, process: Process) -> anyhow::Result<()> {
        *self.shared.process.lock().unwrap() = Some(process);
        if let Some(stream) = self.shared.stream.lock().unwrap().as_ref() {
            stream.0.request_start().context("не удалось запустить звук")?;
        }
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some(stream) = self.shared.stream.lock().unwrap().take() {
            let _ = stream.0.request_stop();
        }
    }
}

fn open_stream(shared: &Arc<Shared>) -> Result<AudioStream, AudioError> {
    let mut builder = AudioStreamBuilder::new()?
        .direction(if shared.input { AudioDirection::Input } else { AudioDirection::Output })
        .sharing_mode(AudioSharingMode::Shared)
        .sample_rate(shared.sample_rate.load(Ordering::Relaxed) as i32)
        .channel_count(shared.channels.load(Ordering::Relaxed) as i32)
        .format(AudioFormat::PCM_Float);
    builder = if shared.input {
        // Без «быстрого» пути: на нём телефон не включает эхоподавление.
        builder
            .input_preset(AudioInputPreset::VoiceCommunication)
            .performance_mode(AudioPerformanceMode::None)
    } else {
        builder
            .usage(AudioUsage::VoiceCommunication)
            .content_type(AudioContentType::Speech)
            .performance_mode(AudioPerformanceMode::LowLatency)
    };

    let input = shared.input;
    let weak = Arc::downgrade(shared);
    let mut scratch = vec![0.0f32; MAX_FRAMES];
    let data = move |stream: &AudioStream, data: *mut c_void, frames: i32| {
        let Some(shared) = weak.upgrade() else {
            return AudioCallbackResult::Stop;
        };
        let channels = stream.channel_count().max(1) as usize;
        let frames = (frames.max(0) as usize).min(MAX_FRAMES);
        let mono = &mut scratch[..frames];
        let len = frames * channels;
        // SAFETY: AAudio передаёт буфер ровно на `frames` кадров в формате потока.
        match stream.format() {
            AudioFormat::PCM_Float => {
                let data = unsafe { std::slice::from_raw_parts_mut(data as *mut f32, len) };
                run(&shared, input, mono, channels, data, |s| s, |s| s);
            }
            AudioFormat::PCM_I16 => {
                let data = unsafe { std::slice::from_raw_parts_mut(data as *mut i16, len) };
                run(
                    &shared,
                    input,
                    mono,
                    channels,
                    data,
                    |s| s as f32 / 32768.0,
                    |s| (s.clamp(-1.0, 1.0) * 32767.0) as i16,
                );
            }
            _ => {}
        }
        AudioCallbackResult::Continue
    };

    let weak = Arc::downgrade(shared);
    let error = move |_: &AudioStream, err: AudioError| {
        if err != AudioError::Disconnected {
            eprintln!("звук: ошибка AAudio: {err:?}");
        }
        // Закрывать поток из его же колбэка нельзя: открываем заново в
        // отдельном потоке.
        reopen(weak.clone());
    };

    builder.data_callback(Box::new(data)).error_callback(Box::new(error)).open_stream()
}

/// Перекладывает звук между буфером AAudio (`data`, `channels` каналов) и
/// обработчиком (моно, `mono`).
fn run<T: Copy>(
    shared: &Shared,
    input: bool,
    mono: &mut [f32],
    channels: usize,
    data: &mut [T],
    to_f32: impl Fn(T) -> f32,
    from_f32: impl Fn(f32) -> T,
) {
    // Не ждём: обработчик занят только в момент замены (см. `start`).
    let Ok(mut guard) = shared.process.try_lock() else {
        if !input {
            data.fill(from_f32(0.0));
        }
        return;
    };
    let Some(process) = guard.as_mut() else {
        if !input {
            data.fill(from_f32(0.0));
        }
        return;
    };
    if input {
        for (out, frame) in mono.iter_mut().zip(data.chunks_exact(channels)) {
            *out = frame.iter().map(|&s| to_f32(s)).sum::<f32>() / channels as f32;
        }
        process(mono);
    } else {
        process(mono);
        for (frame, &sample) in data.chunks_exact_mut(channels).zip(mono.iter()) {
            frame.fill(from_f32(sample));
        }
    }
}

/// Открывает поток заново после отключения (сменилось устройство).
fn reopen(weak: Weak<Shared>) {
    let Some(shared) = weak.upgrade() else { return };
    if shared.closed.load(Ordering::SeqCst) || shared.reopening.swap(true, Ordering::SeqCst) {
        return;
    }
    drop(shared);
    std::thread::spawn(move || {
        for attempt in 1..=20u64 {
            std::thread::sleep(Duration::from_millis(150 * attempt.min(6)));
            let Some(shared) = weak.upgrade() else { return };
            if shared.closed.load(Ordering::SeqCst) {
                return;
            }
            let mut slot = shared.stream.lock().unwrap();
            if let Some(old) = slot.take() {
                let _ = old.0.request_stop();
            }
            match open_stream(&shared).and_then(|stream| {
                stream.request_start()?;
                Ok(stream)
            }) {
                Ok(stream) => {
                    let rate = shared.sample_rate.load(Ordering::Relaxed);
                    if stream.sample_rate() as u32 != rate {
                        eprintln!(
                            "звук: после переключения частота {} вместо {rate}",
                            stream.sample_rate()
                        );
                    }
                    *slot = Some(SendStream(stream));
                    shared.reopening.store(false, Ordering::SeqCst);
                    return;
                }
                Err(e) => eprintln!("звук: не удалось открыть заново (попытка {attempt}): {e:?}"),
            }
        }
        if let Some(shared) = weak.upgrade() {
            shared.reopening.store(false, Ordering::SeqCst);
        }
    });
}

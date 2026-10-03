//! Работа со звуковой картой через `cpal` (на Windows это WASAPI).
//!
//! Аудиоколбэки вызываются системой в отдельном потоке реального времени: в них
//! нельзя ждать блокировок, выделять память и делать что-то долгое, иначе
//! будут щелчки и пропуски. Поэтому колбэки только перекладывают сэмплы
//! в lock-free кольцевые буферы, а кодирование и микширование происходят
//! в обычных потоках (см. `pipeline`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::Thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use vwf_proto::SAMPLE_RATE;

use crate::resample::Resampler;

/// Сигнал «в кольцевом буфере что-то изменилось» от аудиоколбэка рабочему потоку.
///
/// Вместо того чтобы проверять буфер по таймеру (и терять в среднем половину
/// периода опроса), поток спит, пока колбэк его не разбудит. Будить поток
/// из аудиоколбэка безопасно: `unpark` не блокируется и не выделяет память.
#[derive(Default)]
pub struct Wakeup {
    thread: OnceLock<Thread>,
}

impl Wakeup {
    /// Вызывается один раз из потока, который будет ждать сигналов.
    pub fn register(&self) {
        let _ = self.thread.set(std::thread::current());
    }

    pub fn wake(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }

    /// Ждёт сигнала, но не дольше `timeout`.
    pub fn wait(&self, timeout: Duration) {
        std::thread::park_timeout(timeout);
    }
}

/// Имена устройств для выбора в интерфейсе.
#[derive(Debug, Clone, Default)]
pub struct DeviceList {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub default_input: Option<String>,
    pub default_output: Option<String>,
}

pub fn device_list() -> anyhow::Result<DeviceList> {
    let host = cpal::default_host();
    Ok(DeviceList {
        inputs: host.input_devices()?.map(|d| device_name(&d)).collect(),
        outputs: host.output_devices()?.map(|d| device_name(&d)).collect(),
        default_input: host.default_input_device().map(|d| device_name(&d)),
        default_output: host.default_output_device().map(|d| device_name(&d)),
    })
}

/// Печатает все устройства ввода и вывода.
pub fn list_devices() -> anyhow::Result<()> {
    let list = device_list()?;
    println!("Микрофоны:");
    for name in &list.inputs {
        let mark = if Some(name) == list.default_input.as_ref() {
            " (по умолчанию)"
        } else {
            ""
        };
        println!("  {name}{mark}");
    }
    println!("Динамики/наушники:");
    for name in &list.outputs {
        let mark = if Some(name) == list.default_output.as_ref() {
            " (по умолчанию)"
        } else {
            ""
        };
        println!("  {name}{mark}");
    }
    Ok(())
}

/// Имя устройства, как его показывает Windows: «Микрофон (USB Audio Device)».
/// Полное описание cpal дописывает к нему тип и подключение
/// («[Microphone] via USB») — это лишнее.
fn device_name(device: &Device) -> String {
    device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "<без имени>".into())
}

/// Ищет устройство по точному имени, затем по части имени (без учёта регистра),
/// или берёт устройство по умолчанию.
fn find_device(input: bool, name: Option<&str>) -> anyhow::Result<Device> {
    let host = cpal::default_host();
    let Some(pattern) = name else {
        let device = if input { host.default_input_device() } else { host.default_output_device() };
        return device.context(if input {
            "в системе нет микрофона по умолчанию"
        } else {
            "в системе нет устройства вывода по умолчанию"
        });
    };
    let devices: Vec<(String, Device)> = if input {
        host.input_devices()?.map(|d| (device_name(&d), d)).collect()
    } else {
        host.output_devices()?.map(|d| (device_name(&d), d)).collect()
    };
    let lower = pattern.to_lowercase();
    let index = devices
        .iter()
        .position(|(name, _)| name == pattern)
        .or_else(|| devices.iter().position(|(name, _)| name.to_lowercase().contains(&lower)))
        // Имя, сохранённое прежними версиями: «Имя [Microphone] via USB».
        .or_else(|| devices.iter().position(|(name, _)| pattern.starts_with(&format!("{name} "))))
        .with_context(|| format!("не найдено устройство «{pattern}» (см. команду `devices`)"))?;
    Ok(devices.into_iter().nth(index).expect("индекс из того же списка").1)
}

/// Предпочитаем 48 кГц, если устройство их умеет, — тогда не нужен ресемплинг.
fn pick_config(device: &Device, input: bool) -> anyhow::Result<cpal::SupportedStreamConfig> {
    let default =
        if input { device.default_input_config()? } else { device.default_output_config()? };
    if default.sample_rate() == SAMPLE_RATE {
        return Ok(default);
    }
    let ranges: Vec<cpal::SupportedStreamConfigRange> = if input {
        device.supported_input_configs()?.collect()
    } else {
        device.supported_output_configs()?.collect()
    };
    Ok(ranges
        .into_iter()
        .filter(|r| {
            r.channels() == default.channels() && r.sample_format() == default.sample_format()
        })
        .find_map(|r| r.try_with_sample_rate(SAMPLE_RATE))
        .unwrap_or(default))
}

/// Обработчик ошибок аудиопотока.
///
/// Отдельно считается xrun: система не успела передать нам данные с микрофона,
/// и кусочек звука потерялся (на слух — щелчок). Это не авария, поток продолжает
/// работать, поэтому здесь только увеличиваем счётчик, а сообщает о сбоях
/// основной поток (см. `GlitchCounter`). Печатать из аудиопотока нельзя:
/// вывод в консоль может заблокироваться и сам вызвать новые сбои.
///
/// Первую секунду после запуска xrun'ы не считаем: WASAPI часто выдаёт их,
/// пока аудиодвижок Windows раскручивается.
fn stream_error_handler(
    label: &'static str,
    xruns: Arc<AtomicU64>,
) -> impl FnMut(cpal::Error) + Send + 'static {
    let started = Instant::now();
    move |err| {
        if err.kind() != cpal::ErrorKind::Xrun {
            eprintln!("{label}: ошибка аудиопотока: {err}");
        } else if started.elapsed() >= Duration::from_secs(1) {
            xruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Запущенный захват с микрофона. Пока значение живо — микрофон пишет.
pub struct Capture {
    _stream: cpal::Stream,
    pub device_name: String,
    pub config: StreamConfig,
    /// Сколько раз система теряла кусочки звука с микрофона.
    pub glitches: Arc<AtomicU64>,
}

/// Начинает захват. Возвращает поток и потребителя кольцевого буфера,
/// в котором появляются моно-сэмплы f32 на частоте 48 кГц.
/// После каждой порции звука колбэк будит `wakeup`.
pub fn start_capture(
    name: Option<&str>,
    wakeup: Arc<Wakeup>,
) -> anyhow::Result<(Capture, HeapCons<f32>)> {
    let device = find_device(true, name)?;
    let supported = pick_config(&device, true)?;
    #[allow(unused_mut)]
    let mut config = supported.config();
    // В WASAPI размер буфера захвата по умолчанию — около 20 мс. Если поток cpal
    // хоть раз опоздает сильнее (драйверы, энергосбережение), Windows выбросит
    // данные, и будет щелчок. Буфер на 100 мс даёт запас. Задержку это
    // не увеличивает: cpal забирает всё, что накопилось, при каждом пробуждении.
    #[cfg(target_os = "windows")]
    {
        config.buffer_size = cpal::BufferSize::Fixed(config.sample_rate / 10);
    }
    let glitches = Arc::new(AtomicU64::new(0));
    // Секунда запаса: если поток кодирования ненадолго подвиснет, звук не потеряется.
    let (producer, consumer) = HeapRb::<f32>::new(SAMPLE_RATE as usize).split();

    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            build_input::<f32>(&device, config, producer, &glitches, wakeup.clone())
        }
        SampleFormat::I16 => {
            build_input::<i16>(&device, config, producer, &glitches, wakeup.clone())
        }
        SampleFormat::I32 => {
            build_input::<i32>(&device, config, producer, &glitches, wakeup.clone())
        }
        SampleFormat::U16 => {
            build_input::<u16>(&device, config, producer, &glitches, wakeup.clone())
        }
        SampleFormat::U8 => build_input::<u8>(&device, config, producer, &glitches, wakeup.clone()),
        SampleFormat::F64 => {
            build_input::<f64>(&device, config, producer, &glitches, wakeup.clone())
        }
        other => bail!("формат сэмплов микрофона {other} не поддерживается"),
    }?;
    stream.play()?;
    Ok((Capture { _stream: stream, device_name: device_name(&device), config, glitches }, consumer))
}

fn build_input<T>(
    device: &Device,
    config: StreamConfig,
    mut producer: HeapProd<f32>,
    glitches: &Arc<AtomicU64>,
    wakeup: Arc<Wakeup>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let mut resampler = Resampler::new(config.sample_rate, SAMPLE_RATE);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            for frame in data.chunks_exact(channels) {
                // Сводим все каналы микрофона в моно.
                let mono =
                    frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32;
                resampler.push(mono, |s| {
                    // Буфер переполнен — значит, его никто не читает; просто теряем звук.
                    let _ = producer.try_push(s);
                });
            }
            wakeup.wake();
        },
        stream_error_handler("микрофон", glitches.clone()),
        None,
    )?;
    Ok(stream)
}

/// Запущенное воспроизведение. Пока значение живо — звук играет.
pub struct Playback {
    _stream: cpal::Stream,
    pub device_name: String,
    pub config: StreamConfig,
    /// Сколько раз воспроизведению не хватило звука (или система сообщила о сбое).
    pub glitches: Arc<AtomicU64>,
}

/// Начинает воспроизведение. Возвращает поток и производителя кольцевого буфера:
/// в него надо класть моно-сэмплы f32 на частоте **устройства** (`config.sample_rate`).
/// Если данных не хватает, играет тишина.
/// После каждой порции звука колбэк будит `wakeup`, чтобы микшер долил новую.
pub fn start_playback(
    name: Option<&str>,
    wakeup: Arc<Wakeup>,
) -> anyhow::Result<(Playback, HeapProd<f32>)> {
    let device = find_device(false, name)?;
    let supported = pick_config(&device, false)?;
    let config = supported.config();
    let (producer, consumer) = HeapRb::<f32>::new(config.sample_rate as usize / 2).split();
    let glitches = Arc::new(AtomicU64::new(0));

    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            build_output::<f32>(&device, config, consumer, &glitches, wakeup.clone())
        }
        SampleFormat::I16 => {
            build_output::<i16>(&device, config, consumer, &glitches, wakeup.clone())
        }
        SampleFormat::I32 => {
            build_output::<i32>(&device, config, consumer, &glitches, wakeup.clone())
        }
        SampleFormat::U16 => {
            build_output::<u16>(&device, config, consumer, &glitches, wakeup.clone())
        }
        SampleFormat::U8 => {
            build_output::<u8>(&device, config, consumer, &glitches, wakeup.clone())
        }
        SampleFormat::F64 => {
            build_output::<f64>(&device, config, consumer, &glitches, wakeup.clone())
        }
        other => bail!("формат сэмплов устройства вывода {other} не поддерживается"),
    }?;
    stream.play()?;
    Ok((
        Playback { _stream: stream, device_name: device_name(&device), config, glitches },
        producer,
    ))
}

fn build_output<T>(
    device: &Device,
    config: StreamConfig,
    mut consumer: HeapCons<f32>,
    glitches: &Arc<AtomicU64>,
    wakeup: Arc<Wakeup>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let underruns = glitches.clone();
    // Пока микшер не положил в буфер первые данные, пустой буфер — норма.
    let mut primed = false;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut starved = false;
            for frame in data.chunks_exact_mut(channels) {
                let sample = match consumer.try_pop() {
                    Some(s) => {
                        primed = true;
                        s
                    }
                    None => {
                        starved = true;
                        0.0
                    }
                };
                // Моно-голос одинаково во все каналы.
                frame.fill(T::from_sample(sample));
            }
            // Микшер держит буфер заполненным всегда, даже тишиной, так что
            // нехватка данных означает, что он не успел, — это слышно как щелчок.
            if starved && primed {
                underruns.fetch_add(1, Ordering::Relaxed);
            }
            wakeup.wake();
        },
        stream_error_handler("вывод", glitches.clone()),
        None,
    )?;
    Ok(stream)
}

/// Следит за счётчиком сбоев и сообщает о новых, не чаще раза в несколько секунд.
pub struct GlitchCounter {
    label: &'static str,
    counter: Arc<AtomicU64>,
    reported: u64,
}

impl GlitchCounter {
    pub fn new(label: &'static str, counter: Arc<AtomicU64>) -> Self {
        Self { label, counter, reported: 0 }
    }

    /// Текст сообщения, если с прошлого раза появились новые сбои.
    pub fn take_report(&mut self) -> Option<String> {
        let total = self.counter.load(Ordering::Relaxed);
        if total == self.reported {
            return None;
        }
        let new = total - self.reported;
        self.reported = total;
        Some(format!("{}: сбоев звука: {new} (всего {total})", self.label))
    }
}

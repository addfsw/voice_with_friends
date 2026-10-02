//! Работа со звуковой картой через `cpal` (на Windows это WASAPI).
//!
//! Аудиоколбэки вызываются системой в отдельном потоке реального времени: в них
//! нельзя ждать блокировок, выделять память и делать что-то долгое, иначе
//! будут щелчки и пропуски. Поэтому колбэки только перекладывают сэмплы
//! в lock-free кольцевые буферы, а кодирование и микширование происходят
//! в обычных потоках (см. `pipeline`).

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use vwf_proto::SAMPLE_RATE;

use crate::resample::Resampler;

/// Печатает все устройства ввода и вывода.
pub fn list_devices() -> anyhow::Result<()> {
    let host = cpal::default_host();
    let default_in = host.default_input_device().map(|d| device_name(&d));
    let default_out = host.default_output_device().map(|d| device_name(&d));

    println!("Микрофоны:");
    for d in host.input_devices()? {
        let name = device_name(&d);
        let mark =
            if Some(&name) == default_in.as_ref() { " (по умолчанию)" } else { "" };
        println!("  {name}{mark}");
    }
    println!("Динамики/наушники:");
    for d in host.output_devices()? {
        let name = device_name(&d);
        let mark =
            if Some(&name) == default_out.as_ref() { " (по умолчанию)" } else { "" };
        println!("  {name}{mark}");
    }
    Ok(())
}

fn device_name(device: &Device) -> String {
    device.description().map(|d| d.to_string()).unwrap_or_else(|_| "<без имени>".into())
}

/// Ищет устройство по части имени (без учёта регистра) или берёт устройство по умолчанию.
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
    let pattern = pattern.to_lowercase();
    let devices: Vec<Device> =
        if input { host.input_devices()?.collect() } else { host.output_devices()?.collect() };
    devices
        .into_iter()
        .find(|d| device_name(d).to_lowercase().contains(&pattern))
        .with_context(|| format!("не найдено устройство «{pattern}» (см. команду `devices`)"))
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
/// Отдельно обрабатывается xrun: система не успела передать нам данные с микрофона
/// (или забрать данные для вывода), и кусочек звука потерялся. Это не авария:
/// поток продолжает работать, а на слух это в худшем случае короткий щелчок.
/// Первую секунду после запуска xrun'ы не показываем: WASAPI часто выдаёт их,
/// пока аудиодвижок Windows раскручивается. Дальше сообщаем не чаще раза в 5 секунд,
/// чтобы не засорять консоль.
fn stream_error_handler(label: &'static str) -> impl FnMut(cpal::Error) + Send + 'static {
    let started = Instant::now();
    let mut xruns = 0u64;
    let mut last_report: Option<Instant> = None;
    move |err| {
        if err.kind() != cpal::ErrorKind::Xrun {
            eprintln!("{label}: ошибка аудиопотока: {err}");
            return;
        }
        if started.elapsed() < Duration::from_secs(1) {
            return;
        }
        xruns += 1;
        if last_report.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
            eprintln!(
                "{label}: потерян кусочек звука, система не успела его передать (всего {xruns})"
            );
            last_report = Some(Instant::now());
        }
    }
}

/// Запущенный захват с микрофона. Пока значение живо — микрофон пишет.
pub struct Capture {
    _stream: cpal::Stream,
    pub device_name: String,
    pub config: StreamConfig,
}

/// Начинает захват. Возвращает поток и потребителя кольцевого буфера,
/// в котором появляются моно-сэмплы f32 на частоте 48 кГц.
pub fn start_capture(name: Option<&str>) -> anyhow::Result<(Capture, HeapCons<f32>)> {
    let device = find_device(true, name)?;
    let supported = pick_config(&device, true)?;
    let config = supported.config();
    // Секунда запаса: если поток кодирования ненадолго подвиснет, звук не потеряется.
    let (producer, consumer) = HeapRb::<f32>::new(SAMPLE_RATE as usize).split();

    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_input::<f32>(&device, config, producer),
        SampleFormat::I16 => build_input::<i16>(&device, config, producer),
        SampleFormat::I32 => build_input::<i32>(&device, config, producer),
        SampleFormat::U16 => build_input::<u16>(&device, config, producer),
        SampleFormat::U8 => build_input::<u8>(&device, config, producer),
        SampleFormat::F64 => build_input::<f64>(&device, config, producer),
        other => bail!("формат сэмплов микрофона {other} не поддерживается"),
    }?;
    stream.play()?;
    Ok((Capture { _stream: stream, device_name: device_name(&device), config }, consumer))
}

fn build_input<T>(
    device: &Device,
    config: StreamConfig,
    mut producer: HeapProd<f32>,
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
        },
        stream_error_handler("микрофон"),
        None,
    )?;
    Ok(stream)
}

/// Запущенное воспроизведение. Пока значение живо — звук играет.
pub struct Playback {
    _stream: cpal::Stream,
    pub device_name: String,
    pub config: StreamConfig,
}

/// Начинает воспроизведение. Возвращает поток и производителя кольцевого буфера:
/// в него надо класть моно-сэмплы f32 на частоте **устройства** (`config.sample_rate`).
/// Если данных не хватает, играет тишина.
pub fn start_playback(name: Option<&str>) -> anyhow::Result<(Playback, HeapProd<f32>)> {
    let device = find_device(false, name)?;
    let supported = pick_config(&device, false)?;
    let config = supported.config();
    let (producer, consumer) = HeapRb::<f32>::new(config.sample_rate as usize / 2).split();

    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_output::<f32>(&device, config, consumer),
        SampleFormat::I16 => build_output::<i16>(&device, config, consumer),
        SampleFormat::I32 => build_output::<i32>(&device, config, consumer),
        SampleFormat::U16 => build_output::<u16>(&device, config, consumer),
        SampleFormat::U8 => build_output::<u8>(&device, config, consumer),
        SampleFormat::F64 => build_output::<f64>(&device, config, consumer),
        other => bail!("формат сэмплов устройства вывода {other} не поддерживается"),
    }?;
    stream.play()?;
    Ok((Playback { _stream: stream, device_name: device_name(&device), config }, producer))
}

fn build_output<T>(
    device: &Device,
    config: StreamConfig,
    mut consumer: HeapCons<f32>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for frame in data.chunks_exact_mut(channels) {
                let sample = T::from_sample(consumer.try_pop().unwrap_or(0.0));
                // Моно-голос одинаково во все каналы.
                frame.fill(sample);
            }
        },
        stream_error_handler("вывод"),
        None,
    )?;
    Ok(stream)
}

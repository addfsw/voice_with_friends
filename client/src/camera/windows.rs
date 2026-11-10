//! Веб-камера Windows через Media Foundation.
//!
//! Камера открывается в режиме, ближайшем к 1280×720 и 30 к/с, а кадры
//! Media Foundation сама переводит в NV12 (в том числе из MJPEG, в котором
//! многие камеры отдают 720p).

use std::time::Instant;

use anyhow::Context;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaType, IMFSourceReader, MF_API_VERSION,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SDK_VERSION, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
    MFCreateAttributes, MFCreateDeviceSource, MFCreateMediaType,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video,
    MFSTARTUP_NOSOCKET, MFShutdown, MFStartup, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{GUID, HSTRING, Interface, PWSTR};

use super::{CameraInfo, CameraSource, I420};

/// Какой режим камеры хотим: самый большой слой — 720p.
const WANT_HEIGHT: u32 = 720;
const WANT_FPS: f64 = 30.0;

/// Media Foundation и COM в текущем потоке. Ошибку «COM уже запущен в
/// другом режиме» не считаем: Media Foundation работает и так.
struct MfSession;

impl MfSession {
    fn start() -> anyhow::Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup((MF_SDK_VERSION << 16) | MF_API_VERSION, MFSTARTUP_NOSOCKET)
                .context("Media Foundation не запустилась")?;
        }
        Ok(Self)
    }
}

impl Drop for MfSession {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}

/// Все камеры: `(название, символьная ссылка — её и используем как id)`.
fn enumerate() -> anyhow::Result<Vec<(String, String)>> {
    let _mf = MfSession::start()?;
    unsafe {
        let attributes = video_capture_attributes()?;
        let mut devices: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attributes, &mut devices, &mut count)
            .context("не удалось получить список камер")?;
        let mut list = Vec::new();
        if !devices.is_null() {
            for device in std::slice::from_raw_parts_mut(devices, count as usize) {
                if let Some(activate) = device.take() {
                    let name = allocated_string(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME);
                    let link = allocated_string(
                        &activate,
                        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                    );
                    if let (Some(name), Some(link)) = (name, link) {
                        list.push((name, link));
                    }
                }
            }
            CoTaskMemFree(Some(devices.cast()));
        }
        Ok(list)
    }
}

unsafe fn video_capture_attributes() -> anyhow::Result<IMFAttributes> {
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 2)?;
        let attributes = attributes.context("пустые атрибуты")?;
        attributes.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;
        Ok(attributes)
    }
}

unsafe fn allocated_string(activate: &IMFActivate, key: &GUID) -> Option<String> {
    unsafe {
        let mut value = PWSTR::null();
        let mut len = 0u32;
        activate.GetAllocatedString(key, &mut value, &mut len).ok()?;
        let text = value.to_string().ok();
        CoTaskMemFree(Some(value.0.cast()));
        text
    }
}

pub fn cameras() -> anyhow::Result<Vec<CameraInfo>> {
    Ok(enumerate()?.into_iter().map(|(name, id)| CameraInfo { id, name }).collect())
}

/// Открывает камеру по символьной ссылке; пустой id — первая попавшаяся.
pub fn open(id: &str) -> anyhow::Result<Box<dyn CameraSource>> {
    let id = if id.is_empty() {
        enumerate()?.into_iter().next().map(|(_, id)| id).context("камера не найдена")?
    } else {
        id.to_string()
    };
    let mf = MfSession::start()?;
    unsafe {
        let attributes = video_capture_attributes()?;
        attributes.SetString(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
            &HSTRING::from(id.as_str()),
        )?;
        let source = MFCreateDeviceSource(&attributes)
            .context("камера не открылась: она отключена или занята другой программой")?;

        let mut reader_attributes = None;
        MFCreateAttributes(&mut reader_attributes, 1)?;
        let reader_attributes = reader_attributes.context("пустые атрибуты")?;
        // Перевод в NV12 (и распаковку MJPEG) делает сама Media Foundation.
        reader_attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromMediaSource(&source, &reader_attributes)
            .context("не удалось читать кадры камеры")?;

        let (width, height) = choose_mode(&reader)?;
        let output = MFCreateMediaType()?;
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        output.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(width) << 32) | u64::from(height))?;
        reader
            .SetCurrentMediaType(STREAM, None, &output)
            .context("камера не отдаёт кадры в нужном формате")?;
        Ok(Box::new(WindowsCamera { reader, width, height, _mf: mf }))
    }
}

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

/// Выбирает режим камеры: высота ближе всего к 720 (не больше), частота —
/// не меньше 25 к/с, если такая есть. Возвращает размер кадра.
unsafe fn choose_mode(reader: &IMFSourceReader) -> anyhow::Result<(u32, u32)> {
    unsafe {
        let mut best: Option<(i64, IMFMediaType, u32, u32)> = None;
        for index in 0.. {
            let Ok(native) = reader.GetNativeMediaType(STREAM, index) else { break };
            let Ok(size) = native.GetUINT64(&MF_MT_FRAME_SIZE) else { continue };
            let (w, h) = ((size >> 32) as u32, size as u32);
            let fps = native.GetUINT64(&MF_MT_FRAME_RATE).map_or(30.0, |rate| {
                let (num, den) = ((rate >> 32) as u32, rate as u32);
                if den == 0 { 0.0 } else { f64::from(num) / f64::from(den) }
            });
            // Меньше — лучше: сначала частота, потом близость высоты к 720
            // (больше 720 — хуже: лишняя работа на масштабирование).
            let fps_penalty = if fps + 0.5 >= 25.0 { 0 } else { 10_000 };
            let height_penalty = if h <= WANT_HEIGHT {
                i64::from(WANT_HEIGHT - h)
            } else {
                i64::from(h - WANT_HEIGHT) * 2
            };
            let fps_bonus = -((fps.min(WANT_FPS)) as i64);
            let score = fps_penalty + height_penalty * 10 + fps_bonus;
            if best.as_ref().is_none_or(|(s, ..)| score < *s) {
                best = Some((score, native, w, h));
            }
        }
        let (_, native, w, h) = best.context("камера не сообщает режимы съёмки")?;
        reader.SetCurrentMediaType(STREAM, None, &native).context("режим камеры не включился")?;
        Ok((w, h))
    }
}

struct WindowsCamera {
    reader: IMFSourceReader,
    width: u32,
    height: u32,
    _mf: MfSession,
}

impl CameraSource for WindowsCamera {
    fn frame(&mut self, _deadline: Instant) -> anyhow::Result<Option<I420>> {
        // Чтение синхронное: камера сама отдаёт кадры со своей частотой.
        unsafe {
            let mut flags = 0u32;
            let mut sample = None;
            self.reader
                .ReadSample(STREAM, 0, None, Some(&mut flags), None, Some(&mut sample))
                .context("камера перестала отдавать кадры")?;
            if flags & (MF_SOURCE_READERF_ERROR.0 as u32 | MF_SOURCE_READERF_ENDOFSTREAM.0 as u32)
                != 0
            {
                anyhow::bail!("камера отключилась");
            }
            let Some(sample) = sample else { return Ok(None) };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (w, h) = (self.width as usize, self.height as usize);
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer>() {
                let mut data = std::ptr::null_mut();
                let mut pitch = 0i32;
                buffer2d.Lock2D(&mut data, &mut pitch)?;
                let frame = (pitch > 0).then(|| {
                    let pitch = pitch as usize;
                    let all = std::slice::from_raw_parts(data, pitch * h * 3 / 2);
                    let (y, uv) = all.split_at(pitch * h);
                    I420::from_nv12(self.width, self.height, y, pitch, uv, pitch)
                });
                let _ = buffer2d.Unlock2D();
                return Ok(frame);
            }
            let mut data = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut data, None, Some(&mut len))?;
            let frame = (len as usize >= w * h * 3 / 2).then(|| {
                let all = std::slice::from_raw_parts(data, len as usize);
                let (y, uv) = all.split_at(w * h);
                I420::from_nv12(self.width, self.height, y, w, uv, w)
            });
            let _ = buffer.Unlock();
            Ok(frame)
        }
    }
}

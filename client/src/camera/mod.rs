//! Камера: снимаем, сжимаем сразу в нескольких качествах (слоях), отдаём в сеть.
//!
//! Как в групповых звонках Telegram: отправитель кодирует до
//! [`vwf_proto::CAMERA_LAYERS`] слоёв ([`LAYERS`]), а сервер каждому зрителю
//! пересылает один — какой нужен его плитке и тянет его сеть. Слой, который
//! никто не смотрит, не кодируется ([`CameraSharer::set_viewers`]). Самый
//! маленький кодируется всегда: из него же своё превью в комнате.
//!
//! Источники кадров: веб-камера Windows (Media Foundation), камера Android
//! (кадры приходят из Kotlin, см. [`android`]) и проверочная картинка
//! ([`fake`], включается переменной `VWF_FAKE_CAMERA=1`).

mod fake;
mod frame;
mod sharer;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(windows)]
mod windows;

use std::time::Instant;

pub use frame::I420;
pub use sharer::CameraSharer;

/// Качество одного слоя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layer {
    /// Наибольшая высота кадра (ширина — по пропорциям камеры).
    pub max_height: u32,
    pub fps: u32,
    /// Битрейт для кадра 16:9 этой высоты, бит/с.
    pub bitrate: u32,
}

/// Слои камеры, от маленького к большому (номер слоя — индекс).
pub const LAYERS: [Layer; vwf_proto::CAMERA_LAYERS] = [
    // Плитки: лицо узнаётся, трафика почти нет.
    Layer { max_height: 180, fps: 15, bitrate: 150_000 },
    // Крупная плитка, когда участников мало.
    Layer { max_height: 360, fps: 30, bitrate: 500_000 },
    // На весь экран.
    Layer { max_height: 720, fps: 30, bitrate: 1_400_000 },
];

impl Layer {
    /// Битрейт для кадра `width`×`height`: пропорционально числу точек
    /// относительно 16:9, но не меньше половины.
    pub fn bitrate_for(&self, width: u32, height: u32) -> u32 {
        let reference = u64::from(self.max_height) * u64::from(self.max_height) * 16 / 9;
        let pixels = u64::from(width) * u64::from(height);
        let bitrate = u64::from(self.bitrate) * pixels / reference.max(1);
        bitrate.clamp(u64::from(self.bitrate / 2), u64::from(self.bitrate)) as u32
    }
}

/// Сколько слоёв отдаёт камера на этой платформе. На телефоне программный
/// кодер 720p в реальном времени тянет не всякий процессор — там два слоя.
pub const fn platform_layers() -> u8 {
    if cfg!(target_os = "android") { 2 } else { vwf_proto::CAMERA_LAYERS as u8 }
}

/// Камера, которую можно выбрать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraInfo {
    /// Идентификатор для [`open`].
    pub id: String,
    /// Название для человека.
    pub name: String,
}

/// Откуда берутся кадры камеры. Создаётся и живёт в потоке камеры
/// ([`CameraSharer`]): объекты Media Foundation нельзя передавать между потоками.
pub trait CameraSource {
    /// Ждёт следующий кадр не дольше `deadline`. `None` — кадр не успел прийти.
    fn frame(&mut self, deadline: Instant) -> anyhow::Result<Option<I420>>;
}

/// Проверочная камера включена (`VWF_FAKE_CAMERA=1`): на компьютере без
/// веб-камеры так можно проверить видео в комнате.
fn fake_enabled() -> bool {
    std::env::var_os("VWF_FAKE_CAMERA").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Список камер. Первая — по умолчанию.
pub fn cameras() -> anyhow::Result<Vec<CameraInfo>> {
    let mut list = Vec::new();
    #[cfg(windows)]
    list.extend(windows::cameras()?);
    #[cfg(target_os = "android")]
    list.extend(android::cameras());
    if fake_enabled() {
        list.push(CameraInfo { id: fake::ID.into(), name: "Проверочная камера".into() });
    }
    Ok(list)
}

/// Открывает камеру по идентификатору из [`cameras`]. Вызывать в потоке,
/// где камера будет работать.
pub fn open(id: &str) -> anyhow::Result<Box<dyn CameraSource>> {
    if id == fake::ID {
        return Ok(Box::new(fake::FakeCamera::new()));
    }
    #[cfg(windows)]
    {
        windows::open(id)
    }
    #[cfg(target_os = "android")]
    {
        android::open(id)
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        anyhow::bail!("камера на этой системе не поддерживается")
    }
}

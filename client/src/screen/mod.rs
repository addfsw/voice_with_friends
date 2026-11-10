//! Демонстрация экрана.
//!
//! Показывающий: [`capture`] снимает экран, [`share::Sharer`] сжимает кадры
//! в H.264 и отдаёт их в сеть. Сервер пересылает кадры тем, кто нажал
//! «Смотреть». Зритель: [`view::Viewer`] декодирует кадры и превращает в JPEG,
//! который показывает окно просмотра.

pub mod capture;
pub(crate) mod codec;
pub mod share;
pub mod view;

pub use capture::{FrameSource, Image, Source, sources};
pub use share::{ShareSettings, Sharer};
pub use view::{Viewer, ViewerFrame};

/// Сжатый кадр демонстрации, готовый к отправке.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub header: vwf_proto::VideoHeader,
    /// H.264.
    pub data: Vec<u8>,
}

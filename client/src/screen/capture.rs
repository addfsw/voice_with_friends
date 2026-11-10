//! Захват экрана: что можно показать (мониторы, окна) и получение кадров.
//!
//! На Windows мониторы снимаются через DXGI Desktop Duplication: видеокарта
//! сама отдаёт кадр, когда изображение изменилось, и почти не нагружает
//! процессор. Окна — через GDI (снимок по таймеру). В Linux — простой снимок
//! экрана X11: он нужен только для проверок.

use std::time::Instant;

/// Кадр с экрана: RGBA, построчно, без отступов.
#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Что можно показать.
#[derive(Clone, Debug)]
pub struct Source {
    /// Идентификатор для [`open`].
    pub id: String,
    /// Название для человека: «Экран 1 (1920×1080)», заголовок окна.
    pub name: String,
    /// Окно (а не весь монитор).
    pub window: bool,
}

/// Откуда берутся кадры. Позволяет подставить в тестах свою картинку.
///
/// Источник создаётся и живёт в потоке демонстрации ([`super::Sharer`]):
/// объекты захвата Windows нельзя передавать между потоками.
pub trait FrameSource {
    /// Ждёт следующий кадр не дольше `deadline`. `None` — изображение не
    /// изменилось (или новый кадр не успел прийти).
    fn frame(&mut self, deadline: Instant) -> anyhow::Result<Option<Image>>;

    /// Никто не смотрит: можно не снимать. По умолчанию ничего не делает.
    fn pause(&mut self) {}

    fn resume(&mut self) {}
}

/// Список того, что можно показать: сначала мониторы, потом окна.
pub fn sources() -> anyhow::Result<Vec<Source>> {
    platform::sources()
}

/// Открывает источник по идентификатору из [`sources`].
pub fn open(id: &str) -> anyhow::Result<Box<dyn FrameSource>> {
    platform::open(id)
}

/// Ждёт до `deadline` (для источников, которые снимают по таймеру).
#[allow(dead_code)]
fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        std::thread::sleep(deadline - now);
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::time::Instant;

    use anyhow::Context;
    use xcap::{Frame, Monitor, VideoRecorder, Window};

    use super::{FrameSource, Image, Source, sleep_until};

    pub fn sources() -> anyhow::Result<Vec<Source>> {
        let mut list = Vec::new();
        let monitors = Monitor::all().context("не удалось получить список мониторов")?;
        let several = monitors.len() > 1;
        for (i, monitor) in monitors.iter().enumerate() {
            let (Ok(id), Ok(w), Ok(h)) = (monitor.id(), monitor.width(), monitor.height()) else {
                continue;
            };
            let primary = if several && monitor.is_primary().unwrap_or(false) {
                ", основной"
            } else {
                ""
            };
            let name = if several {
                format!("Экран {} ({w}×{h}{primary})", i + 1)
            } else {
                format!("Весь экран ({w}×{h})")
            };
            list.push(Source { id: format!("monitor:{id}"), name, window: false });
        }
        let me = std::process::id();
        for window in Window::all().unwrap_or_default() {
            let title = window.title().unwrap_or_default();
            let usable = !title.trim().is_empty()
                && !window.is_minimized().unwrap_or(true)
                && window.width().unwrap_or(0) >= 200
                && window.height().unwrap_or(0) >= 150
                && window.pid().ok() != Some(me);
            let Ok(id) = window.id() else { continue };
            if usable {
                let app = window.app_name().unwrap_or_default();
                let name = if app.is_empty() || title.contains(&app) {
                    title
                } else {
                    format!("{title} — {app}")
                };
                list.push(Source { id: format!("window:{id}"), name, window: true });
            }
        }
        Ok(list)
    }

    pub fn open(id: &str) -> anyhow::Result<Box<dyn FrameSource>> {
        if let Some(id) = id.strip_prefix("monitor:") {
            let id: u32 = id.parse()?;
            let monitor = Monitor::all()?
                .into_iter()
                .find(|m| m.id().ok() == Some(id))
                .context("монитор не найден — возможно, его отключили")?;
            // DXGI бывает недоступен (удалённый рабочий стол, некоторые
            // ноутбуки с двумя видеокартами) — тогда снимаем через GDI.
            return Ok(match monitor.video_recorder() {
                Ok((recorder, frames)) if recorder.start().is_ok() => {
                    Box::new(Dxgi { recorder, frames, running: true })
                }
                _ => Box::new(MonitorGdi(monitor)),
            });
        }
        if let Some(id) = id.strip_prefix("window:") {
            let id: u32 = id.parse()?;
            let window = Window::all()?
                .into_iter()
                .find(|w| w.id().ok() == Some(id))
                .context("окно не найдено — возможно, его закрыли")?;
            return Ok(Box::new(WindowGdi(window)));
        }
        anyhow::bail!("неизвестный источник {id}")
    }

    struct Dxgi {
        recorder: VideoRecorder,
        frames: Receiver<Frame>,
        running: bool,
    }

    impl FrameSource for Dxgi {
        fn frame(&mut self, deadline: Instant) -> anyhow::Result<Option<Image>> {
            let wait = deadline.saturating_duration_since(Instant::now());
            match self.frames.recv_timeout(wait) {
                Ok(f) => Ok(Some(Image { width: f.width, height: f.height, rgba: f.raw })),
                Err(RecvTimeoutError::Timeout) => Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("захват экрана прервался (сменилось разрешение или видеорежим?)")
                }
            }
        }

        fn pause(&mut self) {
            if std::mem::replace(&mut self.running, false) {
                let _ = self.recorder.stop();
            }
        }

        fn resume(&mut self) {
            if !std::mem::replace(&mut self.running, true) {
                let _ = self.recorder.start();
            }
        }
    }

    impl Drop for Dxgi {
        fn drop(&mut self) {
            let _ = self.recorder.stop();
        }
    }

    struct MonitorGdi(Monitor);

    impl FrameSource for MonitorGdi {
        fn frame(&mut self, deadline: Instant) -> anyhow::Result<Option<Image>> {
            sleep_until(deadline);
            let image = self.0.capture_image().context("не удалось снять экран")?;
            let (width, height) = image.dimensions();
            Ok(Some(Image { width, height, rgba: image.into_raw() }))
        }
    }

    struct WindowGdi(Window);

    impl FrameSource for WindowGdi {
        fn frame(&mut self, deadline: Instant) -> anyhow::Result<Option<Image>> {
            sleep_until(deadline);
            // Свёрнутое окно не снять — просто ждём, пока его развернут.
            if self.0.is_minimized().unwrap_or(false) {
                return Ok(None);
            }
            let image = self.0.capture_image().context("окно закрыли или его не снять")?;
            let (width, height) = image.dimensions();
            Ok(Some(Image { width, height, rgba: image.into_raw() }))
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::time::Instant;

    use anyhow::Context;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat, Window};
    use x11rb::rust_connection::RustConnection;

    use super::{FrameSource, Image, Source, sleep_until};

    fn connect() -> anyhow::Result<(RustConnection, Window, u16, u16)> {
        let (conn, screen) = x11rb::connect(None).context("нет доступа к X11 (DISPLAY)")?;
        let root = &conn.setup().roots[screen];
        let (root, w, h) = (root.root, root.width_in_pixels, root.height_in_pixels);
        Ok((conn, root, w, h))
    }

    pub fn sources() -> anyhow::Result<Vec<Source>> {
        let (_, _, w, h) = connect()?;
        Ok(vec![Source {
            id: "screen".into(),
            name: format!("Весь экран ({w}×{h})"),
            window: false,
        }])
    }

    pub fn open(id: &str) -> anyhow::Result<Box<dyn FrameSource>> {
        anyhow::ensure!(id == "screen", "неизвестный источник {id}");
        let (conn, root, width, height) = connect()?;
        Ok(Box::new(X11 { conn, root, width, height }))
    }

    struct X11 {
        conn: RustConnection,
        root: Window,
        width: u16,
        height: u16,
    }

    impl FrameSource for X11 {
        fn frame(&mut self, deadline: Instant) -> anyhow::Result<Option<Image>> {
            sleep_until(deadline);
            let (w, h) = (self.width, self.height);
            let reply = self
                .conn
                .get_image(ImageFormat::Z_PIXMAP, self.root, 0, 0, w, h, !0)?
                .reply()
                .context("не удалось снять экран X11")?;
            // 24-битный экран X11 хранит пиксели как BGRX.
            let mut rgba = reply.data;
            anyhow::ensure!(rgba.len() == w as usize * h as usize * 4, "неожиданный формат экрана");
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
                px[3] = 255;
            }
            Ok(Some(Image { width: w.into(), height: h.into(), rgba }))
        }
    }
}

/// Остальные системы (Android): показывать свой экран нельзя, но смотреть
/// чужой — можно, поэтому модуль собирается, а список источников пуст.
#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use super::{FrameSource, Source};

    pub fn sources() -> anyhow::Result<Vec<Source>> {
        Ok(Vec::new())
    }

    pub fn open(_id: &str) -> anyhow::Result<Box<dyn FrameSource>> {
        anyhow::bail!("на этом устройстве показывать экран нельзя")
    }
}

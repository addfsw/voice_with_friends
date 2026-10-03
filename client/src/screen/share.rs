//! Показ своего экрана: поток, который снимает экран, сжимает кадры и
//! отдаёт их в сеть.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vwf_proto::VideoHeader;

use super::VideoFrame;
use super::capture::{self, FrameSource, Image};
use super::codec::{self, H264Encoder};

/// Качество демонстрации.
#[derive(Debug, Clone, Copy)]
pub struct ShareSettings {
    /// Наибольшая высота кадра (720, 1080...); 0 — как на экране.
    pub max_height: u32,
    /// Кадров в секунду.
    pub fps: u32,
}

impl Default for ShareSettings {
    fn default() -> Self {
        Self { max_height: 1080, fps: 30 }
    }
}

impl ShareSettings {
    /// Битрейт под размер и частоту кадров: около 0,1 бита на пиксель —
    /// текст на экране читается, 720p при 30 к/с — ~2,8 Мбит/с,
    /// 1080p — ~6 Мбит/с.
    fn bitrate(&self, width: u32, height: u32) -> u32 {
        let bits = f64::from(width) * f64::from(height) * f64::from(self.fps) * 0.1;
        (bits as u32).clamp(800_000, 8_000_000)
    }
}

/// Идёт демонстрация экрана. Останавливается при удалении.
pub struct Sharer {
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
    viewers: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl Sharer {
    /// Начинает показ источника `source` (идентификатор из [`capture::sources`]).
    ///
    /// `send` отдаёт кадр в сеть; `false` — очередь отправки полна (сеть не
    /// успевает), кадр выброшен. `on_error` вызывается, если показ прервался
    /// сам (например, закрыли показываемое окно).
    pub fn start(
        source: &str,
        settings: ShareSettings,
        send: impl FnMut(VideoFrame) -> bool + Send + 'static,
        on_error: impl FnOnce(String) + Send + 'static,
    ) -> anyhow::Result<Self> {
        // Источник открываем в потоке показа: объекты захвата Windows
        // не всегда можно передавать между потоками.
        let source = source.to_string();
        Self::start_with(move || capture::open(&source), settings, send, on_error)
    }

    /// То же, но кадры берутся из `open()` — например, в тестах.
    pub fn start_with(
        open: impl FnOnce() -> anyhow::Result<Box<dyn FrameSource>> + Send + 'static,
        settings: ShareSettings,
        send: impl FnMut(VideoFrame) -> bool + Send + 'static,
        on_error: impl FnOnce(String) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let keyframe = Arc::new(AtomicBool::new(true));
        let viewers = Arc::new(AtomicU32::new(0));
        let (opened_tx, opened_rx) = std::sync::mpsc::channel();
        let thread = {
            let (stop, keyframe, viewers) = (stop.clone(), keyframe.clone(), viewers.clone());
            thread::Builder::new().name("vwf-screen-share".into()).spawn(move || {
                crate::audio::lower_thread_priority();
                let source = match open() {
                    Ok(source) => {
                        let _ = opened_tx.send(Ok(()));
                        source
                    }
                    Err(e) => {
                        let _ = opened_tx.send(Err(e));
                        return;
                    }
                };
                let state = Loop { stop, keyframe, viewers, settings };
                if let Err(e) = state.run(source, send) {
                    on_error(format!("{e:#}"));
                }
            })?
        };
        match opened_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => anyhow::bail!("поток демонстрации завершился"),
        }
        Ok(Self { stop, keyframe, viewers, thread: Some(thread) })
    }

    /// Прислать опорный кадр (новый зритель или у кого-то потерялись кадры).
    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    /// Сколько человек смотрит. Пока никто — экран не снимается.
    pub fn set_viewers(&self, count: u32) {
        self.viewers.store(count, Ordering::Relaxed);
    }

    pub fn viewers(&self) -> u32 {
        self.viewers.load(Ordering::Relaxed)
    }
}

impl Drop for Sharer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Loop {
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
    viewers: Arc<AtomicU32>,
    settings: ShareSettings,
}

impl Loop {
    fn run(
        &self,
        mut source: Box<dyn FrameSource>,
        mut send: impl FnMut(VideoFrame) -> bool,
    ) -> anyhow::Result<()> {
        let fps = self.settings.fps.clamp(1, 60);
        let frame_time = Duration::from_secs(1) / fps;
        let mut encoder: Option<H264Encoder> = None;
        // Последний снятый кадр: если экран не меняется, а новому зрителю
        // нужен опорный кадр, сжимаем его заново.
        let mut last: Option<Image> = None;
        let mut frame_no = 0u32;
        let mut next = Instant::now();
        let mut paused = false;

        while !self.stop.load(Ordering::Relaxed) {
            if self.viewers.load(Ordering::Relaxed) == 0 {
                if !paused {
                    source.pause();
                    paused = true;
                }
                thread::sleep(Duration::from_millis(100));
                next = Instant::now();
                continue;
            }
            if paused {
                source.resume();
                paused = false;
            }

            next += frame_time;
            let now = Instant::now();
            if next < now {
                // Не успеваем (медленный захват или кодер) — не пытаемся
                // догнать пропущенные кадры.
                next = now;
            }
            let fresh = source.frame(next)?;
            let changed = fresh.is_some();
            if let Some(image) = fresh {
                let (w, h) = codec::fit(image.width, image.height, self.settings.max_height);
                // Размер подходит — без лишней копии кадра (8 МБ в 1080p).
                last = Some(if (image.width, image.height) == (w, h) {
                    image
                } else {
                    codec::scale(&image, w, h)
                });
            }
            let want_key = self.keyframe.load(Ordering::Relaxed);
            let Some(image) = &last else { continue };
            if !changed && !want_key {
                continue;
            }

            // Новый размер (развернули окно) — новый кодер; он начнёт с опорного кадра.
            if encoder.as_ref().is_none_or(|e| (e.width, e.height) != (image.width, image.height)) {
                let bitrate = self.settings.bitrate(image.width, image.height);
                encoder = Some(H264Encoder::new(image.width, image.height, fps, bitrate)?);
            }
            let encoder = encoder.as_mut().expect("создан выше");
            self.keyframe.store(false, Ordering::Relaxed);
            let Some((data, keyframe)) = encoder.encode(image, want_key)? else { continue };
            let header = VideoHeader {
                frame: frame_no,
                keyframe,
                width: image.width.min(u16::MAX.into()) as u16,
                height: image.height.min(u16::MAX.into()) as u16,
            };
            frame_no = frame_no.wrapping_add(1);
            if !send(VideoFrame { header, data }) {
                // Кадр выброшен — у зрителей дальше всё сломается, пока не
                // придёт опорный кадр. Шлём его, как только сеть разгрузится.
                self.keyframe.store(true, Ordering::Relaxed);
            }
        }
        Ok(())
    }
}

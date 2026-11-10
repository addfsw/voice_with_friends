//! Поток камеры: снимает кадры и сжимает их в нужные слои.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vwf_proto::{CAMERA_LAYERS, VideoHeader, VideoKind};

use super::{CameraSource, I420, LAYERS};
use crate::screen::VideoFrame;
use crate::screen::codec::{self, H264Encoder};

/// Включённая камера. Останавливается при удалении.
pub struct CameraSharer {
    stop: Arc<AtomicBool>,
    shared: Arc<Shared>,
    layers: u8,
    thread: Option<JoinHandle<()>>,
}

/// Что поток камеры узнаёт от сети.
#[derive(Default)]
struct Shared {
    /// Сколько зрителей у каждого слоя (сообщает сервер).
    viewers: [AtomicU32; CAMERA_LAYERS],
    /// Нужен опорный кадр слоя.
    keyframe: [AtomicBool; CAMERA_LAYERS],
}

impl CameraSharer {
    /// Включает камеру `id` (из [`super::cameras`]) с `layers` слоями.
    ///
    /// `send` отдаёт кадр в сеть (`false` — очередь полна, кадр выброшен);
    /// `preview` получает каждый кадр слоя 0 — для своей плитки; `on_error`
    /// вызывается, если камера отключилась сама (например, её выдернули).
    pub fn start(
        id: &str,
        layers: u8,
        send: impl FnMut(VideoFrame) -> bool + Send + 'static,
        preview: impl FnMut(VideoFrame) + Send + 'static,
        on_error: impl FnOnce(String) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let id = id.to_string();
        Self::start_with(move || super::open(&id), layers, send, preview, on_error)
    }

    /// То же, но кадры берутся из `open()` — например, в тестах.
    pub fn start_with(
        open: impl FnOnce() -> anyhow::Result<Box<dyn CameraSource>> + Send + 'static,
        layers: u8,
        send: impl FnMut(VideoFrame) -> bool + Send + 'static,
        preview: impl FnMut(VideoFrame) + Send + 'static,
        on_error: impl FnOnce(String) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let layers = layers.clamp(1, CAMERA_LAYERS as u8);
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared::default());
        let (opened_tx, opened_rx) = std::sync::mpsc::channel();
        let thread = {
            let (stop, shared) = (stop.clone(), shared.clone());
            thread::Builder::new().name("vwf-camera".into()).spawn(move || {
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
                let mut state = Encoders::new(layers, shared);
                if let Err(e) = state.run(&stop, source, send, preview) {
                    on_error(format!("{e:#}"));
                }
            })?
        };
        match opened_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => anyhow::bail!("поток камеры завершился"),
        }
        Ok(Self { stop, shared, layers, thread: Some(thread) })
    }

    /// Сколько слоёв отдаёт эта камера.
    pub fn layers(&self) -> u8 {
        self.layers
    }

    /// Сколько зрителей у каждого слоя: слои без зрителей не кодируются
    /// (кроме слоя 0 — он нужен для своего превью) и не отправляются.
    pub fn set_viewers(&self, viewers: [u32; CAMERA_LAYERS]) {
        for (slot, count) in self.shared.viewers.iter().zip(viewers) {
            slot.store(count, Ordering::Relaxed);
        }
    }

    /// Прислать опорный кадр слоя (новый зритель или у кого-то потерялись кадры).
    pub fn request_keyframe(&self, layer: u8) {
        if let Some(flag) = self.shared.keyframe.get(usize::from(layer)) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for CameraSharer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Кодеры слоёв и их счёт кадров.
struct Encoders {
    layers: usize,
    shared: Arc<Shared>,
    encoders: [Option<H264Encoder>; CAMERA_LAYERS],
    frame_no: [u32; CAMERA_LAYERS],
    /// Когда слою пора следующий кадр (у слоя 0 частота ниже, чем у камеры).
    due: [Instant; CAMERA_LAYERS],
}

impl Encoders {
    fn new(layers: u8, shared: Arc<Shared>) -> Self {
        Self {
            layers: usize::from(layers),
            shared,
            encoders: Default::default(),
            frame_no: [0; CAMERA_LAYERS],
            due: [Instant::now(); CAMERA_LAYERS],
        }
    }

    fn run(
        &mut self,
        stop: &AtomicBool,
        mut source: Box<dyn CameraSource>,
        mut send: impl FnMut(VideoFrame) -> bool,
        mut preview: impl FnMut(VideoFrame),
    ) -> anyhow::Result<()> {
        while !stop.load(Ordering::Relaxed) {
            let Some(frame) = source.frame(Instant::now() + Duration::from_millis(200))? else {
                continue;
            };
            let now = Instant::now();
            for layer in 0..self.layers {
                let viewers = self.shared.viewers[layer].load(Ordering::Relaxed);
                if layer > 0 && viewers == 0 {
                    // Никто не смотрит — кодер не нужен. Новый начнёт с опорного кадра.
                    self.encoders[layer] = None;
                    continue;
                }
                let Some(encoded) = self.encode(layer, &frame, now)? else { continue };
                if layer == 0 {
                    preview(encoded.clone());
                }
                if viewers > 0 && !send(encoded) {
                    // Кадр выброшен — у зрителей картинка развалится до
                    // опорного. Шлём его, как только сеть разгрузится.
                    self.shared.keyframe[layer].store(true, Ordering::Relaxed);
                }
            }
        }
        Ok(())
    }

    /// Сжимает кадр для слоя, если слою пора. `None` — рано или кодер пропустил кадр.
    fn encode(
        &mut self,
        layer: usize,
        frame: &I420,
        now: Instant,
    ) -> anyhow::Result<Option<VideoFrame>> {
        let spec = LAYERS[layer];
        let frame_time = Duration::from_secs(1) / spec.fps;
        let want_key = self.shared.keyframe[layer].load(Ordering::Relaxed);
        // Немного раньше срока — можно: камера сама даёт кадры неровно.
        if now + frame_time / 4 < self.due[layer] && !want_key {
            return Ok(None);
        }
        // Отстали (медленный кодер, пауза) — не догоняем пропущенные кадры.
        self.due[layer] = (self.due[layer] + frame_time).max(now + frame_time / 2);

        let (w, h) = codec::fit(frame.width, frame.height, spec.max_height);
        let scaled;
        let image = if (frame.width, frame.height) == (w, h) {
            frame
        } else {
            scaled = frame.scale(w, h);
            &scaled
        };
        let encoder = &mut self.encoders[layer];
        if encoder.as_ref().is_none_or(|e| (e.width, e.height) != (image.width, image.height)) {
            let bitrate = spec.bitrate_for(image.width, image.height);
            *encoder = Some(H264Encoder::new(image.width, image.height, spec.fps, bitrate)?);
        }
        let encoder = encoder.as_mut().expect("создан выше");
        self.shared.keyframe[layer].store(false, Ordering::Relaxed);
        let Some((data, keyframe)) = encoder.encode_yuv(image, want_key)? else {
            return Ok(None);
        };
        let header = VideoHeader {
            frame: self.frame_no[layer],
            keyframe,
            width: image.width.min(u16::MAX.into()) as u16,
            height: image.height.min(u16::MAX.into()) as u16,
            kind: VideoKind::Camera,
            layer: layer as u8,
        };
        self.frame_no[layer] = self.frame_no[layer].wrapping_add(1);
        Ok(Some(VideoFrame { header, data }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Камера, которая отдаёт кадры 640×360 так быстро, как их берут.
    struct Still(u32);

    impl CameraSource for Still {
        fn frame(&mut self, _deadline: Instant) -> anyhow::Result<Option<I420>> {
            std::thread::sleep(Duration::from_millis(10));
            self.0 += 1;
            let mut frame = I420::new(640, 360);
            frame.data[..640 * 360].iter_mut().enumerate().for_each(|(i, v)| {
                *v = ((i as u32 + self.0 * 7) % 251) as u8;
            });
            Ok(Some(frame))
        }
    }

    /// Слои, у которых нет зрителей, не кодируются; слой 0 всегда идёт в
    /// превью, а в сеть — только когда у него есть зрители.
    #[test]
    fn encodes_only_watched_layers() {
        let sent: Arc<Mutex<Vec<VideoHeader>>> = Arc::default();
        let previews = Arc::new(AtomicU32::new(0));
        let sharer = {
            let (sent, previews) = (sent.clone(), previews.clone());
            CameraSharer::start_with(
                || Ok(Box::new(Still(0))),
                3,
                move |frame| {
                    sent.lock().unwrap().push(frame.header);
                    true
                },
                move |_| {
                    previews.fetch_add(1, Ordering::Relaxed);
                },
                |e| panic!("камера остановилась: {e}"),
            )
            .unwrap()
        };
        std::thread::sleep(Duration::from_millis(400));
        assert!(previews.load(Ordering::Relaxed) > 0, "превью идёт и без зрителей");
        assert!(sent.lock().unwrap().is_empty(), "без зрителей в сеть ничего");

        sharer.set_viewers([0, 2, 0]);
        std::thread::sleep(Duration::from_millis(500));
        drop(sharer);
        let sent = sent.lock().unwrap();
        assert!(!sent.is_empty());
        assert!(sent.iter().all(|h| h.layer == 1 && h.kind == VideoKind::Camera));
        assert!(sent[0].keyframe, "новый слой начинается с опорного кадра");
        assert_eq!((sent[0].width, sent[0].height), (640, 360));
        // Номера кадров слоя идут подряд.
        for (i, h) in sent.iter().enumerate() {
            assert_eq!(h.frame, i as u32);
        }
    }

    #[test]
    fn layer_sizes_follow_camera_aspect() {
        assert_eq!(codec::fit(640, 480, LAYERS[0].max_height), (240, 180));
        assert_eq!(codec::fit(1280, 720, LAYERS[1].max_height), (640, 360));
        // Маленькая камера не растягивается до верхнего слоя.
        assert_eq!(codec::fit(640, 480, LAYERS[2].max_height), (640, 480));
        assert!(LAYERS[0].bitrate_for(240, 180) >= LAYERS[0].bitrate / 2);
        assert_eq!(LAYERS[2].bitrate_for(1280, 720), LAYERS[2].bitrate);
    }
}

//! Просмотр чужого экрана: поток, который декодирует кадры и отдаёт их окну.

use std::collections::HashMap;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vwf_proto::{PeerId, VideoHeader};

use super::codec::H264Decoder;

/// Готовый к показу кадр.
#[derive(Debug, Clone)]
pub struct ViewerFrame {
    pub from: PeerId,
    pub width: u32,
    pub height: u32,
    pub jpeg: Vec<u8>,
}

enum Command {
    Frame(PeerId, VideoHeader, Vec<u8>),
    Forget(PeerId),
}

/// Сколько кадров может ждать декодирования. Если декодер не успевает,
/// лишние выбрасываются, и просим опорный кадр.
const QUEUE: usize = 8;

/// Не чаще этого просить опорный кадр у одного показывающего.
const KEYFRAME_INTERVAL: Duration = Duration::from_millis(500);

/// Декодер кадров всех показывающих. Останавливается при удалении.
pub struct Viewer {
    tx: Option<SyncSender<Command>>,
    thread: Option<JoinHandle<()>>,
}

/// Состояние показа одного собеседника.
struct Stream {
    decoder: H264Decoder,
    /// Какой кадр должен прийти следующим.
    expected: Option<u32>,
    /// Потеряли кадр — ждём опорный, а остальные пропускаем.
    waiting_keyframe: bool,
    asked: Option<Instant>,
}

impl Viewer {
    /// `on_frame` получает готовые кадры; `request_keyframe(id)` — когда у
    /// показывающего `id` нужно попросить опорный кадр (потерялись кадры).
    pub fn start(
        mut on_frame: impl FnMut(ViewerFrame) + Send + 'static,
        mut request_keyframe: impl FnMut(PeerId) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = sync_channel::<Command>(QUEUE);
        let thread = thread::Builder::new().name("vwf-screen-view".into()).spawn(move || {
            let mut streams: HashMap<PeerId, Stream> = HashMap::new();
            let mut ask = |stream: &mut Stream, from: PeerId| {
                stream.waiting_keyframe = true;
                if stream.asked.is_none_or(|t| t.elapsed() >= KEYFRAME_INTERVAL) {
                    stream.asked = Some(Instant::now());
                    request_keyframe(from);
                }
            };
            while let Ok(command) = rx.recv() {
                let (from, header, data) = match command {
                    Command::Frame(from, header, data) => (from, header, data),
                    Command::Forget(id) => {
                        streams.remove(&id);
                        continue;
                    }
                };
                let stream = match streams.entry(from) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => match H264Decoder::new() {
                        Ok(decoder) => e.insert(Stream {
                            decoder,
                            expected: None,
                            waiting_keyframe: true,
                            asked: None,
                        }),
                        Err(err) => {
                            eprintln!("{err:#}");
                            continue;
                        }
                    },
                };
                // Пропущенный кадр ломает все следующие до опорного: каждый
                // кадр H.264 — это изменения относительно предыдущего.
                let gap = stream.expected.is_some_and(|n| n != header.frame);
                if !header.keyframe && (stream.waiting_keyframe || gap) {
                    ask(stream, from);
                    continue;
                }
                stream.expected = Some(header.frame.wrapping_add(1));
                match stream.decoder.decode(&data) {
                    Ok(Some((width, height, jpeg))) => {
                        stream.waiting_keyframe = false;
                        on_frame(ViewerFrame { from, width, height, jpeg });
                    }
                    Ok(None) => stream.waiting_keyframe = false,
                    Err(_) => {
                        // Декодер мог запутаться — начинаем заново с опорного кадра.
                        if let Ok(decoder) = H264Decoder::new() {
                            stream.decoder = decoder;
                        }
                        ask(stream, from);
                    }
                }
            }
        })?;
        Ok(Self { tx: Some(tx), thread: Some(thread) })
    }

    /// Пришёл кадр от `from`. `false` — декодер не успевает, кадр выброшен
    /// (поток сам попросит опорный кадр, когда освободится).
    pub fn push(&self, from: PeerId, header: VideoHeader, data: Vec<u8>) -> bool {
        let Some(tx) = &self.tx else { return false };
        match tx.try_send(Command::Frame(from, header, data)) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    /// Больше не смотрим `id` — забыть его декодер.
    pub fn forget(&self, id: PeerId) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Command::Forget(id));
        }
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        // Закрытый канал завершит поток.
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

//! Камера Android. Кадры снимает CameraX в Kotlin (`CameraCapture.kt`) и
//! через JNI (`app/src/android.rs`) кладёт сюда, в [`push_frame`]. Какую
//! камеру включить (фронтальную или основную), решает Kotlin: окно говорит
//! ему об этом само.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::time::Instant;

use super::{CameraInfo, CameraSource, I420};

/// Куда отдавать кадры: открытая камера и её номер (чтобы закрытие старой
/// не отключило новую).
static SINK: Mutex<Option<(u64, SyncSender<I420>)>> = Mutex::new(None);
static NEXT: Mutex<u64> = Mutex::new(0);

pub fn cameras() -> Vec<CameraInfo> {
    vec![
        CameraInfo { id: "front".into(), name: "Фронтальная".into() },
        CameraInfo { id: "back".into(), name: "Основная".into() },
    ]
}

pub fn open(_id: &str) -> anyhow::Result<Box<dyn CameraSource>> {
    // Два кадра в запасе: если кодер не успевает, лишние выбрасываются.
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    let id = {
        let mut next = NEXT.lock().unwrap();
        *next += 1;
        *next
    };
    *SINK.lock().unwrap() = Some((id, tx));
    Ok(Box::new(AndroidCamera { id, rx }))
}

/// Новый кадр с камеры (вызывает JNI). Если камера в программе не включена
/// или кодер не успевает, кадр выбрасывается.
pub fn push_frame(frame: I420) {
    if let Some((_, tx)) = SINK.lock().unwrap().as_ref() {
        let _ = tx.try_send(frame);
    }
}

struct AndroidCamera {
    id: u64,
    rx: Receiver<I420>,
}

impl CameraSource for AndroidCamera {
    fn frame(&mut self, deadline: Instant) -> anyhow::Result<Option<I420>> {
        // Берём самый свежий кадр: старые только добавили бы задержку.
        let mut frame = match self.rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(frame) => frame,
            Err(RecvTimeoutError::Timeout) => return Ok(None),
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("камера отключилась"),
        };
        loop {
            match self.rx.try_recv() {
                Ok(newer) => frame = newer,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        Ok(Some(frame))
    }
}

impl Drop for AndroidCamera {
    fn drop(&mut self) {
        let mut sink = SINK.lock().unwrap();
        if sink.as_ref().is_some_and(|(id, _)| *id == self.id) {
            *sink = None;
        }
    }
}

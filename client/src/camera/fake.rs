//! Проверочная камера: движущаяся картинка 1280×720, 30 к/с. Нужна, чтобы
//! проверить видео в комнате на компьютере без веб-камеры.

use std::time::{Duration, Instant};

use super::{CameraSource, I420};

pub const ID: &str = "fake";

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FRAME_TIME: Duration = Duration::from_millis(33);

pub struct FakeCamera {
    next: Instant,
    n: u32,
}

impl FakeCamera {
    pub fn new() -> Self {
        Self { next: Instant::now(), n: 0 }
    }
}

impl CameraSource for FakeCamera {
    fn frame(&mut self, deadline: Instant) -> anyhow::Result<Option<I420>> {
        if self.next > deadline {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            return Ok(None);
        }
        std::thread::sleep(self.next.saturating_duration_since(Instant::now()));
        self.next = (self.next + FRAME_TIME).max(Instant::now());
        self.n = self.n.wrapping_add(1);
        Ok(Some(picture(self.n)))
    }
}

/// Цветной фон, который медленно меняется, и белый квадрат, который ездит
/// по кругу: по нему видно и плавность, и задержку.
fn picture(n: u32) -> I420 {
    let mut frame = I420::new(WIDTH, HEIGHT);
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let t = n as f32 / 30.0;
    let (cx, cy) = (
        (w as f32 / 2.0 + (t * 1.3).cos() * w as f32 * 0.3) as usize,
        (h as f32 / 2.0 + (t * 1.3).sin() * h as f32 * 0.3) as usize,
    );
    let side = h / 5;
    let (y, u, v) = frame.planes_mut();
    for row in 0..h {
        for col in 0..w {
            let inside = col.abs_diff(cx) < side / 2 && row.abs_diff(cy) < side / 2;
            y[row * w + col] = if inside { 235 } else { (60 + (col + row) * 80 / (w + h)) as u8 };
        }
    }
    let hue = (n / 3 % 256) as u8;
    u.fill(hue);
    v.fill(255 - hue);
    frame
}

//! Замер скорости этапов демонстрации экрана:
//! `cargo run --release -p vwf-client --example screen_bench`.
//!
//! «Экран» синтетический и меняется целиком каждый кадр (как видео или
//! игра) — худший случай для кодера.

use std::time::{Duration, Instant};

use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
};
use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};

fn frame(w: u32, h: u32, t: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            // Плавный градиент + сдвигающиеся полосы и «шум» — похоже на видео.
            let v = ((x + t * 7) / 24 % 2 * 120) as u8;
            let n = ((x * 31 + y * 17 + t * 13) % 64) as u8;
            rgba.extend_from_slice(&[v.wrapping_add(n), (y / 4) as u8, ((x + t) / 8) as u8, 255]);
        }
    }
    rgba
}

fn ms(d: Duration, n: u32) -> f64 {
    d.as_secs_f64() * 1000.0 / f64::from(n)
}

/// Весь конвейер показа (поток `Sharer`: темп, масштаб, кодер) на
/// синтетическом источнике, который отдаёт новый кадр мгновенно.
fn sharer_fps(w: u32, h: u32, max_height: u32) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use vwf_client::screen::{FrameSource, Image, ShareSettings, Sharer};

    struct Synthetic(Vec<Vec<u8>>, usize, u32, u32);
    impl FrameSource for Synthetic {
        fn frame(&mut self, deadline: Instant) -> anyhow::Result<Option<Image>> {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            self.1 += 1;
            let rgba = self.0[self.1 % self.0.len()].clone();
            Ok(Some(Image { width: self.2, height: self.3, rgba }))
        }
    }
    let frames: Vec<Vec<u8>> = (0..8).map(|t| frame(w, h, t)).collect();
    let sent = Arc::new(AtomicU32::new(0));
    let counter = sent.clone();
    let sharer = Sharer::start_with(
        move || Ok(Box::new(Synthetic(frames, 0, w, h))),
        ShareSettings { max_height, fps: 30 },
        move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
            true
        },
        |e| panic!("{e}"),
    )
    .unwrap();
    sharer.set_viewers(1);
    std::thread::sleep(Duration::from_secs(1));
    let before = sent.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_secs(3));
    let fps = f64::from(sent.load(Ordering::Relaxed) - before) / 3.0;
    println!("показ {w}×{h} → {max_height}p, цель 30 к/с: {fps:.1} к/с");
}

fn main() {
    sharer_fps(1920, 1080, 1080);
    sharer_fps(2560, 1440, 1080);
    sharer_fps(1920, 1080, 720);
    let n = 60u32;
    for (w, h) in [(1920u32, 1080u32), (1280, 720)] {
        println!("== {w}×{h}, {n} кадров");
        let frames: Vec<Vec<u8>> = (0..8).map(|t| frame(w, h, t)).collect();

        let mut yuv = YUVBuffer::new(w as usize, h as usize);
        let start = Instant::now();
        for i in 0..n {
            yuv.read_rgba8(RgbaSliceU8::new(&frames[i as usize % 8], (w as usize, h as usize)));
        }
        println!("RGBA→YUV (openh264):         {:6.1} мс", ms(start.elapsed(), n));

        for (label, usage, threads, complexity) in [
            ("экран, 1 поток", UsageType::ScreenContentRealTime, 1, Complexity::Medium),
            ("камера, 1 поток", UsageType::CameraVideoRealTime, 1, Complexity::Medium),
            ("камера, 4 потока", UsageType::CameraVideoRealTime, 4, Complexity::Medium),
            ("камера, 4 потока, low", UsageType::CameraVideoRealTime, 4, Complexity::Low),
            ("экран, 4 потока, low", UsageType::ScreenContentRealTime, 4, Complexity::Low),
        ] {
            let config = EncoderConfig::new()
                .usage_type(usage)
                .rate_control_mode(RateControlMode::Bitrate)
                .bitrate(BitRate::from_bps(6_000_000))
                .max_frame_rate(FrameRate::from_hz(30.0))
                .num_threads(threads)
                .complexity(complexity)
                .skip_frames(false);
            let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config).unwrap();
            let mut encoded = Vec::new();
            let mut yuvs = Vec::new();
            for f in &frames {
                let mut y = YUVBuffer::new(w as usize, h as usize);
                y.read_rgba8(RgbaSliceU8::new(f, (w as usize, h as usize)));
                yuvs.push(y);
            }
            let start = Instant::now();
            let mut bytes = 0;
            for i in 0..n {
                let out = encoder.encode(&yuvs[i as usize % 8]).unwrap().to_vec();
                bytes += out.len();
                if encoded.len() < n as usize {
                    encoded.push(out);
                }
            }
            println!(
                "H.264 {label:24} {:6.1} мс, {:5.1} Мбит/с при 30 к/с",
                ms(start.elapsed(), n),
                bytes as f64 * 8.0 * 30.0 / f64::from(n) / 1e6
            );

            if label == "камера, 4 потока" {
                let mut decoder = Decoder::new().unwrap();
                let mut rgb = vec![0u8; (w * h * 3) as usize];
                let (mut t_dec, mut t_rgb, mut t_jpeg, mut jpeg_bytes) =
                    (Duration::ZERO, Duration::ZERO, Duration::ZERO, 0);
                for data in &encoded {
                    let s = Instant::now();
                    let Some(frame) = decoder.decode(data).unwrap() else { continue };
                    t_dec += s.elapsed();
                    let s = Instant::now();
                    frame.write_rgb8(&mut rgb);
                    t_rgb += s.elapsed();
                    let s = Instant::now();
                    let mut jpeg = Vec::new();
                    jpeg_encoder::Encoder::new(&mut jpeg, 85)
                        .encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
                        .unwrap();
                    t_jpeg += s.elapsed();
                    jpeg_bytes += jpeg.len();
                    let _ = frame.dimensions();
                }
                let k = encoded.len() as u32;
                println!("декодер H.264:               {:6.1} мс", ms(t_dec, k));
                println!("YUV→RGB:                     {:6.1} мс", ms(t_rgb, k));
                println!(
                    "JPEG:                        {:6.1} мс, {} КБ на кадр",
                    ms(t_jpeg, k),
                    jpeg_bytes / k as usize / 1024
                );
            }
        }
    }
}

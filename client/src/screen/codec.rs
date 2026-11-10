//! Масштабирование кадров, H.264 (OpenH264) и JPEG для показа в окне.

use anyhow::Context;
use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod,
    RateControlMode, UsageType,
};
use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};

use super::capture::Image;

/// Размер, в который вписывается кадр `width`×`height`, чтобы не превышать
/// `max_height` по высоте (0 — без ограничения). Обе стороны чётные: этого
/// требует формат YUV 4:2:0, с которым работает H.264.
pub fn fit(width: u32, height: u32, max_height: u32) -> (u32, u32) {
    let (w, h) = if max_height > 0 && height > max_height {
        let w = (u64::from(width) * u64::from(max_height) / u64::from(height)) as u32;
        (w, max_height)
    } else {
        (width, height)
    };
    ((w & !1).max(2), (h & !1).max(2))
}

/// Масштабирует RGBA-кадр до `width`×`height` (билинейно).
///
/// Это на каждом кадре (например, 1440p → 1080p), поэтому: коэффициенты по
/// горизонтали считаются один раз на кадр, арифметика целочисленная, а
/// строки делятся между ядрами процессора.
pub fn scale(src: &Image, width: u32, height: u32) -> Image {
    if (src.width, src.height) == (width, height) {
        return src.clone();
    }
    let (sw, sh) = (src.width as usize, src.height as usize);
    let (dw, dh) = (width as usize, height as usize);

    // Для каждого столбца результата: левый пиксель источника и вес правого
    // (0..=256). Координаты — с фиксированной точкой 16.16.
    let x_step = ((sw as u64) << 16) / dw as u64;
    let columns: Vec<(usize, u32)> = (0..dw)
        .map(|x| {
            let fx = x as u64 * x_step;
            (((fx >> 16) as usize).min(sw - 1), ((fx & 0xffff) >> 8) as u32)
        })
        .collect();
    let y_step = ((sh as u64) << 16) / dh as u64;

    let mut rgba = vec![0u8; dw * dh * 4];
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 4);
    let rows_per_thread = dh.div_ceil(threads);
    std::thread::scope(|scope| {
        for (chunk, out) in rgba.chunks_mut(rows_per_thread * dw * 4).enumerate() {
            let columns = &columns;
            scope.spawn(move || {
                crate::audio::lower_thread_priority();
                for (i, out) in out.chunks_exact_mut(dw * 4).enumerate() {
                    let y = chunk * rows_per_thread + i;
                    let fy = y as u64 * y_step;
                    let y0 = ((fy >> 16) as usize).min(sh - 1);
                    let y1 = (y0 + 1).min(sh - 1);
                    let wy = ((fy & 0xffff) >> 8) as u32;
                    let row0 = &src.rgba[y0 * sw * 4..][..sw * 4];
                    let row1 = &src.rgba[y1 * sw * 4..][..sw * 4];
                    for (px, &(x0, wx)) in out.chunks_exact_mut(4).zip(columns) {
                        let x1 = (x0 + 1).min(sw - 1);
                        let (a, b) = (&row0[x0 * 4..x0 * 4 + 4], &row0[x1 * 4..x1 * 4 + 4]);
                        let (c, d) = (&row1[x0 * 4..x0 * 4 + 4], &row1[x1 * 4..x1 * 4 + 4]);
                        for k in 0..4 {
                            let top = u32::from(a[k]) * (256 - wx) + u32::from(b[k]) * wx;
                            let bottom = u32::from(c[k]) * (256 - wx) + u32::from(d[k]) * wx;
                            px[k] = ((top * (256 - wy) + bottom * wy) >> 16) as u8;
                        }
                    }
                }
            });
        }
    });
    Image { width, height, rgba }
}

/// Кодер H.264 для одного размера кадра.
pub struct H264Encoder {
    encoder: Encoder,
    yuv: YUVBuffer,
    pub width: u32,
    pub height: u32,
}

impl H264Encoder {
    pub fn new(width: u32, height: u32, fps: u32, bitrate: u32) -> anyhow::Result<Self> {
        // Замеры (examples/screen_bench.rs): режим «экран» в 1,5–2 раза
        // медленнее «камеры» и не держит битрейт — на подвижной картинке выдаёт
        // в 4 раза больше заданного и забивает канал. Поэтому «камера»; а для
        // кадров больше 720p ещё и упрощённый поиск движения (в 1,5 раза быстрее).
        let complexity =
            if width * height > 1280 * 720 { Complexity::Low } else { Complexity::Medium };
        let config = EncoderConfig::new()
            .usage_type(UsageType::CameraVideoRealTime)
            .complexity(complexity)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(bitrate))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            // Опорный кадр раз в 10 секунд: даже если просьба о нём
            // потеряется, картинка у зрителя восстановится сама.
            .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 10))
            // Не пропускать кадры, чтобы уложиться в битрейт, — лучше
            // чуть менее чёткая, но плавная картинка.
            .skip_frames(false);
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config)
            .context("не удалось запустить кодер H.264")?;
        let yuv = YUVBuffer::new(width as usize, height as usize);
        Ok(Self { encoder, yuv, width, height })
    }

    /// Сжимает кадр (размер должен совпадать с кодером). Возвращает
    /// `(данные H.264, опорный ли кадр)` или `None`, если кодер решил кадр
    /// пропустить (не уложился в битрейт).
    pub fn encode(
        &mut self,
        image: &Image,
        keyframe: bool,
    ) -> anyhow::Result<Option<(Vec<u8>, bool)>> {
        debug_assert_eq!((image.width, image.height), (self.width, self.height));
        self.yuv.read_rgba8(RgbaSliceU8::new(
            &image.rgba,
            (image.width as usize, image.height as usize),
        ));
        encode_frame(&mut self.encoder, &self.yuv, keyframe)
    }

    /// То же для кадра, который уже в YUV 4:2:0 (камера).
    pub fn encode_yuv(
        &mut self,
        frame: &impl YUVSource,
        keyframe: bool,
    ) -> anyhow::Result<Option<(Vec<u8>, bool)>> {
        debug_assert_eq!(frame.dimensions(), (self.width as usize, self.height as usize));
        encode_frame(&mut self.encoder, frame, keyframe)
    }
}

fn encode_frame(
    encoder: &mut Encoder,
    frame: &impl YUVSource,
    keyframe: bool,
) -> anyhow::Result<Option<(Vec<u8>, bool)>> {
    if keyframe {
        encoder.force_intra_frame();
    }
    let stream = encoder.encode(frame).context("ошибка кодера H.264")?;
    let key = match stream.frame_type() {
        FrameType::IDR | FrameType::I => true,
        FrameType::P | FrameType::IPMixed => false,
        FrameType::Skip | FrameType::Invalid => return Ok(None),
    };
    let data = stream.to_vec();
    Ok((!data.is_empty()).then_some((data, key)))
}

/// Декодер H.264 одного собеседника: кадр сразу в JPEG для окна просмотра.
pub struct H264Decoder {
    decoder: Decoder,
    rgb: Vec<u8>,
}

/// Качество JPEG для показа (кадр уже прошёл через H.264, выше смысла нет).
const JPEG_QUALITY: u8 = 85;

impl H264Decoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            decoder: Decoder::new().context("не удалось запустить декодер H.264")?,
            rgb: Vec::new(),
        })
    }

    /// Декодирует кадр. `Ok(None)` — декодеру нужно больше данных.
    /// Возвращает `(ширина, высота, JPEG)`.
    pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<(u32, u32, Vec<u8>)>> {
        let Some(yuv) = self.decoder.decode(data).context("кадр не декодировался")?
        else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        self.rgb.resize(w * h * 3, 0);
        yuv.write_rgb8(&mut self.rgb);
        let mut jpeg = Vec::with_capacity(w * h / 4);
        jpeg_encoder::Encoder::new(&mut jpeg, JPEG_QUALITY)
            .encode(&self.rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
            .context("ошибка JPEG")?;
        Ok(Some((w as u32, h as u32, jpeg)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Картинка с полосками, которая сдвигается с каждым кадром.
    pub fn test_image(width: u32, height: u32, shift: u32) -> Image {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let v = (((x + shift) / 16 + y / 16) % 2 * 200 + 30) as u8;
                rgba.extend_from_slice(&[v, (x % 256) as u8, (y % 256) as u8, 255]);
            }
        }
        Image { width, height, rgba }
    }

    #[test]
    fn fit_keeps_aspect_and_even_sides() {
        assert_eq!(fit(2560, 1440, 1080), (1920, 1080));
        assert_eq!(fit(1920, 1080, 720), (1280, 720));
        assert_eq!(fit(1280, 720, 1080), (1280, 720));
        assert_eq!(fit(1365, 767, 0), (1364, 766));
    }

    #[test]
    fn scale_preserves_flat_color() {
        let src = Image { width: 4, height: 4, rgba: [10, 20, 30, 255].repeat(16) };
        let out = scale(&src, 3, 2);
        assert_eq!(out.rgba, [10, 20, 30, 255].repeat(6));
    }

    #[test]
    fn h264_roundtrip() {
        let (w, h) = (320, 240);
        let mut encoder = H264Encoder::new(w, h, 30, 1_000_000).unwrap();
        let mut decoder = H264Decoder::new().unwrap();
        let mut decoded = 0;
        for i in 0..10 {
            let Some((data, key)) = encoder.encode(&test_image(w, h, i * 4), i == 0).unwrap()
            else {
                continue;
            };
            assert_eq!(key, i == 0, "кадр {i}");
            if let Some((dw, dh, jpeg)) = decoder.decode(&data).unwrap() {
                assert_eq!((dw, dh), (w, h));
                assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "это JPEG");
                decoded += 1;
            }
        }
        assert!(decoded >= 8, "декодировано {decoded} из 10");
    }
}

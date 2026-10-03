//! Масштабирование кадров, H.264 (OpenH264) и JPEG для показа в окне.

use anyhow::Context;
use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode,
    UsageType,
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
pub fn scale(src: &Image, width: u32, height: u32) -> Image {
    if (src.width, src.height) == (width, height) {
        return src.clone();
    }
    let (sw, sh) = (src.width as usize, src.height as usize);
    let (dw, dh) = (width as usize, height as usize);
    let mut rgba = vec![0u8; dw * dh * 4];
    // Координаты в 16.16 с фиксированной точкой: быстро и без плавающей.
    let x_step = ((sw as u64) << 16) / dw as u64;
    let y_step = ((sh as u64) << 16) / dh as u64;
    for y in 0..dh {
        let fy = (y as u64 * y_step) as usize;
        let y0 = (fy >> 16).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let wy = (fy & 0xffff) as u32;
        let row0 = &src.rgba[y0 * sw * 4..][..sw * 4];
        let row1 = &src.rgba[y1 * sw * 4..][..sw * 4];
        let out = &mut rgba[y * dw * 4..][..dw * 4];
        for x in 0..dw {
            let fx = (x as u64 * x_step) as usize;
            let x0 = (fx >> 16).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let wx = (fx & 0xffff) as u32;
            for c in 0..4 {
                let top =
                    u32::from(row0[x0 * 4 + c]) * (0x10000 - wx) + u32::from(row0[x1 * 4 + c]) * wx;
                let bottom =
                    u32::from(row1[x0 * 4 + c]) * (0x10000 - wx) + u32::from(row1[x1 * 4 + c]) * wx;
                let v = (u64::from(top >> 8) * u64::from(0x10000 - wy)
                    + u64::from(bottom >> 8) * u64::from(wy))
                    >> 24;
                out[x * 4 + c] = v as u8;
            }
        }
    }
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
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(bitrate))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            // Опорный кадр раз в 10 секунд: даже если просьба о нём
            // потеряется, картинка у зрителя восстановится сама.
            .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 10))
            .skip_frames(true);
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
        if keyframe {
            self.encoder.force_intra_frame();
        }
        let stream = self.encoder.encode(&self.yuv).context("ошибка кодера H.264")?;
        let key = match stream.frame_type() {
            FrameType::IDR | FrameType::I => true,
            FrameType::P | FrameType::IPMixed => false,
            FrameType::Skip | FrameType::Invalid => return Ok(None),
        };
        let data = stream.to_vec();
        Ok((!data.is_empty()).then_some((data, key)))
    }
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

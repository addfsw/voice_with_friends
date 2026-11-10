//! Кадр камеры в YUV 4:2:0 (I420) — в нём же работает кодер H.264, так что
//! кадр с камеры не гоняется через RGBA туда и обратно.

use openh264::formats::YUVSource;

/// Кадр I420: три плоскости подряд — яркость `width×height`, затем U и V
/// по `width/2 × height/2`. Обе стороны чётные.
#[derive(Clone, PartialEq, Eq)]
pub struct I420 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for I420 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "I420({}×{})", self.width, self.height)
    }
}

impl I420 {
    /// Чёрный кадр. Нечётные стороны округляются вниз до чётных.
    pub fn new(width: u32, height: u32) -> Self {
        let (width, height) = ((width & !1).max(2), (height & !1).max(2));
        let luma = (width * height) as usize;
        let mut data = vec![0u8; luma * 3 / 2];
        data[luma..].fill(128);
        Self { width, height, data }
    }

    fn luma_len(&self) -> usize {
        (self.width * self.height) as usize
    }

    fn chroma_len(&self) -> usize {
        self.luma_len() / 4
    }

    pub fn y_plane(&self) -> &[u8] {
        &self.data[..self.luma_len()]
    }

    pub fn u_plane(&self) -> &[u8] {
        &self.data[self.luma_len()..][..self.chroma_len()]
    }

    pub fn v_plane(&self) -> &[u8] {
        &self.data[self.luma_len() + self.chroma_len()..]
    }

    /// Плоскости для записи: `(Y, U, V)`.
    pub fn planes_mut(&mut self) -> (&mut [u8], &mut [u8], &mut [u8]) {
        let (luma, chroma) = (self.luma_len(), self.chroma_len());
        let (y, uv) = self.data.split_at_mut(luma);
        let (u, v) = uv.split_at_mut(chroma);
        (y, u, v)
    }

    /// Из кадра NV12 (так отдают кадры веб-камеры в Windows): плоскость
    /// яркости и чередующиеся U/V. `y_stride` и `uv_stride` — длина строки в
    /// байтах (бывает больше ширины). Нечётный крайний столбец/строка отбрасываются.
    pub fn from_nv12(
        width: u32,
        height: u32,
        y: &[u8],
        y_stride: usize,
        uv: &[u8],
        uv_stride: usize,
    ) -> Self {
        Self::from_planes(width, height, y, y_stride, uv, &uv[1..], uv_stride, 2)
    }

    /// Из трёх плоскостей с произвольным шагом (так отдаёт кадры камера
    /// Android, `YUV_420_888`): у U и V `uv_pixel_stride` — шаг между
    /// соседними точками (1 — плоскости раздельные, 2 — чередуются).
    #[allow(clippy::too_many_arguments)]
    pub fn from_planes(
        width: u32,
        height: u32,
        y: &[u8],
        y_stride: usize,
        u: &[u8],
        v: &[u8],
        uv_stride: usize,
        uv_pixel_stride: usize,
    ) -> Self {
        let mut frame = Self::new(width, height);
        let (w, h) = (frame.width as usize, frame.height as usize);
        let (cw, ch) = (w / 2, h / 2);
        let (dy, du, dv) = frame.planes_mut();
        for row in 0..h {
            let src = &y[row * y_stride..];
            dy[row * w..][..w].copy_from_slice(&src[..w]);
        }
        for row in 0..ch {
            let (su, sv) = (&u[row * uv_stride..], &v[row * uv_stride..]);
            let (ou, ov) = (&mut du[row * cw..][..cw], &mut dv[row * cw..][..cw]);
            if uv_pixel_stride == 1 {
                ou.copy_from_slice(&su[..cw]);
                ov.copy_from_slice(&sv[..cw]);
            } else {
                for x in 0..cw {
                    ou[x] = su[x * uv_pixel_stride];
                    ov[x] = sv[x * uv_pixel_stride];
                }
            }
        }
        frame
    }

    /// Из RGBA (проверочная камера, тесты). Коэффициенты BT.601, как у кодера.
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Self {
        let mut frame = Self::new(width, height);
        let (w, h) = (frame.width as usize, frame.height as usize);
        let src_w = width as usize;
        let cw = w / 2;
        let (dy, du, dv) = frame.planes_mut();
        let px = |x: usize, y: usize| {
            let p = &rgba[(y * src_w + x) * 4..][..3];
            (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]))
        };
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = px(x, y);
                dy[y * w + x] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8;
            }
        }
        for y in 0..h / 2 {
            for x in 0..cw {
                let (r, g, b) = px(x * 2, y * 2);
                du[y * cw + x] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
                dv[y * cw + x] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            }
        }
        frame
    }

    /// Масштабирует кадр до `width`×`height` (билинейно, каждую плоскость).
    pub fn scale(&self, width: u32, height: u32) -> Self {
        let mut out = Self::new(width, height);
        if (out.width, out.height) == (self.width, self.height) {
            return self.clone();
        }
        let (sw, sh) = (self.width as usize, self.height as usize);
        let (dw, dh) = (out.width as usize, out.height as usize);
        let (sy, su, sv) = (self.y_plane(), self.u_plane(), self.v_plane());
        let (oy, ou, ov) = out.planes_mut();
        scale_plane(sy, sw, sh, oy, dw, dh);
        scale_plane(su, sw / 2, sh / 2, ou, dw / 2, dh / 2);
        scale_plane(sv, sw / 2, sh / 2, ov, dw / 2, dh / 2);
        out
    }
}

/// Билинейное масштабирование одной плоскости (фиксированная точка 16.16,
/// как у `screen::codec::scale`). Кадры камеры маленькие, поэтому в одном потоке.
fn scale_plane(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    let x_step = ((sw as u64) << 16) / dw as u64;
    let y_step = ((sh as u64) << 16) / dh as u64;
    let columns: Vec<(usize, usize, u32)> = (0..dw)
        .map(|x| {
            let fx = x as u64 * x_step;
            let x0 = ((fx >> 16) as usize).min(sw - 1);
            (x0, (x0 + 1).min(sw - 1), ((fx & 0xffff) >> 8) as u32)
        })
        .collect();
    for (y, out) in dst.chunks_exact_mut(dw).enumerate().take(dh) {
        let fy = y as u64 * y_step;
        let y0 = ((fy >> 16) as usize).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let wy = ((fy & 0xffff) >> 8) as u32;
        let (row0, row1) = (&src[y0 * sw..][..sw], &src[y1 * sw..][..sw]);
        for (px, &(x0, x1, wx)) in out.iter_mut().zip(&columns) {
            let top = u32::from(row0[x0]) * (256 - wx) + u32::from(row0[x1]) * wx;
            let bottom = u32::from(row1[x0]) * (256 - wx) + u32::from(row1[x1]) * wx;
            *px = ((top * (256 - wy) + bottom * wy) >> 16) as u8;
        }
    }
}

impl YUVSource for I420 {
    fn dimensions(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }

    fn strides(&self) -> (usize, usize, usize) {
        let w = self.width as usize;
        (w, w / 2, w / 2)
    }

    fn y(&self) -> &[u8] {
        self.y_plane()
    }

    fn u(&self) -> &[u8] {
        self.u_plane()
    }

    fn v(&self) -> &[u8] {
        self.v_plane()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_frame_is_black_and_even() {
        let frame = I420::new(5, 3);
        assert_eq!((frame.width, frame.height), (4, 2));
        assert_eq!(frame.y_plane(), &[0; 8]);
        assert_eq!(frame.u_plane(), &[128; 2]);
        assert_eq!(frame.v_plane(), &[128; 2]);
    }

    #[test]
    fn nv12_with_padding() {
        // 4×2, строки по 6 байт (2 байта отступа).
        let y = [1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
        let uv = [10, 20, 30, 40, 0, 0];
        let frame = I420::from_nv12(4, 2, &y, 6, &uv, 6);
        assert_eq!(frame.y_plane(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(frame.u_plane(), &[10, 30]);
        assert_eq!(frame.v_plane(), &[20, 40]);
    }

    #[test]
    fn separate_planes() {
        let y = [9; 16];
        let (u, v) = ([1, 2, 3, 4], [5, 6, 7, 8]);
        let frame = I420::from_planes(4, 4, &y, 4, &u, &v, 2, 1);
        assert_eq!(frame.u_plane(), &u);
        assert_eq!(frame.v_plane(), &v);
    }

    #[test]
    fn rgba_colors() {
        let white = I420::from_rgba(2, 2, &[255; 16]);
        assert_eq!(white.y_plane(), &[235; 4]);
        assert_eq!((white.u_plane()[0], white.v_plane()[0]), (128, 128));
        let red = I420::from_rgba(2, 2, &[255, 0, 0, 255].repeat(4));
        assert!(red.v_plane()[0] > 200, "у красного V высокий");
    }

    #[test]
    fn scale_keeps_flat_color() {
        let mut frame = I420::new(8, 8);
        frame.data.fill(77);
        let small = frame.scale(4, 2);
        assert_eq!((small.width, small.height), (4, 2));
        assert!(small.data.iter().all(|&v| v == 77));
        assert_eq!(small.data.len(), 4 * 2 * 3 / 2);
    }
}

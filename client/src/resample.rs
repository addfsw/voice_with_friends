//! Простейший потоковый ресемплер с линейной интерполяцией.
//!
//! Звуковые карты на Windows обычно работают на 48 кГц, но бывают и 44,1 кГц,
//! а Opus мы кормим ровно 48 кГц. Для голоса линейной интерполяции хватает;
//! при понижении частоты без фильтра возможен лёгкий алиасинг на верхах,
//! но в полосе речи (до ~8 кГц) он не слышен. Позже можно заменить на `rubato`.

pub struct Resampler {
    /// Сколько входных сэмплов приходится на один выходной.
    step: f64,
    /// Положение следующего выходного сэмпла между `prev` (0.0) и текущим входным (1.0).
    pos: f64,
    prev: f32,
    passthrough: bool,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            step: from_rate as f64 / to_rate as f64,
            pos: 0.0,
            prev: 0.0,
            passthrough: from_rate == to_rate,
        }
    }

    /// Подаёт один входной сэмпл; `out` вызывается для каждого готового выходного
    /// (0, 1 или несколько раз). Не выделяет память — можно звать из аудиопотока.
    #[inline]
    pub fn push(&mut self, sample: f32, mut out: impl FnMut(f32)) {
        if self.passthrough {
            out(sample);
            return;
        }
        while self.pos < 1.0 {
            out(self.prev + (sample - self.prev) * self.pos as f32);
            self.pos += self.step;
        }
        self.pos -= 1.0;
        self.prev = sample;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(from: u32, to: u32, n: usize) -> Vec<f32> {
        let mut r = Resampler::new(from, to);
        let mut out = Vec::new();
        for i in 0..n {
            r.push(i as f32, |s| out.push(s));
        }
        out
    }

    #[test]
    fn same_rate_is_passthrough() {
        assert_eq!(run(48_000, 48_000, 5), vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn output_length_follows_ratio() {
        let out = run(44_100, 48_000, 44_100);
        assert!((out.len() as i64 - 48_000).abs() <= 1, "{}", out.len());
        let out = run(48_000, 44_100, 48_000);
        assert!((out.len() as i64 - 44_100).abs() <= 1, "{}", out.len());
    }

    #[test]
    fn interpolates_linearly() {
        // Удвоение частоты: между соседними входными сэмплами появляется середина.
        let out = run(24_000, 48_000, 4);
        // Первые два выхода — разгон от начального нулевого сэмпла.
        assert_eq!(out, vec![0.0, 0.0, 0.0, 0.5, 1.0, 1.5, 2.0, 2.5]);
    }
}

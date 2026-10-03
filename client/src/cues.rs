//! Короткие звуки событий: кто-то зашёл или вышел, вы выключили микрофон и т.п.
//!
//! Звуки не хранятся в файлах, а синтезируются: пара нот с мягкой атакой и
//! затуханием. Их подмешивает микшер, поэтому они звучат в том же устройстве
//! вывода, что и голоса.

use vwf_proto::SAMPLE_RATE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Мы вошли в комнату.
    Connected,
    /// Кто-то вошёл.
    PeerJoined,
    /// Кто-то вышел.
    PeerLeft,
    Muted,
    Unmuted,
    Deafened,
    Undeafened,
}

// Ноты, Гц.
const G4: f32 = 392.00;
const C5: f32 = 523.25;
const E5: f32 = 659.25;
const G5: f32 = 783.99;

/// Громкость звуков (полная шкала — 1.0): заметно, но тише голоса.
const LEVEL: f32 = 0.18;

impl Cue {
    /// Ноты: (частота, длительность в мс).
    fn notes(self) -> &'static [(f32, u32)] {
        match self {
            Cue::Connected => &[(C5, 70), (E5, 70), (G5, 130)],
            Cue::PeerJoined => &[(C5, 80), (G5, 140)],
            Cue::PeerLeft => &[(G5, 80), (C5, 140)],
            Cue::Muted => &[(G4, 90)],
            Cue::Unmuted => &[(E5, 90)],
            Cue::Deafened => &[(C5, 70), (G4, 110)],
            Cue::Undeafened => &[(G4, 70), (C5, 110)],
        }
    }

    /// Сам звук: моно, 48 кГц.
    pub fn render(self) -> Vec<f32> {
        let rate = SAMPLE_RATE as f32;
        let attack = (rate * 0.005) as usize; // 5 мс
        let mut out = Vec::new();
        for &(freq, ms) in self.notes() {
            let len = (rate * ms as f32 / 1000.0) as usize;
            for i in 0..len {
                let t = i as f32 / rate;
                let phase = std::f32::consts::TAU * freq * t;
                // Чистый синус звучит «пищаще»; немного второй гармоники смягчает его.
                let tone = phase.sin() + 0.25 * (2.0 * phase).sin();
                let rise = (i as f32 / attack as f32).min(1.0);
                // Затухание до нуля к концу ноты: без щелчка на стыке нот.
                let fall = (1.0 - i as f32 / len as f32).powf(1.5);
                out.push(tone * rise * fall * LEVEL / 1.25);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_are_short_quiet_and_start_and_end_silently() {
        for cue in [
            Cue::Connected,
            Cue::PeerJoined,
            Cue::PeerLeft,
            Cue::Muted,
            Cue::Unmuted,
            Cue::Deafened,
            Cue::Undeafened,
        ] {
            let samples = cue.render();
            let ms = samples.len() as u32 * 1000 / SAMPLE_RATE;
            assert!((50..400).contains(&ms), "{cue:?}: {ms} мс");
            assert!(samples.iter().all(|s| s.abs() <= LEVEL), "{cue:?}: слишком громко");
            assert!(samples[0].abs() < 0.01 && samples.last().unwrap().abs() < 0.01, "{cue:?}");
        }
    }
}

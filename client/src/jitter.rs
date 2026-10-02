//! Джиттер-буфер: сглаживает неравномерность доставки пакетов.
//!
//! Отправитель шлёт кадр ровно каждые 20 мс, но по сети они приходят то пачкой,
//! то с опозданием, иногда не по порядку или не приходят вовсе. Буфер придерживает
//! пакеты и выдаёт строго по одному кадру на каждые 20 мс воспроизведения,
//! по порядку номеров.
//!
//! Сколько придерживать, буфер решает сам. Номер кадра `seq` у отправителя растёт
//! на 1 каждые 20 мс, даже в тишине, когда пакеты не отправляются. Поэтому
//! `время прихода − seq × 20 мс` — это задержка пакета в сети плюс постоянное
//! смещение часов. Разброс этой величины по последним пакетам и есть джиттер.
//! Начало фразы откладывается так, чтобы 95 % недавних пакетов успевали
//! к своему времени, плюс небольшой запас. Редкие выбросы (одна пачка
//! опоздавших пакетов) задержку не раздувают: такие пакеты заменит FEC/PLC.
//! В локальной сети задержка получается несколько миллисекунд, в плохом Wi-Fi — десятки.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use vwf_proto::FRAME_MS;

/// Если пропало больше кадров подряд, не пытаемся «замазывать» дыру,
/// а сразу прыгаем к ближайшему имеющемуся пакету.
const MAX_GAP: u32 = 25;
/// Жёсткий предел на число хранимых пакетов.
const CAPACITY: usize = 100;
/// По скольким последним пакетам оценивать джиттер (~5 с речи).
const HISTORY: usize = 250;
/// Больше этого задержку не наращиваем, даже если сеть совсем плохая.
const MAX_DELAY_MS: f64 = 300.0;
/// Какая доля недавних пакетов должна успевать вовремя.
const ON_TIME_QUANTILE: f64 = 0.95;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Delay {
    /// Подстраиваться под сеть: измеренный джиттер + запас `margin_ms`.
    Adaptive { margin_ms: f64 },
    /// Постоянная задержка, мс.
    Fixed(f64),
}

impl Default for Delay {
    fn default() -> Self {
        Delay::Adaptive { margin_ms: 10.0 }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pop {
    /// Есть пакет для этого кадра.
    Packet(Vec<u8>),
    /// Пакет потерян. Если пришёл следующий пакет — в нём может быть
    /// избыточная копия потерянного кадра (Opus in-band FEC).
    Lost { next: Option<Vec<u8>> },
    /// Этот участник сейчас молчит (или его фраза ещё не «созрела»).
    Idle,
}

pub struct JitterBuffer {
    packets: BTreeMap<u32, Vec<u8>>,
    next_seq: u32,
    /// Последний кадр, который уже проигран (или признан потерянным).
    /// Всё, что не новее него, пришло слишком поздно.
    last_played: Option<u32>,
    playing: bool,
    delay: Delay,
    /// Точка отсчёта времени для этого собеседника.
    epoch: Instant,
    /// `время прихода − seq × 20 мс` для последних пакетов, мс.
    transit: VecDeque<f64>,
    /// `(минимум, 95-й процентиль)` по `transit`, пересчитывается при каждом пакете.
    transit_range: (f64, f64),
}

impl JitterBuffer {
    pub fn new(delay: Delay, now: Instant) -> Self {
        Self {
            packets: BTreeMap::new(),
            next_seq: 0,
            last_played: None,
            playing: false,
            delay,
            epoch: now,
            transit: VecDeque::with_capacity(HISTORY),
            transit_range: (0.0, 0.0),
        }
    }

    pub fn push(&mut self, seq: u32, packet: Vec<u8>, now: Instant) {
        if self.transit.len() == HISTORY {
            self.transit.pop_front();
        }
        self.transit.push_back(self.ms_since_epoch(now) - seq_ms(seq));
        self.transit_range = quantile_range(&self.transit);

        if self.last_played.is_some_and(|last| seq <= last) {
            return; // опоздал: его время уже прошло
        }
        if self.packets.len() >= CAPACITY {
            self.packets.pop_first();
        }
        self.packets.insert(seq, packet);
    }

    /// Текущая задержка воспроизведения сверх минимальной сетевой, мс.
    pub fn delay_ms(&self) -> f64 {
        match self.delay {
            Delay::Fixed(ms) => ms,
            Delay::Adaptive { margin_ms } => {
                let (min, high) = self.transit_range;
                (high - min + margin_ms).min(MAX_DELAY_MS)
            }
        }
    }

    /// Выдаёт содержимое следующего 20-мс кадра.
    pub fn pop(&mut self, now: Instant) -> Pop {
        if !self.playing {
            let Some(first) = self.first_seq() else { return Pop::Idle };
            // Когда кадр `first` должен зазвучать: момент, когда он пришёл бы
            // при самой быстрой недавней доставке, плюс задержка буфера.
            let (min_transit, _) = self.transit_range;
            let play_at = seq_ms(first) + min_transit + self.delay_ms();
            if self.ms_since_epoch(now) < play_at {
                return Pop::Idle;
            }
            self.playing = true;
            self.next_seq = first;
        }

        // Буфер раздулся (пачка пакетов после задержки в сети, или часы отправителя
        // идут чуть быстрее наших): выкидываем лишнее старое, чтобы не копить задержку.
        // При медленном дрейфе часов это пропуск одного кадра раз в несколько минут.
        let max = (self.delay_ms() / FRAME_MS as f64).ceil() as usize + 3;
        if self.packets.len() > max {
            while self.packets.len() > max {
                self.packets.pop_first();
            }
            self.next_seq = self.next_seq.max(self.first_seq().expect("буфер не пуст"));
        }

        let seq = self.next_seq;
        if let Some(packet) = self.packets.remove(&seq) {
            self.mark_played(seq);
            return Pop::Packet(packet);
        }

        let Some(first) = self.first_seq() else {
            // Пакеты кончились: человек замолчал или сеть надолго пропала.
            // Следующую фразу снова начнём с выдержки `delay_ms`.
            self.playing = false;
            return Pop::Idle;
        };

        if first - seq > MAX_GAP {
            self.next_seq = first;
            return self.pop(now);
        }

        self.mark_played(seq);
        Pop::Lost { next: self.packets.get(&(seq + 1)).cloned() }
    }

    /// Сколько кадров лежит в буфере.
    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    fn ms_since_epoch(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.epoch).as_secs_f64() * 1000.0
    }

    fn first_seq(&self) -> Option<u32> {
        self.packets.first_key_value().map(|(s, _)| *s)
    }

    fn mark_played(&mut self, seq: u32) {
        self.last_played = Some(seq);
        self.next_seq = seq.wrapping_add(1);
    }
}

/// Минимум и 95-й процентиль.
fn quantile_range(values: &VecDeque<f64>) -> (f64, f64) {
    let mut sorted: Vec<f64> = values.iter().copied().collect();
    sorted.sort_by(f64::total_cmp);
    let Some(&min) = sorted.first() else { return (0.0, 0.0) };
    let high = ((sorted.len() - 1) as f64 * ON_TIME_QUANTILE).round() as usize;
    (min, sorted[high])
}

fn seq_ms(seq: u32) -> f64 {
    seq as f64 * FRAME_MS as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn p(seq: u32) -> Vec<u8> {
        vec![seq as u8]
    }

    /// Виртуальное время: `at(ms)` — момент через `ms` миллисекунд после старта.
    struct Clock(Instant);

    impl Clock {
        fn at(&self, ms: u64) -> Instant {
            self.0 + Duration::from_millis(ms)
        }
    }

    fn setup(delay: Delay) -> (JitterBuffer, Clock) {
        let start = Instant::now();
        (JitterBuffer::new(delay, start), Clock(start))
    }

    #[test]
    fn fixed_delay_holds_first_packet() {
        let (mut jb, t) = setup(Delay::Fixed(30.0));
        jb.push(0, p(0), t.at(0));
        assert_eq!(jb.pop(t.at(10)), Pop::Idle);
        assert_eq!(jb.pop(t.at(29)), Pop::Idle);
        assert_eq!(jb.pop(t.at(30)), Pop::Packet(p(0)));
        jb.push(1, p(1), t.at(20));
        assert_eq!(jb.pop(t.at(50)), Pop::Packet(p(1)));
        assert_eq!(jb.pop(t.at(70)), Pop::Idle);
    }

    #[test]
    fn adaptive_delay_follows_measured_jitter() {
        let (mut jb, t) = setup(Delay::Adaptive { margin_ms: 10.0 });
        // Ровная сеть: задержка — только запас.
        for seq in 0..10 {
            jb.push(seq, p(seq), t.at(seq as u64 * 20));
        }
        assert!((jb.delay_ms() - 10.0).abs() < 1e-6, "{}", jb.delay_ms());
        // Сеть стала неровной: каждый второй пакет опаздывает на 35 мс.
        for seq in 10..30 {
            let lag = if seq % 2 == 0 { 35 } else { 0 };
            jb.push(seq, p(seq), t.at(seq as u64 * 20 + lag));
        }
        assert!((jb.delay_ms() - 45.0).abs() < 1e-6, "{}", jb.delay_ms());
    }

    #[test]
    fn rare_outliers_do_not_inflate_delay() {
        let (mut jb, t) = setup(Delay::Adaptive { margin_ms: 10.0 });
        for seq in 0..100 {
            // Одна пачка: кадры 50..=52 застряли и пришли вместе на 60 мс позже.
            let at = if (50..=52).contains(&seq) { 52 * 20 + 60 } else { seq as u64 * 20 };
            jb.push(seq, p(seq), t.at(at));
        }
        assert!((jb.delay_ms() - 10.0).abs() < 1e-6, "{}", jb.delay_ms());
    }

    #[test]
    fn late_first_packet_does_not_add_delay() {
        let (mut jb, t) = setup(Delay::Adaptive { margin_ms: 10.0 });
        // История: задержки 0 и 30 мс вперемешку, 95-й процентиль — 30 мс.
        jb.push(0, p(0), t.at(0));
        jb.push(1, p(1), t.at(50));
        while jb.pop(t.at(100)) != Pop::Idle {}
        // Новая фраза. Задержка буфера: 30 + 10 = 40 мс сверх самой быстрой доставки,
        // то есть кадр 10 должен зазвучать в 10 × 20 + 0 + 40 = 240 мс.
        // Он сам пришёл «медленно» (в 230 мс), поэтому ждёт всего 10 мс, а не 40.
        jb.push(10, p(10), t.at(230));
        assert_eq!(jb.pop(t.at(239)), Pop::Idle);
        assert_eq!(jb.pop(t.at(240)), Pop::Packet(p(10)));
    }

    #[test]
    fn lost_packet_offers_next_for_fec() {
        let (mut jb, t) = setup(Delay::Fixed(0.0));
        jb.push(0, p(0), t.at(0));
        jb.push(2, p(2), t.at(40));
        assert_eq!(jb.pop(t.at(40)), Pop::Packet(p(0)));
        jb.push(3, p(3), t.at(60));
        assert_eq!(jb.pop(t.at(60)), Pop::Lost { next: Some(p(2)) });
        assert_eq!(jb.pop(t.at(80)), Pop::Packet(p(2)));
        assert_eq!(jb.pop(t.at(100)), Pop::Packet(p(3)));
    }

    #[test]
    fn late_packet_is_dropped() {
        let (mut jb, t) = setup(Delay::Fixed(0.0));
        jb.push(5, p(5), t.at(100));
        jb.push(7, p(7), t.at(140));
        assert_eq!(jb.pop(t.at(140)), Pop::Packet(p(5)));
        assert_eq!(jb.pop(t.at(160)), Pop::Lost { next: Some(p(7)) });
        jb.push(6, p(6), t.at(170)); // опоздал
        assert_eq!(jb.pop(t.at(180)), Pop::Packet(p(7)));
        assert_eq!(jb.pop(t.at(200)), Pop::Idle);
    }

    #[test]
    fn overfull_buffer_drops_oldest() {
        let (mut jb, t) = setup(Delay::Fixed(40.0));
        // 20 пакетов пришли одной пачкой после затора в сети.
        for s in 0..20 {
            jb.push(s, p(s), t.at(400));
        }
        // max = 40/20 + 3 = 5 кадров: остаются 15..=19.
        assert_eq!(jb.pop(t.at(400)), Pop::Packet(p(15)));
        assert_eq!(jb.pop(t.at(420)), Pop::Packet(p(16)));
    }

    #[test]
    fn big_gap_is_skipped() {
        let (mut jb, t) = setup(Delay::Fixed(0.0));
        jb.push(0, p(0), t.at(0));
        jb.push(1, p(1), t.at(20));
        assert_eq!(jb.pop(t.at(20)), Pop::Packet(p(0)));
        jb.push(100, p(100), t.at(30));
        assert_eq!(jb.pop(t.at(40)), Pop::Packet(p(1)));
        assert_eq!(jb.pop(t.at(60)), Pop::Packet(p(100)));
    }
}

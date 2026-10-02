//! Джиттер-буфер: сглаживает неравномерность доставки пакетов.
//!
//! Отправитель шлёт кадр ровно каждые 20 мс, но по сети они приходят то пачкой,
//! то с опозданием, иногда не по порядку или не приходят вовсе. Буфер копит
//! несколько кадров (`target`) перед началом проигрывания и потом выдаёт
//! строго по одному кадру на каждые 20 мс воспроизведения, по порядку номеров.
//!
//! Больше `target` — устойчивее к плохой сети, но больше задержка.

use std::collections::BTreeMap;

/// Если пропало больше кадров подряд, не пытаемся «замазывать» дыру,
/// а сразу прыгаем к ближайшему имеющемуся пакету.
const MAX_GAP: u32 = 25;
/// Жёсткий предел на число хранимых пакетов.
const CAPACITY: usize = 100;

#[derive(Debug, PartialEq, Eq)]
pub enum Pop {
    /// Есть пакет для этого кадра.
    Packet(Vec<u8>),
    /// Пакет потерян. Если пришёл следующий пакет — в нём может быть
    /// избыточная копия потерянного кадра (Opus in-band FEC).
    Lost { next: Option<Vec<u8>> },
    /// Этот участник сейчас молчит.
    Idle,
}

pub struct JitterBuffer {
    packets: BTreeMap<u32, Vec<u8>>,
    next_seq: u32,
    /// Последний кадр, который уже проигран (или признан потерянным).
    /// Всё, что не новее него, пришло слишком поздно.
    last_played: Option<u32>,
    playing: bool,
    target: usize,
    max: usize,
}

impl JitterBuffer {
    /// `target` — сколько кадров накопить перед началом проигрывания.
    pub fn new(target: usize) -> Self {
        let target = target.max(1);
        Self {
            packets: BTreeMap::new(),
            next_seq: 0,
            last_played: None,
            playing: false,
            target,
            // Запас сверху, после которого буфер начинает выбрасывать старые кадры.
            max: target + 5,
        }
    }

    pub fn push(&mut self, seq: u32, packet: Vec<u8>) {
        if self.last_played.is_some_and(|last| seq <= last) {
            return; // опоздал: его время уже прошло
        }
        if self.packets.len() >= CAPACITY {
            self.packets.pop_first();
        }
        self.packets.insert(seq, packet);
    }

    /// Выдаёт содержимое следующего 20-мс кадра.
    pub fn pop(&mut self) -> Pop {
        if !self.playing {
            if self.packets.len() < self.target {
                return Pop::Idle;
            }
            self.playing = true;
            self.next_seq = self.first_seq().expect("буфер не пуст");
        }

        // Буфер раздулся (пачка пакетов после задержки в сети, или часы отправителя
        // идут чуть быстрее наших): выкидываем лишнее старое, чтобы не копить задержку.
        // Только сверх `max`: при медленном дрейфе часов это пропуск одного кадра
        // раз в несколько минут, а не скачок сразу на 100 мс.
        if self.packets.len() > self.max {
            while self.packets.len() > self.max {
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
            // Следующий разговор снова начнём с накопления `target` кадров.
            self.playing = false;
            return Pop::Idle;
        };

        if first - seq > MAX_GAP {
            self.next_seq = first;
            return self.pop();
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

    fn first_seq(&self) -> Option<u32> {
        self.packets.first_key_value().map(|(s, _)| *s)
    }

    fn mark_played(&mut self, seq: u32) {
        self.last_played = Some(seq);
        self.next_seq = seq.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(seq: u32) -> Vec<u8> {
        vec![seq as u8]
    }

    #[test]
    fn waits_for_target_then_plays_in_order() {
        let mut jb = JitterBuffer::new(3);
        jb.push(10, p(10));
        jb.push(12, p(12));
        assert_eq!(jb.pop(), Pop::Idle);
        jb.push(11, p(11));
        assert_eq!(jb.pop(), Pop::Packet(p(10)));
        assert_eq!(jb.pop(), Pop::Packet(p(11)));
        assert_eq!(jb.pop(), Pop::Packet(p(12)));
        assert_eq!(jb.pop(), Pop::Idle);
    }

    #[test]
    fn lost_packet_offers_next_for_fec() {
        let mut jb = JitterBuffer::new(2);
        jb.push(0, p(0));
        jb.push(2, p(2));
        assert_eq!(jb.pop(), Pop::Packet(p(0)));
        jb.push(3, p(3));
        assert_eq!(jb.pop(), Pop::Lost { next: Some(p(2)) });
        assert_eq!(jb.pop(), Pop::Packet(p(2)));
        assert_eq!(jb.pop(), Pop::Packet(p(3)));
    }

    #[test]
    fn late_packet_is_dropped() {
        let mut jb = JitterBuffer::new(1);
        jb.push(5, p(5));
        jb.push(7, p(7));
        assert_eq!(jb.pop(), Pop::Packet(p(5)));
        assert_eq!(jb.pop(), Pop::Lost { next: Some(p(7)) });
        jb.push(6, p(6)); // опоздал
        assert_eq!(jb.pop(), Pop::Packet(p(7)));
        assert_eq!(jb.pop(), Pop::Idle);
    }

    #[test]
    fn rebuffers_after_silence() {
        let mut jb = JitterBuffer::new(2);
        jb.push(0, p(0));
        jb.push(1, p(1));
        assert_eq!(jb.pop(), Pop::Packet(p(0)));
        assert_eq!(jb.pop(), Pop::Packet(p(1)));
        assert_eq!(jb.pop(), Pop::Idle);
        jb.push(2, p(2));
        assert_eq!(jb.pop(), Pop::Idle); // снова копим 2 кадра
        jb.push(3, p(3));
        assert_eq!(jb.pop(), Pop::Packet(p(2)));
    }

    #[test]
    fn overfull_buffer_drops_oldest() {
        let mut jb = JitterBuffer::new(2);
        for s in 0..20 {
            jb.push(s, p(s));
        }
        // max = 7: остаются кадры 13..=19.
        assert_eq!(jb.pop(), Pop::Packet(p(13)));
        assert_eq!(jb.pop(), Pop::Packet(p(14)));
    }

    #[test]
    fn big_gap_is_skipped() {
        let mut jb = JitterBuffer::new(1);
        jb.push(0, p(0));
        assert_eq!(jb.pop(), Pop::Packet(p(0)));
        jb.push(100, p(100));
        assert_eq!(jb.pop(), Pop::Packet(p(100)));
    }
}

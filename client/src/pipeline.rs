//! Голосовой конвейер.
//!
//! Передача:  микрофон → [кольцевой буфер] → кадры по 20 мс → VAD → Opus → отправка
//! Приём:     пакет → джиттер-буфер участника → декодер Opus → микшер → [кольцевой буфер] → динамики
//!
//! Обе половины работают в своих потоках и общаются с аудиоколбэками
//! только через lock-free кольцевые буферы. Аудиоколбэк после каждой порции
//! звука будит свой поток (`Wakeup`), поэтому кадр кодируется сразу, как только
//! набрался, а микшер доливает звук сразу, как только карта его забрала.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context;
use ringbuf::traits::{Consumer, Observer, Producer};
use ringbuf::{HeapCons, HeapProd};
use vwf_proto::{FRAME_MS, FRAME_SAMPLES, PeerId, SAMPLE_RATE};

use nnnoiseless::DenoiseState;

use crate::audio::Wakeup;
use crate::jitter::{Delay, JitterBuffer, Pop};
use crate::resample::Resampler;

/// Рабочие потоки просыпаются по сигналу из аудиоколбэка; этот таймаут —
/// только страховка на случай, если сигнал не придёт (например, устройство отвалилось).
const WAKE_TIMEOUT: Duration = Duration::from_millis(20);
/// Максимальный размер Opus-пакета (с запасом; реальные — 60–150 байт).
const MAX_PACKET: usize = 1275;

// ---------------------------------------------------------------------------
// Передача
// ---------------------------------------------------------------------------

/// Когда передавать голос.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TxMode {
    /// Активация голосом: когда громкость выше порога.
    Voice = 0,
    /// Всегда, пока микрофон не выключен.
    Always = 1,
    /// Рация (push-to-talk): пока нажата клавиша.
    PushToTalk = 2,
}

impl TxMode {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => TxMode::Always,
            2 => TxMode::PushToTalk,
            _ => TxMode::Voice,
        }
    }
}

/// Настройки кодера, которые задаются при запуске.
#[derive(Clone, Debug)]
pub struct TxSettings {
    /// Битрейт Opus, бит/с. Для голоса 24–32 кбит/с — уже отличное качество.
    pub bitrate: i32,
}

impl Default for TxSettings {
    fn default() -> Self {
        Self { bitrate: 32_000 }
    }
}

/// Состояние передачи, которое меняют и читают другие потоки (интерфейс,
/// обработчик горячей клавиши). Живёт дольше одного звонка, поэтому
/// настройки сохраняются между подключениями.
pub struct TxState {
    muted: AtomicBool,
    mode: AtomicU8,
    vad_threshold_db_bits: AtomicU32,
    ptt_pressed: AtomicBool,
    noise_suppression: AtomicBool,
    /// Идёт ли сейчас передача (для индикатора).
    transmitting: AtomicBool,
    /// Громкость последнего кадра с микрофона, dBFS.
    level_db_bits: AtomicU32,
}

impl TxState {
    pub fn new(mode: TxMode, vad_threshold_db: f32) -> Self {
        Self {
            muted: AtomicBool::new(false),
            mode: AtomicU8::new(mode as u8),
            vad_threshold_db_bits: AtomicU32::new(vad_threshold_db.to_bits()),
            ptt_pressed: AtomicBool::new(false),
            noise_suppression: AtomicBool::new(true),
            transmitting: AtomicBool::new(false),
            level_db_bits: AtomicU32::new((-120f32).to_bits()),
        }
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn mode(&self) -> TxMode {
        TxMode::from_u8(self.mode.load(Ordering::Relaxed))
    }

    pub fn set_mode(&self, mode: TxMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }

    pub fn vad_threshold_db(&self) -> f32 {
        f32::from_bits(self.vad_threshold_db_bits.load(Ordering::Relaxed))
    }

    pub fn set_vad_threshold_db(&self, db: f32) {
        self.vad_threshold_db_bits.store(db.to_bits(), Ordering::Relaxed);
    }

    pub fn set_ptt_pressed(&self, pressed: bool) {
        self.ptt_pressed.store(pressed, Ordering::Relaxed);
    }

    pub fn noise_suppression(&self) -> bool {
        self.noise_suppression.load(Ordering::Relaxed)
    }

    pub fn set_noise_suppression(&self, enabled: bool) {
        self.noise_suppression.store(enabled, Ordering::Relaxed);
    }

    pub fn transmitting(&self) -> bool {
        self.transmitting.load(Ordering::Relaxed)
    }

    pub fn level_db(&self) -> f32 {
        f32::from_bits(self.level_db_bits.load(Ordering::Relaxed))
    }

    /// Решает, передавать ли кадр с громкостью `level_db`.
    fn should_transmit(&self, vad: &mut Vad, level_db: f32) -> bool {
        // Детектор речи обновляем всегда, чтобы при переключении режима
        // у него было актуальное состояние.
        let voice = vad.is_active(level_db, self.vad_threshold_db());
        if self.muted() {
            return false;
        }
        match self.mode() {
            TxMode::Voice => voice,
            TxMode::Always => true,
            TxMode::PushToTalk => self.ptt_pressed.load(Ordering::Relaxed),
        }
    }
}

impl Default for TxState {
    fn default() -> Self {
        Self::new(TxMode::Voice, -45.0)
    }
}

/// Шумоподавление RNNoise (крейт `nnnoiseless`, перенос на Rust).
///
/// Внутри небольшая рекуррентная нейросеть: по спектру каждого 10-мс кусочка
/// она решает, сколько в каждой полосе частот голоса, а сколько шума, и
/// приглушает полосы с шумом. Хорошо убирает гул вентилятора, шум улицы,
/// стук клавиатуры. Добавляет около 10 мс задержки и немного нагружает процессор.
struct NoiseSuppressor {
    state: Box<DenoiseState<'static>>,
    input: [f32; DenoiseState::FRAME_SIZE],
    output: [f32; DenoiseState::FRAME_SIZE],
}

// Кадр Opus (20 мс) должен состоять из целого числа кадров RNNoise (10 мс).
const _: () = assert!(FRAME_SAMPLES.is_multiple_of(DenoiseState::FRAME_SIZE));

impl NoiseSuppressor {
    fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input: [0.0; DenoiseState::FRAME_SIZE],
            output: [0.0; DenoiseState::FRAME_SIZE],
        }
    }

    fn process(&mut self, frame: &mut [f32; FRAME_SAMPLES]) {
        for chunk in frame.chunks_exact_mut(DenoiseState::FRAME_SIZE) {
            // RNNoise ждёт сэмплы в масштабе 16-битного звука, а не [-1, 1].
            for (dst, s) in self.input.iter_mut().zip(chunk.iter()) {
                *dst = s * 32768.0;
            }
            self.state.process_frame(&mut self.output, &self.input);
            for (s, out) in chunk.iter_mut().zip(self.output.iter()) {
                *s = out / 32768.0;
            }
        }
    }
}

/// Простейший детектор речи по громкости с «хвостом»: после того как голос
/// стих, передача продолжается ещё `HANGOVER_FRAMES` кадров, чтобы не обрезать
/// окончания слов.
#[derive(Default)]
struct Vad {
    hangover_left: u32,
}

const HANGOVER_FRAMES: u32 = 300 / FRAME_MS; // 300 мс

impl Vad {
    fn is_active(&mut self, level_db: f32, threshold_db: f32) -> bool {
        if level_db >= threshold_db {
            self.hangover_left = HANGOVER_FRAMES;
            true
        } else if self.hangover_left > 0 {
            self.hangover_left -= 1;
            true
        } else {
            false
        }
    }
}

fn level_db(frame: &[f32]) -> f32 {
    let mean_square = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
    10.0 * mean_square.max(1e-12).log10()
}

pub struct Transmitter {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Номер следующего кадра — чтобы новый кодер (после смены микрофона)
    /// продолжил нумерацию, а не начал с нуля.
    next_seq: Arc<AtomicU32>,
}

impl Transmitter {
    /// Запускает поток кодирования. `send(seq, opus)` вызывается для каждого
    /// готового пакета. `seq` — номер 20-мс кадра: он растёт с каждым кадром
    /// с микрофона, даже если кадр не отправлен (тишина, mute). Так получатель
    /// по номеру знает, когда кадр был записан, и может измерять джиттер.
    /// `wakeup` — тот же, что передан в `audio::start_capture`; `first_seq` —
    /// номер первого кадра.
    pub fn spawn(
        mut capture: HeapCons<f32>,
        wakeup: Arc<Wakeup>,
        state: Arc<TxState>,
        settings: TxSettings,
        first_seq: u32,
        mut send: impl FnMut(u32, &[u8]) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let mut encoder =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)
                .context("не удалось создать кодер Opus")?;
        encoder.set_bitrate(opus::Bitrate::Bits(settings.bitrate))?;
        // In-band FEC: в каждый пакет добавляется сжатая копия предыдущего кадра,
        // и одиночную потерю получатель может восстановить по следующему пакету.
        encoder.set_inband_fec(true)?;
        encoder.set_packet_loss_perc(10)?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let next_seq = Arc::new(AtomicU32::new(first_seq));
        let thread_next_seq = next_seq.clone();
        let thread = thread::Builder::new().name("vwf-encoder".into()).spawn(move || {
            let mut vad = Vad::default();
            let mut denoiser = NoiseSuppressor::new();
            let mut frame = [0f32; FRAME_SAMPLES];
            let mut packet = [0u8; MAX_PACKET];
            let next_seq = thread_next_seq;
            wakeup.register();
            // Пока запускались вывод и микшер, микрофон уже что-то записал.
            // Это старый звук: отправлять его — значит начать разговор с задержкой.
            capture.clear();
            while !thread_stop.load(Ordering::Relaxed) {
                if capture.occupied_len() < FRAME_SAMPLES {
                    wakeup.wait(WAKE_TIMEOUT);
                    continue;
                }
                capture.pop_slice(&mut frame);
                let seq = next_seq.fetch_add(1, Ordering::Relaxed);

                // Шум убираем до детектора речи: тогда гул комнаты не включает
                // передачу, и индикатор громкости показывает то, что услышат другие.
                if state.noise_suppression() {
                    denoiser.process(&mut frame);
                }
                let level = level_db(&frame);
                state.level_db_bits.store(level.to_bits(), Ordering::Relaxed);
                let active = state.should_transmit(&mut vad, level);
                state.transmitting.store(active, Ordering::Relaxed);
                if !active {
                    continue;
                }

                match encoder.encode_float(&frame, &mut packet) {
                    Ok(len) => send(seq, &packet[..len]),
                    Err(e) => eprintln!("ошибка кодирования Opus: {e}"),
                }
            }
            state.transmitting.store(false, Ordering::Relaxed);
        })?;
        Ok(Self { stop, thread: Some(thread), next_seq })
    }

    /// Останавливает кодер и возвращает номер кадра, который был бы следующим.
    pub fn stop(&mut self) -> u32 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.next_seq.load(Ordering::Relaxed)
    }
}

impl Drop for Transmitter {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Приём и микширование
// ---------------------------------------------------------------------------

/// Длительность плавного нарастания звука в начале фразы.
const FADE_IN_SAMPLES: usize = (SAMPLE_RATE / 1000 * 5) as usize; // 5 мс

/// Всё, что относится к одному собеседнику: свой джиттер-буфер и свой декодер
/// (декодер Opus хранит состояние между кадрами, его нельзя делить между людьми).
struct Speaker {
    jitter: JitterBuffer,
    decoder: opus::Decoder,
    /// Играл ли звук этого собеседника в прошлом кадре.
    active: bool,
    /// До какого момента считать, что человек говорит (для индикатора).
    speaking_until: Option<Instant>,
}

/// Громче этого кадр считается речью (для индикатора «говорит»).
const SPEAKING_THRESHOLD_DB: f32 = -50.0;
/// Индикатор «говорит» гаснет не сразу, а через это время после последнего слова,
/// иначе он мигал бы на каждой паузе между словами.
const SPEAKING_HOLD: Duration = Duration::from_millis(250);

impl Speaker {
    /// Декодирует следующий кадр в `out`. `false` — человек молчит.
    ///
    /// Резкий переход между тишиной и звуком слышен как щелчок, поэтому начало
    /// фразы плавно нарастает, а в конце фразы декодер «договаривает» один кадр
    /// (PLC), который плавно затухает до нуля.
    fn next_frame(&mut self, out: &mut [f32; FRAME_SAMPLES], now: Instant) -> bool {
        let result = match self.jitter.pop(now) {
            Pop::Idle => {
                if !std::mem::take(&mut self.active) {
                    return false;
                }
                if self.decoder.decode_float(&[], out, false).is_err() {
                    return false;
                }
                let n = out.len() as f32;
                for (i, s) in out.iter_mut().enumerate() {
                    *s *= 1.0 - i as f32 / n;
                }
                return true;
            }
            Pop::Packet(p) => self.decoder.decode_float(&p, out, false),
            // Восстанавливаем потерянный кадр из FEC-копии в следующем пакете...
            Pop::Lost { next: Some(next) } => self.decoder.decode_float(&next, out, true),
            // ...или, если его нет, просим декодер «додумать» звук (packet loss concealment).
            Pop::Lost { next: None } => self.decoder.decode_float(&[], out, false),
        };
        if let Err(e) = result {
            eprintln!("ошибка декодирования Opus: {e}");
            return false;
        }
        if !std::mem::replace(&mut self.active, true) {
            for (i, s) in out.iter_mut().take(FADE_IN_SAMPLES).enumerate() {
                *s *= i as f32 / FADE_IN_SAMPLES as f32;
            }
        }
        true
    }
}

/// Принимает пакеты от всех собеседников и сводит их в один поток.
pub struct Mixer {
    speakers: Mutex<HashMap<PeerId, Speaker>>,
    /// Громкость каждого собеседника (1.0 — как есть). Хранится отдельно от
    /// `speakers`, чтобы её можно было задать ещё до того, как человек заговорил.
    volumes: Mutex<HashMap<PeerId, f32>>,
    /// «Выключить звук»: ничего не выводить, но продолжать принимать пакеты.
    deafened: AtomicBool,
    delay: Delay,
}

impl Mixer {
    /// `delay` — как джиттер-буферы выбирают задержку.
    pub fn new(delay: Delay) -> Self {
        Self {
            speakers: Mutex::new(HashMap::new()),
            volumes: Mutex::new(HashMap::new()),
            deafened: AtomicBool::new(false),
            delay,
        }
    }

    /// Кладёт пришедший голосовой пакет в джиттер-буфер отправителя.
    pub fn push_packet(&self, from: PeerId, seq: u32, opus: &[u8]) {
        self.push_packet_at(from, seq, opus, Instant::now());
    }

    fn push_packet_at(&self, from: PeerId, seq: u32, opus: &[u8], now: Instant) {
        let mut speakers = self.speakers.lock().unwrap();
        let speaker = match speakers.entry(from) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                match opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono) {
                    Ok(decoder) => e.insert(Speaker {
                        jitter: JitterBuffer::new(self.delay, now),
                        decoder,
                        active: false,
                        speaking_until: None,
                    }),
                    Err(err) => {
                        eprintln!("не удалось создать декодер Opus: {err}");
                        return;
                    }
                }
            }
        };
        speaker.jitter.push(seq, opus.to_vec(), now);
    }

    /// Собеседник ушёл — забываем его состояние.
    pub fn remove_speaker(&self, id: PeerId) {
        self.speakers.lock().unwrap().remove(&id);
        self.volumes.lock().unwrap().remove(&id);
    }

    /// Громкость конкретного собеседника (1.0 — как есть, 0 — не слышно).
    pub fn set_volume(&self, id: PeerId, volume: f32) {
        self.volumes.lock().unwrap().insert(id, volume.clamp(0.0, 4.0));
    }

    pub fn set_deafened(&self, deafened: bool) {
        self.deafened.store(deafened, Ordering::Relaxed);
    }

    pub fn deafened(&self) -> bool {
        self.deafened.load(Ordering::Relaxed)
    }

    /// Кто сейчас говорит.
    pub fn speaking(&self) -> Vec<PeerId> {
        let now = Instant::now();
        let speakers = self.speakers.lock().unwrap();
        speakers
            .iter()
            .filter(|(_, s)| s.speaking_until.is_some_and(|until| until > now))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Текущая задержка джиттер-буфера каждого собеседника, мс.
    pub fn delays_ms(&self) -> Vec<(PeerId, f64)> {
        let speakers = self.speakers.lock().unwrap();
        speakers.iter().map(|(id, s)| (*id, s.jitter.delay_ms())).collect()
    }

    /// Сводит по одному кадру от каждого собеседника.
    pub fn mix_frame(&self, out: &mut [f32; FRAME_SAMPLES]) {
        self.mix_frame_at(out, Instant::now());
    }

    fn mix_frame_at(&self, out: &mut [f32; FRAME_SAMPLES], now: Instant) {
        out.fill(0.0);
        let mut voice = [0f32; FRAME_SAMPLES];
        let volumes = self.volumes.lock().unwrap().clone();
        let deafened = self.deafened();
        let mut speakers = self.speakers.lock().unwrap();
        for (id, speaker) in speakers.iter_mut() {
            // Кадры достаём и при выключенном звуке: иначе джиттер-буфер
            // переполнится, а после включения зазвучит старое.
            if !speaker.next_frame(&mut voice, now) {
                continue;
            }
            if level_db(&voice) > SPEAKING_THRESHOLD_DB {
                speaker.speaking_until = Some(now + SPEAKING_HOLD);
            }
            if deafened {
                continue;
            }
            let volume = volumes.get(id).copied().unwrap_or(1.0);
            for (o, v) in out.iter_mut().zip(voice.iter()) {
                *o += v * volume;
            }
        }
        // Если несколько человек кричат одновременно, сумма может выйти за [-1, 1].
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
    }

    /// Запускает поток, который поддерживает буфер воспроизведения заполненным.
    ///
    /// Темп задаёт сама звуковая карта: новый кадр микшируется, только когда
    /// она «съела» достаточно звука. Поэтому часы микшера никогда не расходятся
    /// с часами карты, а расхождение с часами собеседника гасит джиттер-буфер.
    ///
    /// `lead_ms` — сколько звука держать готовым для карты. Меньше — меньше
    /// задержка, но если поток микшера не успеет проснуться, будет щелчок
    /// (его покажет счётчик сбоев вывода). `wakeup` — тот же, что передан
    /// в `audio::start_playback`.
    pub fn spawn_output(
        self: &Arc<Self>,
        mut playback: HeapProd<f32>,
        device_rate: u32,
        wakeup: Arc<Wakeup>,
        lead_ms: u32,
    ) -> anyhow::Result<MixerOutput> {
        let mixer = self.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let lead = (device_rate * lead_ms / 1000) as usize;
        let thread = thread::Builder::new().name("vwf-mixer".into()).spawn(move || {
            let mut resampler = Resampler::new(SAMPLE_RATE, device_rate);
            let mut frame = [0f32; FRAME_SAMPLES];
            wakeup.register();
            while !thread_stop.load(Ordering::Relaxed) {
                while playback.occupied_len() < lead {
                    mixer.mix_frame(&mut frame);
                    for &s in &frame {
                        resampler.push(s, |x| {
                            let _ = playback.try_push(x);
                        });
                    }
                }
                wakeup.wait(WAKE_TIMEOUT);
            }
        })?;
        Ok(MixerOutput { stop, thread: Some(thread) })
    }
}

/// Работающий поток микшера; останавливается при удалении.
pub struct MixerOutput {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MixerOutput {
    /// Останавливает поток микшера (повторный вызов ничего не делает).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for MixerOutput {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_frame(start: usize) -> [f32; FRAME_SAMPLES] {
        std::array::from_fn(|i| {
            let t = (start + i) as f32 / SAMPLE_RATE as f32;
            0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
        })
    }

    #[test]
    fn vad_holds_after_speech() {
        let mut vad = Vad::default();
        assert!(!vad.is_active(-60.0, -40.0));
        assert!(vad.is_active(-20.0, -40.0));
        for _ in 0..HANGOVER_FRAMES {
            assert!(vad.is_active(-60.0, -40.0));
        }
        assert!(!vad.is_active(-60.0, -40.0));
    }

    #[test]
    fn noise_suppressor_removes_fan_noise() {
        // «Гул вентилятора»: белый шум через простой фильтр нижних частот,
        // около -37 dBFS. Синтетическое белое шипение RNNoise глушит плохо —
        // он обучен на реальных шумах, — поэтому тестируем на похожем на них.
        let mut seed = 0x1234_5678_9abc_def1u64;
        let mut low = 0f32;
        let mut fan = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let white = (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
            low = 0.98 * low + 0.02 * white;
            low * 0.5
        };
        let mut ns = NoiseSuppressor::new();
        let mut before = Vec::new();
        let mut after = Vec::new();
        for i in 0..100 {
            let mut frame: [f32; FRAME_SAMPLES] = std::array::from_fn(|_| fan());
            let input_level = level_db(&frame);
            ns.process(&mut frame);
            // Первые кадры — разгон нейросети.
            if i >= 20 {
                before.push(input_level);
                after.push(level_db(&frame));
            }
        }
        let avg = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let reduction = avg(&before) - avg(&after);
        assert!(reduction > 20.0, "шум ослаблен всего на {reduction:.1} дБ");
    }

    #[test]
    fn transmit_modes() {
        let state = TxState::new(TxMode::Voice, -40.0);
        let mut vad = Vad::default();
        assert!(!state.should_transmit(&mut vad, -60.0));
        assert!(state.should_transmit(&mut vad, -20.0));

        state.set_mode(TxMode::PushToTalk);
        assert!(!state.should_transmit(&mut vad, -20.0));
        state.set_ptt_pressed(true);
        assert!(state.should_transmit(&mut vad, -90.0));

        state.set_mode(TxMode::Always);
        assert!(state.should_transmit(&mut vad, -90.0));
        state.set_muted(true);
        assert!(!state.should_transmit(&mut vad, -20.0));
    }

    /// После смены микрофона новый кодер продолжает нумерацию кадров старого:
    /// с нуля собеседники отбросили бы все пакеты как давно сыгранные.
    #[test]
    fn transmitter_continues_frame_numbers() {
        use ringbuf::HeapRb;
        use ringbuf::traits::Split;

        let state = Arc::new(TxState::new(TxMode::Always, -40.0));
        state.set_noise_suppression(false);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut first_seq = 0;
        for round in 0..2 {
            let (mut mic, capture) = HeapRb::<f32>::new(FRAME_SAMPLES * 8).split();
            let wakeup = Arc::new(Wakeup::default());
            let log = sent.clone();
            let mut tx = Transmitter::spawn(
                capture,
                wakeup.clone(),
                state.clone(),
                TxSettings::default(),
                first_seq,
                move |seq, _| log.lock().unwrap().push(seq),
            )
            .unwrap();
            // Кодер при запуске выбрасывает накопившийся звук, поэтому кадры
            // подаём, когда он уже работает.
            let want = (round + 1) * 3;
            let deadline = Instant::now() + Duration::from_secs(5);
            while sent.lock().unwrap().len() < want {
                assert!(Instant::now() < deadline, "кодер не отправил кадры");
                if mic.occupied_len() < FRAME_SAMPLES {
                    mic.push_slice(&sine_frame(0));
                    wakeup.wake();
                }
                thread::sleep(Duration::from_millis(5));
            }
            first_seq = tx.stop();
            assert_eq!(first_seq as usize, sent.lock().unwrap().len());
        }
        let sent = sent.lock().unwrap();
        assert_eq!(*sent, (0..sent.len() as u32).collect::<Vec<_>>());
    }

    #[test]
    fn level_of_full_scale_sine_is_about_minus_3_db() {
        let frame: Vec<f32> = sine_frame(0).iter().map(|s| s * 2.0).collect();
        assert!((level_db(&frame) + 3.0).abs() < 0.2);
    }

    /// Кодируем синус, теряем один пакет и проверяем, что на выходе микшера
    /// звук есть везде — потеря закрыта FEC/PLC, а не тишиной.
    #[test]
    fn opus_through_mixer_survives_packet_loss() {
        let mut encoder =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        encoder.set_inband_fec(true).unwrap();
        encoder.set_packet_loss_perc(10).unwrap();

        // Пакеты приходят по одному на каждый 20-мс кадр воспроизведения,
        // как в реальной жизни (время виртуальное); пакет 10 теряется.
        let mixer = Mixer::new(Delay::Fixed(40.0));
        let start = Instant::now();
        let at = |frame: u32| start + Duration::from_millis(frame as u64 * FRAME_MS as u64);
        let mut out = [0f32; FRAME_SAMPLES];
        for seq in 0..30u32 {
            let packet = encoder
                .encode_vec_float(&sine_frame(seq as usize * FRAME_SAMPLES), MAX_PACKET)
                .unwrap();
            if seq != 10 {
                mixer.push_packet_at(1, seq, &packet, at(seq));
            }
            mixer.mix_frame_at(&mut out, at(seq));
            let level = level_db(&out);
            match seq {
                // Задержка 40 мс — первые два кадра играет тишина.
                0 | 1 => assert_eq!(level, -120.0, "кадр {seq}"),
                _ => assert!(level > -20.0, "кадр {seq}: {level} dBFS"),
            }
        }
        // Буфер доигрывает остаток и затухающий кадр, потом снова тишина.
        for frame in 30..33 {
            mixer.mix_frame_at(&mut out, at(frame));
            assert!(level_db(&out) > -40.0);
        }
        mixer.mix_frame_at(&mut out, at(33));
        assert!(out.iter().all(|s| *s == 0.0), "после конца речи — тишина");
    }

    #[test]
    fn volume_and_deafen() {
        let mut encoder =
            opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let mixer = Mixer::new(Delay::Fixed(0.0));
        let start = Instant::now();
        let at = |frame: u32| start + Duration::from_millis(frame as u64 * FRAME_MS as u64);
        let mut out = [0f32; FRAME_SAMPLES];
        let mut levels = Vec::new();
        for seq in 0..30u32 {
            match seq {
                10 => mixer.set_volume(1, 0.5),
                20 => mixer.set_deafened(true),
                _ => {}
            }
            let packet = encoder
                .encode_vec_float(&sine_frame(seq as usize * FRAME_SAMPLES), MAX_PACKET)
                .unwrap();
            mixer.push_packet_at(1, seq, &packet, at(seq));
            mixer.mix_frame_at(&mut out, at(seq));
            levels.push(level_db(&out));
        }
        // Половина громкости — минус 6 дБ.
        assert!((levels[9] - levels[15] - 6.0).abs() < 1.0, "{levels:?}");
        // Звук выключен — тишина, но собеседник по-прежнему «говорит».
        assert_eq!(levels[25], -120.0);
        assert_eq!(mixer.speaking(), vec![1]);
    }
}

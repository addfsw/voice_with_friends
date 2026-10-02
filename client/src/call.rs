//! Всё вместе: звуковые устройства, кодер, микшер и соединение с сервером.
//! Этим пользуются и консольный клиент, и графический.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vwf_proto::{PeerId, PeerInfo, ServerMsg};

use crate::audio::{self, Wakeup};
use crate::jitter::Delay;
use crate::net;
use crate::pipeline::{Mixer, MixerOutput, Transmitter, TxSettings, TxState};

/// Параметры звука.
#[derive(Clone, Debug)]
pub struct AudioSettings {
    /// Микрофон (часть имени); `None` — системный по умолчанию.
    pub input: Option<String>,
    /// Устройство вывода (часть имени); `None` — системное по умолчанию.
    pub output: Option<String>,
    pub tx: TxSettings,
    pub jitter: Delay,
    /// Сколько звука держать готовым для звуковой карты, мс.
    pub playback_ms: u32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            tx: TxSettings::default(),
            jitter: Delay::default(),
            playback_ms: 20,
        }
    }
}

/// Запущенные микрофон, динамики, микшер и кодер.
pub struct AudioEngine {
    pub capture: audio::Capture,
    pub playback: audio::Playback,
    pub mixer: Arc<Mixer>,
    pub tx_state: Arc<TxState>,
    // Порядок полей важен: сначала останавливаем потоки, потом устройства.
    _tx: Transmitter,
    _output: MixerOutput,
}

impl AudioEngine {
    /// Открывает устройства и запускает кодер; `send(seq, opus)` получает пакеты.
    pub fn start(
        settings: &AudioSettings,
        tx_state: Arc<TxState>,
        send: impl FnMut(u32, &[u8]) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let mic_wakeup = Arc::new(Wakeup::default());
        let speaker_wakeup = Arc::new(Wakeup::default());
        let (capture, mic) = audio::start_capture(settings.input.as_deref(), mic_wakeup.clone())?;
        let (playback, speakers) =
            audio::start_playback(settings.output.as_deref(), speaker_wakeup.clone())?;

        let mixer = Arc::new(Mixer::new(settings.jitter));
        let output = mixer.spawn_output(
            speakers,
            playback.config.sample_rate,
            speaker_wakeup,
            settings.playback_ms.clamp(5, 200),
        )?;
        let tx = Transmitter::spawn(mic, mic_wakeup, tx_state.clone(), settings.tx.clone(), send)?;
        Ok(Self { capture, playback, mixer, tx_state, _tx: tx, _output: output })
    }
}

/// Что происходит в комнате.
#[derive(Debug, Clone)]
pub enum CallEvent {
    PeerJoined(PeerInfo),
    PeerLeft(PeerId),
    /// Соединение закрылось не по нашей инициативе.
    Ended(String),
}

/// Активный звонок.
pub struct Call {
    pub my_id: PeerId,
    pub audio: AudioEngine,
    peers: Arc<Mutex<HashMap<PeerId, String>>>,
    closer: net::Closer,
    hanging_up: Arc<AtomicBool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Call {
    /// Подключается к серверу и запускает звук. События комнаты передаются в `on_event`.
    pub async fn start(
        server: &str,
        name: &str,
        settings: &AudioSettings,
        tx_state: Arc<TxState>,
        on_event: impl Fn(CallEvent) + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let mut session = net::connect(server, name).await?;
        let my_id = session.my_id;
        let peers: HashMap<PeerId, String> =
            session.peers.iter().map(|p| (p.id, p.name.clone())).collect();
        let peers = Arc::new(Mutex::new(peers));

        let sender = session.voice_sender();
        let audio =
            AudioEngine::start(settings, tx_state, move |seq, opus| sender.send(seq, opus))?;

        let receiver = session.voice_receiver();
        let rx_mixer = audio.mixer.clone();
        let voice_task = tokio::spawn(async move {
            while let Some((from, seq, opus)) = receiver.recv().await {
                rx_mixer.push_packet(from, seq, &opus);
            }
        });

        let closer = session.closer();
        let hanging_up = Arc::new(AtomicBool::new(false));
        let control_task = {
            let mixer = audio.mixer.clone();
            let peers = peers.clone();
            let hanging_up = hanging_up.clone();
            tokio::spawn(async move {
                let reason = loop {
                    match session.next_event().await {
                        Ok(Some(ServerMsg::PeerJoined(peer))) => {
                            peers.lock().unwrap().insert(peer.id, peer.name.clone());
                            on_event(CallEvent::PeerJoined(peer));
                        }
                        Ok(Some(ServerMsg::PeerLeft { id })) => {
                            peers.lock().unwrap().remove(&id);
                            mixer.remove_speaker(id);
                            on_event(CallEvent::PeerLeft(id));
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => break "сервер закрыл соединение".to_string(),
                        Err(e) => break format!("соединение потеряно: {e:#}"),
                    }
                };
                if !hanging_up.load(Ordering::Relaxed) {
                    on_event(CallEvent::Ended(reason));
                }
            })
        };

        Ok(Self { my_id, audio, peers, closer, hanging_up, tasks: vec![voice_task, control_task] })
    }

    /// Кто сейчас в комнате (кроме нас).
    pub fn peers(&self) -> Vec<PeerInfo> {
        let mut peers: Vec<PeerInfo> = self
            .peers
            .lock()
            .unwrap()
            .iter()
            .map(|(id, name)| PeerInfo { id: *id, name: name.clone() })
            .collect();
        peers.sort_by_key(|p| p.id);
        peers
    }

    pub fn mixer(&self) -> &Arc<Mixer> {
        &self.audio.mixer
    }

    /// Завершает звонок.
    pub async fn hang_up(self) {
        self.hanging_up.store(true, Ordering::Relaxed);
        self.closer.close().await;
        for task in &self.tasks {
            task.abort();
        }
    }
}

// Звонок хранится в общем состоянии графического приложения и переходит между
// потоками, поэтому он обязан быть `Send`. Проверяем это при компиляции.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Call>();
};

//! Всё вместе: звуковые устройства, кодер, микшер и соединение с сервером.
//! Этим пользуются и консольный клиент, и графический.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vwf_proto::{ClientMsg, PeerId, PeerInfo, ServerMsg};

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
    /// Изменился статус участника или наш собственный (`id` совпадает с `Call::me`).
    PeerUpdated(PeerInfo),
    /// Соединение закрылось не по нашей инициативе.
    Ended(String),
}

/// Состояние комнаты, которое обновляет задача управляющего канала.
struct Room {
    me: PeerInfo,
    peers: HashMap<PeerId, PeerInfo>,
}

/// Активный звонок.
pub struct Call {
    pub my_id: PeerId,
    /// Отпечаток сертификата сервера.
    pub fingerprint: String,
    pub audio: AudioEngine,
    room: Arc<Mutex<Room>>,
    control: net::Control,
    closer: net::Closer,
    hanging_up: Arc<AtomicBool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Call {
    /// Подключается к серверу и запускает звук. События комнаты передаются в `on_event`.
    /// `admin_token` — секрет хоста, если мы им являемся; `trust` — как проверять
    /// сертификат сервера.
    pub async fn start(
        server: &str,
        name: &str,
        admin_token: Option<&str>,
        trust: net::Trust<'_>,
        settings: &AudioSettings,
        tx_state: Arc<TxState>,
        on_event: impl Fn(CallEvent) + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let mut session = net::connect_with(server, name, admin_token, trust).await?;
        let my_id = session.me.id;
        let fingerprint = session.fingerprint.clone();
        let room = Arc::new(Mutex::new(Room {
            me: session.me.clone(),
            peers: session.peers.iter().map(|p| (p.id, p.clone())).collect(),
        }));

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

        let control = session.control();
        let closer = session.closer();
        let hanging_up = Arc::new(AtomicBool::new(false));
        let control_task = {
            let mixer = audio.mixer.clone();
            let room = room.clone();
            let hanging_up = hanging_up.clone();
            let closer = closer.clone();
            tokio::spawn(async move {
                let reason = loop {
                    match session.next_event().await {
                        Ok(Some(ServerMsg::PeerJoined(peer))) => {
                            room.lock().unwrap().peers.insert(peer.id, peer.clone());
                            on_event(CallEvent::PeerJoined(peer));
                        }
                        Ok(Some(ServerMsg::PeerLeft { id })) => {
                            room.lock().unwrap().peers.remove(&id);
                            mixer.remove_speaker(id);
                            on_event(CallEvent::PeerLeft(id));
                        }
                        Ok(Some(ServerMsg::PeerUpdated(peer))) => {
                            {
                                let mut room = room.lock().unwrap();
                                if peer.id == my_id {
                                    room.me = peer.clone();
                                } else {
                                    room.peers.insert(peer.id, peer.clone());
                                }
                            }
                            on_event(CallEvent::PeerUpdated(peer));
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => break "сервер закрыл соединение".to_string(),
                        Err(e) => break format!("соединение потеряно: {e:#}"),
                    }
                };
                // Если сервер сам назвал причину (например, «сервер остановлен»),
                // она понятнее технического текста ошибки.
                let reason = closer.close_reason().unwrap_or(reason);
                if !hanging_up.load(Ordering::Relaxed) {
                    on_event(CallEvent::Ended(reason));
                }
            })
        };

        let call = Self {
            my_id,
            fingerprint,
            audio,
            room,
            control,
            closer,
            hanging_up,
            tasks: vec![voice_task, control_task],
        };
        call.sync_status();
        Ok(call)
    }

    /// Мы сами, как нас видит сервер.
    pub fn me(&self) -> PeerInfo {
        self.room.lock().unwrap().me.clone()
    }

    /// Кто сейчас в комнате (кроме нас).
    pub fn peers(&self) -> Vec<PeerInfo> {
        let mut peers: Vec<PeerInfo> = self.room.lock().unwrap().peers.values().cloned().collect();
        peers.sort_by_key(|p| p.id);
        peers
    }

    pub fn mixer(&self) -> &Arc<Mixer> {
        &self.audio.mixer
    }

    /// Сообщает остальным, выключены ли у нас микрофон и звук.
    /// Вызывать после каждого их переключения.
    pub fn sync_status(&self) {
        self.control.send(ClientMsg::SetStatus {
            muted: self.audio.tx_state.muted(),
            deafened: self.audio.mixer.deafened(),
        });
    }

    /// Только для хоста: заглушить участника для всех (или вернуть ему голос).
    pub fn set_server_mute(&self, id: PeerId, muted: bool) {
        self.control.send(ClientMsg::SetServerMute { id, muted });
    }

    /// Только для хоста: отключить участника от комнаты.
    pub fn kick(&self, id: PeerId) {
        self.control.send(ClientMsg::Kick { id });
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

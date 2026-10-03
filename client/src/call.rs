//! Всё вместе: звуковые устройства, кодер, микшер и соединение с сервером.
//! Этим пользуются и консольный клиент, и графический.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vwf_proto::{ChatMessage, ClientMsg, PeerId, PeerInfo, ServerMsg};

use crate::audio::{self, Wakeup};
use crate::cues::Cue;
use crate::jitter::Delay;
use crate::net;
use ringbuf::{HeapCons, HeapProd};

use crate::pipeline::{Mixer, MixerOutput, Transmitter, TxSettings, TxState};
use crate::screen::{ShareSettings, Sharer, Viewer, ViewerFrame};

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
    /// Звуки событий: кто-то вошёл или вышел, выключен микрофон и т.п.
    pub cues: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            tx: TxSettings::default(),
            jitter: Delay::default(),
            playback_ms: 20,
            cues: true,
        }
    }
}

/// Куда кодер отдаёт готовые пакеты: `(seq, opus)`.
type SendVoice = Arc<dyn Fn(u32, &[u8]) + Send + Sync>;

/// Микрофон и кодер.
struct Input {
    // Порядок полей важен: сначала останавливаем поток, потом устройство.
    tx: Transmitter,
    capture: audio::Capture,
}

/// Динамики и поток микшера.
struct Output {
    thread: MixerOutput,
    playback: audio::Playback,
}

/// Запущенные микрофон, динамики, микшер и кодер. Устройства можно менять
/// на ходу ([`AudioEngine::set_input`], [`AudioEngine::set_output`]):
/// звонок при этом не прерывается, а микшер сохраняет громкости и буферы.
pub struct AudioEngine {
    pub mixer: Arc<Mixer>,
    pub tx_state: Arc<TxState>,
    settings: AudioSettings,
    send: SendVoice,
    input: Input,
    output: Output,
}

impl AudioEngine {
    /// Открывает устройства и запускает кодер; `send(seq, opus)` получает пакеты.
    pub fn start(
        settings: &AudioSettings,
        tx_state: Arc<TxState>,
        send: impl Fn(u32, &[u8]) + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let mixer = Arc::new(Mixer::new(settings.jitter));
        mixer.set_cues_enabled(settings.cues);
        let output = start_output(&mixer, settings, settings.output.as_deref())?;
        let send: SendVoice = Arc::new(send);
        let input = start_input(&tx_state, settings, &send, settings.input.as_deref(), 0)?;
        Ok(Self { mixer, tx_state, settings: settings.clone(), send, input, output })
    }

    /// Открытый микрофон.
    pub fn capture(&self) -> &audio::Capture {
        &self.input.capture
    }

    /// Открытое устройство вывода.
    pub fn playback(&self) -> &audio::Playback {
        &self.output.playback
    }

    /// Переключает микрофон (часть имени; `None` — системный по умолчанию).
    /// Если новое устройство не открылось, остаётся старое.
    pub fn set_input(&mut self, device: Option<&str>) -> anyhow::Result<()> {
        // Новое устройство открываем, пока старое ещё работает: так ошибка
        // ничего не ломает, а пауза в передаче — только на перезапуск кодера.
        let (capture, mic, wakeup) = open_input(device)?;
        // Номер кадра — время записи (см. `Transmitter::spawn`), поэтому новый
        // кодер продолжает нумерацию старого, а не начинает с нуля.
        let next_seq = self.input.tx.stop();
        let tx = spawn_tx(&self.tx_state, &self.settings, &self.send, mic, wakeup, next_seq)?;
        self.input = Input { tx, capture };
        self.settings.input = device.map(str::to_string);
        Ok(())
    }

    /// Переключает устройство вывода (часть имени; `None` — системное по
    /// умолчанию). Если новое устройство не открылось, остаётся старое.
    pub fn set_output(&mut self, device: Option<&str>) -> anyhow::Result<()> {
        let (playback, speakers, wakeup) = open_output(device)?;
        // Два потока микшера не должны забирать кадры одновременно.
        self.output.thread.stop();
        let thread = self.mixer.spawn_output(
            speakers,
            playback.config.sample_rate,
            wakeup,
            self.settings.playback_ms.clamp(5, 200),
        )?;
        self.output = Output { thread, playback };
        self.settings.output = device.map(str::to_string);
        Ok(())
    }
}

fn open_input(
    device: Option<&str>,
) -> anyhow::Result<(audio::Capture, HeapCons<f32>, Arc<Wakeup>)> {
    let wakeup = Arc::new(Wakeup::default());
    let (capture, mic) = audio::start_capture(device, wakeup.clone())?;
    Ok((capture, mic, wakeup))
}

fn open_output(
    device: Option<&str>,
) -> anyhow::Result<(audio::Playback, HeapProd<f32>, Arc<Wakeup>)> {
    let wakeup = Arc::new(Wakeup::default());
    let (playback, speakers) = audio::start_playback(device, wakeup.clone())?;
    Ok((playback, speakers, wakeup))
}

fn spawn_tx(
    tx_state: &Arc<TxState>,
    settings: &AudioSettings,
    send: &SendVoice,
    mic: HeapCons<f32>,
    wakeup: Arc<Wakeup>,
    first_seq: u32,
) -> anyhow::Result<Transmitter> {
    let send = send.clone();
    Transmitter::spawn(
        mic,
        wakeup,
        tx_state.clone(),
        settings.tx.clone(),
        first_seq,
        move |seq, opus| send(seq, opus),
    )
}

fn start_input(
    tx_state: &Arc<TxState>,
    settings: &AudioSettings,
    send: &SendVoice,
    device: Option<&str>,
    first_seq: u32,
) -> anyhow::Result<Input> {
    let (capture, mic, wakeup) = open_input(device)?;
    let tx = spawn_tx(tx_state, settings, send, mic, wakeup, first_seq)?;
    Ok(Input { tx, capture })
}

fn start_output(
    mixer: &Arc<Mixer>,
    settings: &AudioSettings,
    device: Option<&str>,
) -> anyhow::Result<Output> {
    let (playback, speakers, wakeup) = open_output(device)?;
    let thread = mixer.spawn_output(
        speakers,
        playback.config.sample_rate,
        wakeup,
        settings.playback_ms.clamp(5, 200),
    )?;
    Ok(Output { thread, playback })
}

/// Что происходит в комнате.
#[derive(Debug, Clone)]
pub enum CallEvent {
    PeerJoined(PeerInfo),
    PeerLeft(PeerId),
    /// Изменился статус участника или наш собственный (`id` совпадает с `Call::me`).
    PeerUpdated(PeerInfo),
    /// Новое сообщение в чате (в том числе наше собственное, вернувшееся от сервера).
    Chat(ChatMessage),
    /// Новый кадр экрана, который мы смотрим.
    ScreenFrame(ViewerFrame),
    /// Сколько человек смотрит наш экран.
    Viewers(u32),
    /// Наша демонстрация прервалась сама (например, закрыли показываемое окно).
    ShareStopped(String),
    /// Соединение закрылось не по нашей инициативе.
    Ended {
        reason: String,
        /// Связь потерялась (а не сервер нас отключил или остановился):
        /// имеет смысл переподключиться.
        lost: bool,
    },
}

/// Состояние комнаты, которое обновляет задача управляющего канала.
struct Room {
    me: PeerInfo,
    peers: HashMap<PeerId, PeerInfo>,
    /// Сообщения чата: история с сервера и всё, что пришло после.
    chat: VecDeque<ChatMessage>,
}

/// Сколько сообщений чата держать в памяти.
const CHAT_KEEP: usize = 500;

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
    on_event: OnEvent,
    video: net::VideoSender,
    /// Наша демонстрация экрана, если идёт.
    sharer: Arc<Mutex<Option<Sharer>>>,
    viewer: Arc<Viewer>,
    /// Чьи экраны мы смотрим.
    watching: Arc<Mutex<HashSet<PeerId>>>,
}

type OnEvent = Arc<dyn Fn(CallEvent) + Send + Sync>;

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
        let on_event: OnEvent = Arc::new(on_event);
        let mut session = net::connect_with(server, name, admin_token, trust).await?;
        let my_id = session.me.id;
        let fingerprint = session.fingerprint.clone();
        let room = Arc::new(Mutex::new(Room {
            me: session.me.clone(),
            peers: session.peers.iter().map(|p| (p.id, p.clone())).collect(),
            chat: session.chat.iter().cloned().collect(),
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

        // Демонстрация экрана: свои кадры — в сеть, чужие — в декодер.
        let video = session.video_sender();
        let sharer: Arc<Mutex<Option<Sharer>>> = Arc::default();
        let watching: Arc<Mutex<HashSet<PeerId>>> = Arc::default();
        let viewer = Arc::new(Viewer::start(
            {
                let on_event = on_event.clone();
                move |frame| on_event(CallEvent::ScreenFrame(frame))
            },
            {
                let control = control.clone();
                move |id| control.send(ClientMsg::RequestKeyframe { id })
            },
        )?);
        let video_task = {
            let receiver = session.video_receiver();
            let viewer = viewer.clone();
            let watching = watching.clone();
            tokio::spawn(async move {
                while let Some((from, header, data)) = receiver.recv().await {
                    if watching.lock().unwrap().contains(&from) {
                        // Не успел — выброшен; декодер заметит пропуск и сам
                        // попросит опорный кадр.
                        viewer.push(from, header, data);
                    }
                }
            })
        };
        let control_task = {
            let on_event = on_event.clone();
            let sharer = sharer.clone();
            let viewer = viewer.clone();
            let watching = watching.clone();
            let stop_watching = move |id: PeerId| {
                if watching.lock().unwrap().remove(&id) {
                    viewer.forget(id);
                }
            };
            let mixer = audio.mixer.clone();
            let room = room.clone();
            let hanging_up = hanging_up.clone();
            let closer = closer.clone();
            tokio::spawn(async move {
                let reason = loop {
                    match session.next_event().await {
                        Ok(Some(ServerMsg::PeerJoined(peer))) => {
                            mixer.play_cue(Cue::PeerJoined);
                            room.lock().unwrap().peers.insert(peer.id, peer.clone());
                            on_event(CallEvent::PeerJoined(peer));
                        }
                        Ok(Some(ServerMsg::PeerLeft { id })) => {
                            room.lock().unwrap().peers.remove(&id);
                            mixer.remove_speaker(id);
                            stop_watching(id);
                            mixer.play_cue(Cue::PeerLeft);
                            on_event(CallEvent::PeerLeft(id));
                        }
                        Ok(Some(ServerMsg::Chat(message))) => {
                            if message.from != my_id {
                                mixer.play_cue(Cue::Message);
                            }
                            {
                                let mut room = room.lock().unwrap();
                                if room.chat.len() == CHAT_KEEP {
                                    room.chat.pop_front();
                                }
                                room.chat.push_back(message.clone());
                            }
                            on_event(CallEvent::Chat(message));
                        }
                        Ok(Some(ServerMsg::KeyframeNeeded)) => {
                            if let Some(sharer) = sharer.lock().unwrap().as_ref() {
                                sharer.request_keyframe();
                            }
                        }
                        Ok(Some(ServerMsg::Viewers { count })) => {
                            if let Some(sharer) = sharer.lock().unwrap().as_ref() {
                                sharer.set_viewers(count);
                            }
                            on_event(CallEvent::Viewers(count));
                        }
                        Ok(Some(ServerMsg::PeerUpdated(peer))) => {
                            if !peer.sharing {
                                stop_watching(peer.id);
                            }
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
                let lost = !closer.closed_by_server();
                let reason = closer.close_reason().unwrap_or(reason);
                if !hanging_up.load(Ordering::Relaxed) {
                    on_event(CallEvent::Ended { reason, lost });
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
            tasks: vec![voice_task, control_task, video_task],
            on_event,
            video,
            sharer,
            viewer,
            watching,
        };
        call.sync_status();
        call.audio.mixer.play_cue(Cue::Connected);
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

    /// Сообщения чата, старые — первыми.
    pub fn chat(&self) -> Vec<ChatMessage> {
        self.room.lock().unwrap().chat.iter().cloned().collect()
    }

    /// Отправляет сообщение в чат. Оно придёт обратно событием
    /// [`CallEvent::Chat`], когда сервер разошлёт его всем.
    pub fn send_chat(&self, text: &str) {
        let text: String = text.trim().chars().take(vwf_proto::MAX_CHAT_CHARS).collect();
        if !text.is_empty() {
            self.control.send(ClientMsg::Chat { text });
        }
    }

    pub fn mixer(&self) -> &Arc<Mixer> {
        &self.audio.mixer
    }

    /// Переключает микрофон, не прерывая звонок.
    pub fn set_input(&mut self, device: Option<&str>) -> anyhow::Result<()> {
        self.audio.set_input(device)
    }

    /// Переключает динамики или наушники, не прерывая звонок.
    pub fn set_output(&mut self, device: Option<&str>) -> anyhow::Result<()> {
        self.audio.set_output(device)
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

    /// Начинает показывать экран: `source` — идентификатор из
    /// [`crate::screen::sources`]. Предыдущая демонстрация, если была, заканчивается.
    pub fn start_share(&self, source: &str, settings: ShareSettings) -> anyhow::Result<()> {
        self.stop_share();
        let video = self.video.clone();
        let control = self.control.clone();
        let on_event = self.on_event.clone();
        let sharer = Sharer::start(
            source,
            settings,
            move |frame| video.send(frame),
            move |reason| {
                control.send(ClientMsg::SetSharing { sharing: false });
                on_event(CallEvent::ShareStopped(reason));
            },
        )?;
        *self.sharer.lock().unwrap() = Some(sharer);
        self.control.send(ClientMsg::SetSharing { sharing: true });
        Ok(())
    }

    /// Заканчивает показ экрана.
    pub fn stop_share(&self) {
        let sharer = self.sharer.lock().unwrap().take();
        if let Some(sharer) = sharer {
            drop(sharer);
            self.control.send(ClientMsg::SetSharing { sharing: false });
        }
    }

    /// Идёт ли наша демонстрация.
    pub fn sharing(&self) -> bool {
        self.sharer.lock().unwrap().is_some()
    }

    /// Смотреть экран участника `id` (или перестать). Кадры приходят
    /// событиями [`CallEvent::ScreenFrame`].
    pub fn watch(&self, id: PeerId, watch: bool) {
        let changed = if watch {
            self.watching.lock().unwrap().insert(id)
        } else {
            self.watching.lock().unwrap().remove(&id)
        };
        if changed {
            if !watch {
                self.viewer.forget(id);
            }
            self.control.send(ClientMsg::Watch { id, watch });
        }
    }

    /// Завершает звонок.
    pub async fn hang_up(self) {
        self.sharer.lock().unwrap().take();
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

//! Графический клиент: окно на Tauri (интерфейс — HTML/CSS/JS в `ui/`),
//! а весь звук и сеть — та же библиотека `vwf-client`, что у консольной версии.
//!
//! Интерфейс вызывает команды из этого файла через `invoke(...)` и слушает
//! события комнаты (`peer-joined`, `peer-left`, `call-ended`).

//!
//! На Android нет трея, глобальных клавиш, второй копии программы и
//! отдельных окон: эти части собираются только для компьютера (`cfg(desktop)`).

#[cfg(target_os = "android")]
mod android;
mod camera;
mod host;
mod rooms;
#[cfg(windows)]
pub mod selfupdate;
#[cfg(windows)]
mod shortcut;
mod screen;
#[cfg(desktop)]
mod tray;
mod update;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, State};
#[cfg(desktop)]
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use vwf_client::call::{AudioSettings, Call, CallEvent};
use vwf_client::cues::Cue;
use vwf_client::known_servers::KnownServers;
use vwf_client::mic_test::{MicTest, Phase};
use vwf_client::net::{Availability, CertificateChanged, Trust};
use vwf_client::pipeline::{TxMode, TxState};
use vwf_client::{audio, discovery};
use vwf_proto::{PeerId, PeerInfo};

use crate::host::{HostInfo, HostedServer, InternetStatus};

struct AppState {
    /// Настройки передачи и индикаторы микрофона. Живут всё время работы
    /// программы, поэтому mute, режим и порог не сбрасываются между звонками.
    tx: Arc<TxState>,
    call: tokio::sync::Mutex<Option<Call>>,
    deafened: AtomicBool,
    /// Клавиша рации в формате `Ctrl+Alt+KeyV`, `F8` и т.п.
    ptt_key: Mutex<Option<String>>,
    /// Открытая на этом компьютере комната (свой сервер).
    host: tokio::sync::Mutex<Option<HostedServer>>,
    /// Свои комнаты (загружаются при запуске).
    rooms: Mutex<Option<rooms::Rooms>>,
    /// Своя аватарка (JPEG от окна, пустая — нет) и файл, где она хранится.
    avatar: Mutex<Vec<u8>>,
    avatar_file: Mutex<Option<std::path::PathBuf>>,
    /// Запомненные сертификаты серверов (загружаются при запуске).
    known: Mutex<KnownServers>,
    /// Номер текущего звонка. Растёт при каждом подключении и отключении;
    /// по нему отбрасываются события и переподключения старых звонков.
    generation: AtomicU64,
    /// Параметры текущего звонка — для переподключения.
    params: Mutex<Option<CallParams>>,
    /// Звуки событий.
    cues: AtomicBool,
    /// Крестик окна сворачивает программу в трей (если значок в трее есть).
    close_to_tray: AtomicBool,
    #[cfg_attr(mobile, allow(dead_code))]
    tray_ready: AtomicBool,
    /// Проверка микрофона до входа в комнату.
    mic_test: Mutex<Option<MicTest>>,
}

/// Ошибки отдаём в интерфейс строкой — он покажет её пользователю.
type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Devices {
    inputs: Vec<String>,
    outputs: Vec<String>,
    default_input: Option<String>,
    default_output: Option<String>,
}

/// На Android к звуку можно обращаться только после того, как звуковая
/// библиотека получила Android-контекст (см. `android.rs`).
#[cfg(target_os = "android")]
fn audio_ready() -> CmdResult<()> {
    android::wait_ready()
}

#[cfg(not(target_os = "android"))]
fn audio_ready() -> CmdResult<()> {
    Ok(())
}

// async: команда выполняется не в главном потоке — на Android ей, может быть,
// придётся подождать готовности звука.
#[tauri::command]
async fn list_devices() -> CmdResult<Devices> {
    audio_ready()?;
    let list = audio::device_list().map_err(err)?;
    Ok(Devices {
        inputs: list.inputs,
        outputs: list.outputs,
        default_input: list.default_input,
        default_output: list.default_output,
    })
}

/// Сколько секунд записывается проверка микрофона.
const MIC_TEST_SECONDS: f32 = 4.0;

/// Проверка микрофона: запись на несколько секунд, затем она проигрывается.
/// Во время звонка не нужна (там есть индикатор и «слышать себя»).
#[tauri::command]
async fn mic_test_start(
    state: State<'_, AppState>,
    input: Option<String>,
    output: Option<String>,
) -> CmdResult<()> {
    audio_ready()?;
    if state.call.lock().await.is_some() {
        return Err("во время разговора проверка не нужна".into());
    }
    let non_empty = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
    // Прежняя проверка останавливается до того, как откроется новая. Поток
    // ждём уже без замка: иначе mic_test_status в главном потоке встанет.
    let previous = state.mic_test.lock().unwrap().take();
    drop(previous);
    let test = MicTest::start(non_empty(input), non_empty(output), MIC_TEST_SECONDS);
    *state.mic_test.lock().unwrap() = Some(test);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MicTestState {
    /// "recording", "playing", "done", "failed" или "idle".
    phase: &'static str,
    level_db: f32,
    progress: f32,
    error: Option<String>,
}

#[tauri::command]
fn mic_test_status(state: State<'_, AppState>) -> MicTestState {
    let test = state.mic_test.lock().unwrap();
    let Some(test) = test.as_ref() else {
        return MicTestState { phase: "idle", level_db: -120.0, progress: 0.0, error: None };
    };
    let status = test.status();
    MicTestState {
        phase: match status.phase {
            Phase::Recording => "recording",
            Phase::Playing => "playing",
            Phase::Done => "done",
            Phase::Failed => "failed",
        },
        level_db: status.level_db,
        progress: status.progress,
        error: status.error,
    }
}

#[tauri::command]
async fn mic_test_stop(state: State<'_, AppState>) -> CmdResult<()> {
    // Остановка ждёт поток проверки — не в главном потоке.
    let test = state.mic_test.lock().unwrap().take();
    drop(test);
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectArgs {
    server: String,
    name: String,
    /// Пустая строка или `null` — устройство по умолчанию.
    input: Option<String>,
    output: Option<String>,
    /// Пароль комнаты, если хост его задал.
    #[serde(default)]
    password: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Peer {
    id: PeerId,
    name: String,
    admin: bool,
    muted: bool,
    deafened: bool,
    server_muted: bool,
    sharing: bool,
    /// Платформа и версия программы, например `android 0.1.42`.
    app: String,
    /// Включил камеру.
    camera: bool,
}

impl From<PeerInfo> for Peer {
    fn from(p: PeerInfo) -> Self {
        Self {
            id: p.id,
            name: p.name,
            admin: p.admin,
            muted: p.muted,
            deafened: p.deafened,
            server_muted: p.server_muted,
            sharing: p.sharing,
            app: p.app,
            camera: p.camera,
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Joined {
    me: Peer,
    peers: Vec<Peer>,
    /// Короткий код сертификата сервера — для сверки с хостом.
    server_code: String,
    input_device: String,
    output_device: String,
    /// Сообщения чата, старые — первыми.
    chat: Vec<ChatMessage>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ChatMessage {
    id: u64,
    from: PeerId,
    name: String,
    text: String,
    /// Unix-время, мс. Как число JavaScript оно точное до 2^53 — с запасом.
    time_ms: u64,
}

impl From<vwf_proto::ChatMessage> for ChatMessage {
    fn from(m: vwf_proto::ChatMessage) -> Self {
        // Номер сообщения сервер начинает со случайного u64; в JavaScript числа
        // точны только до 2^53, поэтому для интерфейса берём младшие биты.
        Self {
            id: m.id & ((1 << 53) - 1),
            from: m.from,
            name: m.name,
            text: m.text,
            time_ms: m.time_ms,
        }
    }
}

#[derive(Serialize, Clone)]
struct PeerLeft {
    id: PeerId,
}

#[derive(Serialize, Clone)]
struct CallEnded {
    reason: String,
}

#[derive(Serialize, Clone)]
struct ShareViewers {
    count: u32,
}

/// Сколько зрителей у каждого слоя нашей камеры.
#[derive(Serialize, Clone)]
struct CameraViewers {
    layers: [u32; vwf_proto::CAMERA_LAYERS],
}

#[derive(Serialize, Clone)]
struct PeerAvatar {
    id: PeerId,
    /// `data:`-адрес картинки; `None` — аватарки нет.
    url: Option<String>,
}

/// Картинка как `data:`-адрес — его можно сразу поставить в `<img src>`.
fn data_url(mime: &str, data: &[u8]) -> String {
    use base64::Engine;
    format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(data))
}

#[derive(Serialize, Clone)]
struct ShareStopped {
    reason: String,
}

/// Ошибка подключения. Подмену сертификата интерфейс показывает отдельно,
/// с выбором «отмена / доверять новому».
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ConnectError {
    #[serde(rename_all = "camelCase")]
    Message { message: String },
    #[serde(rename_all = "camelCase")]
    CertificateChanged {
        address: String,
        known_code: String,
        presented_code: String,
        /// Полный отпечаток: его запоминаем, если пользователь доверится.
        presented: String,
    },
}

impl From<anyhow::Error> for ConnectError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast_ref::<CertificateChanged>() {
            Some(changed) => ConnectError::CertificateChanged {
                address: changed.address.clone(),
                known_code: vwf_proto::short_fingerprint(&changed.known),
                presented_code: vwf_proto::short_fingerprint(&changed.presented),
                presented: changed.presented.clone(),
            },
            None => ConnectError::Message { message: format!("{e:#}") },
        }
    }
}

/// С какими параметрами подключались: нужны, чтобы переподключиться.
#[derive(Clone)]
struct CallParams {
    server: String,
    name: String,
    input: Option<String>,
    output: Option<String>,
    password: Option<String>,
}

#[tauri::command]
async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    args: ConnectArgs,
) -> Result<Joined, ConnectError> {
    let non_empty = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
    let params = CallParams {
        server: args.server.trim().to_string(),
        name: args.name.trim().to_string(),
        input: non_empty(args.input),
        output: non_empty(args.output),
        password: args.password.filter(|p| !p.is_empty()),
    };
    // Новый звонок — новое поколение: события и переподключение старого
    // звонка, если он ещё где-то в пути, больше ни на что не влияют.
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    *state.params.lock().unwrap() = Some(params.clone());
    // Проверка микрофона заняла бы микрофон и динамики.
    let test = state.mic_test.lock().unwrap().take();
    drop(test);
    start_call(&app, &state, &params, generation).await
}

/// Подключается и кладёт звонок в `state.call`. Если за это время пользователь
/// нажал «Выйти» или подключился заново (сменилось поколение), новый звонок
/// сразу завершается.
///
/// Пока идёт подключение (до 5 с, если сервер не отвечает; по коду приглашения
/// дольше), `state.call` не заблокирован: иначе кнопки «Выйти», микрофона и
/// т.п. ждали бы его.
async fn start_call(
    app: &AppHandle,
    state: &AppState,
    params: &CallParams,
    generation: u64,
) -> Result<Joined, ConnectError> {
    let cancelled =
        || ConnectError::Message { message: "подключение отменено".into() };
    let old = state.call.lock().await.take();
    if let Some(old) = old {
        old.hang_up().await;
    }
    if state.generation.load(Ordering::SeqCst) != generation {
        return Err(cancelled());
    }
    audio_ready().map_err(|message| ConnectError::Message { message })?;

    let settings = AudioSettings {
        input: params.input.clone(),
        output: params.output.clone(),
        cues: state.cues.load(Ordering::Relaxed),
        ..AudioSettings::default()
    };
    // К своему серверу подключаемся с секретом хоста и знаем его сертификат
    // точно. Для остальных — запоминаем при первом подключении и сверяем.
    let own = state.host.lock().await.as_ref().and_then(|h| h.own(&params.server));
    let (admin_token, trust) = match &own {
        Some((token, fingerprint)) => (Some(token.as_str()), Trust::Exact(fingerprint)),
        None => (None, Trust::Known(&state.known)),
    };
    let credentials =
        vwf_client::net::Credentials { admin_token, password: params.password.as_deref() };
    let events = app.clone();
    let call = Call::start(
        &params.server,
        &params.name,
        credentials,
        trust,
        &settings,
        state.tx.clone(),
        move |event| {
            // События уже закончившегося звонка не нужны.
            let state = events.state::<AppState>();
            if state.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let _ = match event {
                CallEvent::PeerJoined(p) => events.emit("peer-joined", Peer::from(p)),
                CallEvent::PeerLeft(id) => {
                    screen::close_viewer(&events, id);
                    camera::forget(&events, Some(id));
                    events.emit("peer-left", PeerLeft { id })
                }
                CallEvent::PeerUpdated(p) => {
                    if !p.sharing {
                        screen::close_viewer(&events, p.id);
                    }
                    events.emit("peer-updated", Peer::from(p))
                }
                CallEvent::CameraPacket { from, header, data } => {
                    camera::on_packet(&events, from, header, data);
                    Ok(())
                }
                CallEvent::CameraViewers(layers) => {
                    events.emit("camera-viewers", CameraViewers { layers })
                }
                CallEvent::CameraStopped(reason) => {
                    events.emit("camera-stopped", ShareStopped { reason })
                }
                CallEvent::Chat(m) => events.emit("chat-message", ChatMessage::from(m)),
                CallEvent::ScreenPacket { from, header, data } => {
                    screen::on_packet(&events, from, header, data);
                    Ok(())
                }
                CallEvent::Viewers(count) => events.emit("share-viewers", ShareViewers { count }),
                CallEvent::Avatar { id, image } => {
                    let url = image.map(|(mime, data)| data_url(mime, &data));
                    events.emit("peer-avatar", PeerAvatar { id, url })
                }
                CallEvent::ShareStopped(reason) => {
                    events.emit("share-stopped", ShareStopped { reason })
                }
                CallEvent::Ended { reason, lost: true } => {
                    screen::close_all_viewers(&events);
                    camera::forget(&events, None);
                    spawn_reconnect(events.clone(), generation, reason);
                    Ok(())
                }
                CallEvent::Ended { reason, lost: false } => {
                    screen::close_all_viewers(&events);
                    camera::forget(&events, None);
                    events.emit("call-ended", CallEnded { reason })
                }
            };
        },
    )
    .await?;
    call.mixer().set_deafened(state.deafened.load(Ordering::Relaxed));
    call.sync_status();
    let avatar = state.avatar.lock().unwrap().clone();
    if !avatar.is_empty() {
        call.set_avatar(avatar);
    }
    screen::set_requester(app, call.keyframe_requester());
    camera::set_requester(app, call.keyframe_requester());

    // Сервер ещё не получил наш статус, поэтому mute/deafen берём свои.
    let me = Peer {
        muted: state.tx.muted(),
        deafened: state.deafened.load(Ordering::Relaxed),
        ..Peer::from(call.me())
    };
    let joined = Joined {
        me,
        peers: call.peers().into_iter().map(Peer::from).collect(),
        server_code: vwf_proto::short_fingerprint(&call.fingerprint),
        input_device: call.audio.capture().device_name.clone(),
        output_device: call.audio.playback().device_name.clone(),
        chat: call.chat().into_iter().map(ChatMessage::from).collect(),
    };
    let mut slot = state.call.lock().await;
    if state.generation.load(Ordering::SeqCst) != generation {
        drop(slot);
        call.hang_up().await;
        return Err(cancelled());
    }
    if let Some(other) = slot.replace(call) {
        // Сюда можно попасть, только если два подключения одного поколения
        // шли одновременно, — оставляем последнее.
        drop(slot);
        other.hang_up().await;
    }
    Ok(joined)
}

/// Сколько пытаться вернуться в комнату после потери связи.
const RECONNECT_FOR: Duration = Duration::from_secs(90);
/// Пауза между попытками.
const RECONNECT_PAUSE: Duration = Duration::from_secs(3);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Reconnecting {
    reason: String,
    attempt: u32,
}

/// Запускает [`reconnect`] в фоне. Отдельная функция (а не вызов прямо из
/// `start_call`): иначе компилятор не может доказать, что future, который
/// ссылается сам на себя через `start_call`, можно передавать между потоками.
fn spawn_reconnect(app: AppHandle, generation: u64, reason: String) {
    let task: std::pin::Pin<Box<dyn Future<Output = ()> + Send>> =
        Box::pin(reconnect(app, generation, reason));
    tauri::async_runtime::spawn(task);
}

/// Связь со звонком `generation` потерялась: пробуем вернуться в ту же комнату,
/// пока пользователь не нажмёт «Выйти» или не выйдет время.
async fn reconnect(app: AppHandle, generation: u64, reason: String) {
    let state = app.state::<AppState>();
    let current = || state.generation.load(Ordering::SeqCst) == generation;
    let Some(params) = state.params.lock().unwrap().clone() else { return };
    let deadline = tokio::time::Instant::now() + RECONNECT_FOR;
    let mut last_error = reason.clone();
    for attempt in 1.. {
        if !current() {
            return;
        }
        let _ = app.emit("call-reconnecting", Reconnecting { reason: reason.clone(), attempt });
        match start_call(&app, &state, &params, generation).await {
            Ok(joined) => {
                if current() {
                    let _ = app.emit("call-reconnected", joined);
                }
                return;
            }
            // Сертификат сменился — решать пользователю, а не программе.
            Err(ConnectError::CertificateChanged { .. }) => {
                last_error = "сертификат сервера изменился, подключитесь заново".into();
                break;
            }
            Err(ConnectError::Message { message }) => last_error = message,
        }
        if tokio::time::Instant::now() + RECONNECT_PAUSE > deadline {
            break;
        }
        tokio::time::sleep(RECONNECT_PAUSE).await;
    }
    if current() {
        let _ = app.emit("call-ended", CallEnded { reason: last_error });
    }
}

/// Пользователь подтвердил новый сертификат сервера.
#[tauri::command]
fn trust_server(state: State<'_, AppState>, address: String, fingerprint: String) -> CmdResult<()> {
    state.known.lock().unwrap().remember(&address, &fingerprint).map_err(err)
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> CmdResult<()> {
    // Сначала новое поколение: так остановится и переподключение, если оно идёт.
    state.generation.fetch_add(1, Ordering::SeqCst);
    *state.params.lock().unwrap() = None;
    if let Some(call) = state.call.lock().await.take() {
        call.hang_up().await;
    }
    Ok(())
}

/// Работает со списком своих комнат.
fn with_rooms<T>(
    state: &AppState,
    f: impl FnOnce(&mut rooms::Rooms) -> anyhow::Result<T>,
) -> CmdResult<T> {
    let mut rooms = state.rooms.lock().unwrap();
    let rooms = rooms.as_mut().ok_or("список комнат ещё не загружен")?;
    f(rooms).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn list_rooms(state: State<'_, AppState>) -> CmdResult<Vec<rooms::RoomConfig>> {
    with_rooms(&state, |rooms| Ok(rooms.list()))
}

#[tauri::command]
fn create_room(
    state: State<'_, AppState>,
    settings: rooms::RoomSettings,
) -> CmdResult<rooms::RoomConfig> {
    with_rooms(&state, |rooms| rooms.create(settings))
}

/// Новые настройки комнаты из окна настроек. Если комната открыта, пароль,
/// лимит, видимость и интернет меняются сразу; название — со следующего открытия.
#[tauri::command]
async fn update_room(
    state: State<'_, AppState>,
    id: String,
    settings: rooms::RoomSettings,
) -> CmdResult<rooms::RoomConfig> {
    let room = with_rooms(&state, |rooms| rooms.configure(&id, settings))?;
    if let Some(server) = state.host.lock().await.as_ref().filter(|s| s.info.room_id == id) {
        server.set_access(&room.password, room.max_peers);
        server.set_lan(room.lan);
        server.set_internet(room.internet).map_err(|e| format!("{e:#}"))?;
    }
    Ok(room)
}

/// Удаляет комнату; её код приглашения перестаёт работать навсегда.
#[tauri::command]
async fn delete_room(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    if state.host.lock().await.as_ref().is_some_and(|server| server.info.room_id == id) {
        return Err("сначала закройте эту комнату".into());
    }
    with_rooms(&state, |rooms| rooms.delete(&id))
}

/// Видна ли комната в списке «В локальной сети». Для открытой комнаты —
/// сразу.
#[tauri::command]
async fn set_room_lan(state: State<'_, AppState>, id: String, visible: bool) -> CmdResult<()> {
    with_rooms(&state, |rooms| rooms.update(&id, |room| room.lan = visible))?;
    if let Some(server) = state.host.lock().await.as_ref().filter(|s| s.info.room_id == id) {
        server.set_lan(visible);
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Hosted {
    info: HostInfo,
    /// Куда подключаться самому хосту.
    local_address: String,
    /// Настройки комнаты (видимость, интернет).
    room: rooms::RoomConfig,
}

/// Открывает свою комнату `room_id`: запускает сервер на этом компьютере.
/// Одновременно открыта только одна комната — у всех один порт.
#[tauri::command]
async fn start_server(
    app: AppHandle,
    state: State<'_, AppState>,
    room_id: String,
) -> CmdResult<Hosted> {
    let (room, room_file) = with_rooms(&state, |rooms| {
        let room = rooms.get(&room_id).ok_or_else(|| anyhow::anyhow!("комната не найдена"))?;
        Ok((room, rooms.invite_file(&room_id)))
    })?;
    let mut host = state.host.lock().await;
    if host.as_ref().is_some_and(|server| server.info.room_id != room_id) {
        return Err("открыта другая комната — сначала закройте её".into());
    }
    if host.is_none() {
        // Сертификат сервера храним в папке данных приложения
        // (на Windows это %APPDATA%\io.github.addfsw.poigrulkiny).
        let data_dir = app.path().app_data_dir().map_err(err)?;
        *host = Some(
            HostedServer::start(&room, room_file, data_dir).await.map_err(|e| format!("{e:#}"))?,
        );
    }
    let server = host.as_ref().expect("только что запустили");
    Ok(Hosted { info: server.info.clone(), local_address: server.local_address(), room })
}

/// Только для своей комнаты: «никого больше не пускать». Вернуться могут
/// только те, кто был в комнате (не выгнанные) в последние минуты.
#[tauri::command]
async fn set_lock(state: State<'_, AppState>, locked: bool) -> CmdResult<()> {
    if let Some(server) = state.host.lock().await.as_ref() {
        server.set_locked(locked);
    }
    Ok(())
}

/// Только для своей комнаты: доступ через интернет (пробивка NAT, код приглашения).
#[tauri::command]
async fn set_internet(state: State<'_, AppState>, enabled: bool) -> CmdResult<InternetStatus> {
    let host = state.host.lock().await;
    let server = host.as_ref().ok_or("комната не создана")?;
    server.set_internet(enabled).map_err(|e| format!("{e:#}"))?;
    // Запоминаем для этой комнаты: в следующий раз включится сам.
    with_rooms(&state, |rooms| rooms.update(&server.info.room_id, |room| room.internet = enabled))?;
    Ok(server.internet_status())
}

#[tauri::command]
async fn internet_status(state: State<'_, AppState>) -> CmdResult<Option<InternetStatus>> {
    Ok(state.host.lock().await.as_ref().map(HostedServer::internet_status))
}

/// Останавливает свой сервер; все участники отключаются.
#[tauri::command]
async fn stop_server(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(server) = state.host.lock().await.take() {
        server.stop().await;
    }
    Ok(())
}

#[tauri::command]
async fn set_muted(app: AppHandle, muted: bool) -> CmdResult<()> {
    apply_muted(&app, muted).await;
    Ok(())
}

#[tauri::command]
async fn set_deafened(app: AppHandle, deafened: bool) -> CmdResult<()> {
    apply_deafened(&app, deafened).await;
    Ok(())
}

#[derive(Serialize, Clone)]
struct LocalStatus {
    muted: bool,
    deafened: bool,
}

/// Выключает или включает микрофон — по кнопке в окне или из меню трея.
async fn apply_muted(app: &AppHandle, muted: bool) {
    let state = app.state::<AppState>();
    state.tx.set_muted(muted);
    if let Some(call) = state.call.lock().await.as_ref() {
        call.sync_status();
        call.mixer().play_cue(if muted { Cue::Muted } else { Cue::Unmuted });
    }
    local_status_changed(app);
}

/// Выключает или включает звук собеседников.
async fn apply_deafened(app: &AppHandle, deafened: bool) {
    let state = app.state::<AppState>();
    state.deafened.store(deafened, Ordering::Relaxed);
    if let Some(call) = state.call.lock().await.as_ref() {
        call.mixer().set_deafened(deafened);
        call.sync_status();
        call.mixer().play_cue(if deafened { Cue::Deafened } else { Cue::Undeafened });
    }
    local_status_changed(app);
}

/// Сообщает окну и трею, выключены ли микрофон и звук.
fn local_status_changed(app: &AppHandle) {
    let state = app.state::<AppState>();
    let status =
        LocalStatus { muted: state.tx.muted(), deafened: state.deafened.load(Ordering::Relaxed) };
    #[cfg(desktop)]
    if let Some(tray) = app.try_state::<tray::TrayItems>() {
        tray.show_status(status.muted, status.deafened);
    }
    let _ = app.emit("local-status", status);
}

/// Кладёт трубку и останавливает свою комнату — перед выходом из программы.
async fn shutdown(app: &AppHandle) {
    let state = app.state::<AppState>();
    state.generation.fetch_add(1, Ordering::SeqCst);
    if let Some(call) = state.call.lock().await.take() {
        call.hang_up().await;
    }
    if let Some(server) = state.host.lock().await.take() {
        server.stop().await;
    }
}

/// Сворачивать ли программу в трей по крестику окна.
#[tauri::command]
fn set_close_to_tray(state: State<'_, AppState>, enabled: bool) {
    state.close_to_tray.store(enabled, Ordering::Relaxed);
}

/// Своя аватарка как `data:`-адрес (для окна), если она есть.
#[tauri::command]
fn get_avatar(state: State<'_, AppState>) -> Option<String> {
    let avatar = state.avatar.lock().unwrap();
    vwf_client::call::image_type(&avatar).map(|mime| data_url(mime, &avatar))
}

/// Ставит свою аватарку (картинку уже уменьшило окно); пустая — убрать.
/// Запоминается и сразу видна остальным, если идёт звонок.
#[tauri::command]
async fn set_avatar(state: State<'_, AppState>, data: Vec<u8>) -> CmdResult<Option<String>> {
    let url = if data.is_empty() {
        None
    } else {
        let mime = vwf_client::call::image_type(&data)
            .ok_or("картинка слишком большая или в неизвестном формате")?;
        Some(data_url(mime, &data))
    };
    if let Some(file) = state.avatar_file.lock().unwrap().as_ref() {
        let saved = if data.is_empty() {
            std::fs::remove_file(file).or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            })
        } else {
            std::fs::create_dir_all(file.parent().expect("файл в папке"))
                .and_then(|()| std::fs::write(file, &data))
        };
        saved.map_err(|e| format!("не удалось сохранить аватарку: {e}"))?;
    }
    *state.avatar.lock().unwrap() = data.clone();
    if let Some(call) = state.call.lock().await.as_ref() {
        call.set_avatar(data);
    }
    Ok(url)
}

#[tauri::command]
async fn send_chat(state: State<'_, AppState>, text: String) -> CmdResult<()> {
    match state.call.lock().await.as_ref() {
        Some(call) => {
            call.send_chat(&text);
            Ok(())
        }
        None => Err("нет связи с комнатой".into()),
    }
}

/// Открывает ссылку из чата в браузере. Только http(s): ссылку прислал
/// другой человек, и запускать по ней программы или файлы нельзя.
#[tauri::command]
fn open_url(url: String) -> CmdResult<()> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err("открываются только ссылки http и https".into());
    }
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(err)
}

/// Только хост: заглушить участника для всех или вернуть ему голос.
#[tauri::command]
async fn set_server_mute(state: State<'_, AppState>, id: PeerId, muted: bool) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.as_ref() {
        call.set_server_mute(id, muted);
    }
    Ok(())
}

/// Только хост: отключить участника от комнаты.
#[tauri::command]
async fn kick(state: State<'_, AppState>, id: PeerId) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.as_ref() {
        call.kick(id);
    }
    Ok(())
}

#[tauri::command]
async fn set_volume(state: State<'_, AppState>, id: PeerId, volume: f32) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.as_ref() {
        call.mixer().set_volume(id, volume);
    }
    Ok(())
}

/// Переключает микрофон во время звонка, не прерывая его. Возвращает имя
/// открытого устройства; без звонка — `None` (устройство возьмётся из
/// настроек при подключении).
#[tauri::command]
async fn set_input_device(
    state: State<'_, AppState>,
    device: Option<String>,
) -> CmdResult<Option<String>> {
    let mut call = state.call.lock().await;
    let Some(call) = call.as_mut() else { return Ok(None) };
    let device = device.filter(|d| !d.is_empty());
    call.set_input(device.as_deref()).map_err(|e| format!("{e:#}"))?;
    if let Some(params) = state.params.lock().unwrap().as_mut() {
        params.input = device;
    }
    Ok(Some(call.audio.capture().device_name.clone()))
}

/// То же для динамиков или наушников.
#[tauri::command]
async fn set_output_device(
    state: State<'_, AppState>,
    device: Option<String>,
) -> CmdResult<Option<String>> {
    let mut call = state.call.lock().await;
    let Some(call) = call.as_mut() else { return Ok(None) };
    let device = device.filter(|d| !d.is_empty());
    call.set_output(device.as_deref()).map_err(|e| format!("{e:#}"))?;
    if let Some(params) = state.params.lock().unwrap().as_mut() {
        params.output = device;
    }
    Ok(Some(call.audio.playback().device_name.clone()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LanServer {
    name: String,
    /// Полный адрес для подключения, например `192.168.1.10:47777`.
    address: String,
    /// Для показа: без порта, если он стандартный.
    host: String,
    users: u32,
    /// Вход по паролю.
    password: bool,
    compatible: bool,
}

/// Ищет серверы в локальной сети (около секунды).
#[tauri::command]
async fn discover_servers() -> CmdResult<Vec<LanServer>> {
    let servers = discovery::discover(Duration::from_millis(800)).await.map_err(err)?;
    Ok(servers
        .into_iter()
        .map(|s| LanServer {
            host: if s.address.port() == vwf_proto::DEFAULT_PORT {
                s.address.ip().to_string()
            } else {
                s.address.to_string()
            },
            address: s.address.to_string(),
            name: s.name,
            users: s.users,
            password: s.password,
            compatible: s.compatible,
        })
        .collect())
}

/// Запущена ли комната из списка недавних: `online`, `offline` или
/// `unknown` (проверить не удалось — например, нет интернета у нас).
#[derive(Serialize)]
struct RoomCheck {
    server: String,
    status: &'static str,
}

/// Проверяет недавние комнаты разом, не входя в них (несколько секунд).
/// `ask` — человек сам нажал на комнату: можно потратить сообщение ntfy.sh,
/// чтобы спросить хоста напрямую (см. `vwf_client::net::probe`).
#[tauri::command]
async fn check_rooms(servers: Vec<String>, ask: Option<bool>) -> Vec<RoomCheck> {
    let ask = ask.unwrap_or(false);
    let mut checks = tokio::task::JoinSet::new();
    for server in servers {
        checks.spawn(async move {
            let status = match vwf_client::net::probe(server.trim(), ask).await {
                Availability::Online => "online",
                Availability::Offline => "offline",
                Availability::Unknown => "unknown",
            };
            RoomCheck { server, status }
        });
    }
    checks.join_all().await
}

/// Звуки событий: вход и выход участников, кнопки микрофона и звука.
#[tauri::command]
async fn set_cues(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    state.cues.store(enabled, Ordering::Relaxed);
    if let Some(call) = state.call.lock().await.as_ref() {
        call.mixer().set_cues_enabled(enabled);
    }
    Ok(())
}

/// Автоусиление тихого голоса.
#[tauri::command]
fn set_agc(state: State<'_, AppState>, enabled: bool) {
    state.tx.set_agc(enabled);
}

/// «Слышать себя»: свой голос сразу звучит в своих динамиках.
#[tauri::command]
fn set_monitor(state: State<'_, AppState>, enabled: bool) {
    state.tx.set_monitor(enabled);
}

/// Эхоподавление (на телефоне его всегда делает система).
#[tauri::command]
fn set_echo_cancellation(state: State<'_, AppState>, enabled: bool) {
    state.tx.set_echo_cancellation(enabled);
}

#[tauri::command]
fn set_noise_suppression(state: State<'_, AppState>, enabled: bool) {
    state.tx.set_noise_suppression(enabled);
}

#[tauri::command]
fn set_vad_threshold(state: State<'_, AppState>, db: f32) {
    state.tx.set_vad_threshold_db(db.clamp(-90.0, 0.0));
}

#[tauri::command]
fn set_mode(app: AppHandle, state: State<'_, AppState>, mode: String) -> CmdResult<()> {
    let mode = match mode.as_str() {
        "voice" => TxMode::Voice,
        "always" => TxMode::Always,
        "ptt" => TxMode::PushToTalk,
        other => return Err(format!("неизвестный режим передачи: {other}")),
    };
    state.tx.set_mode(mode);
    apply_ptt_shortcut(&app, &state)
}

#[tauri::command]
fn set_ptt_key(app: AppHandle, state: State<'_, AppState>, key: Option<String>) -> CmdResult<()> {
    *state.ptt_key.lock().unwrap() = key.filter(|k| !k.is_empty());
    apply_ptt_shortcut(&app, &state)
}

/// На телефоне глобальных клавиш нет: рация там — кнопкой на экране.
#[cfg(mobile)]
fn apply_ptt_shortcut(_app: &AppHandle, state: &AppState) -> CmdResult<()> {
    state.tx.set_ptt_pressed(false);
    Ok(())
}

/// Регистрирует глобальную клавишу рации, только когда выбран режим «рация».
/// Пока клавиша зарегистрирована, Windows отдаёт её только нам: в других
/// программах она работать перестанет. Поэтому в остальных режимах её снимаем.
#[cfg(desktop)]
fn apply_ptt_shortcut(app: &AppHandle, state: &AppState) -> CmdResult<()> {
    let shortcuts = app.global_shortcut();
    shortcuts.unregister_all().map_err(err)?;
    state.tx.set_ptt_pressed(false);

    let key = state.ptt_key.lock().unwrap().clone();
    let (TxMode::PushToTalk, Some(key)) = (state.tx.mode(), key) else {
        return Ok(());
    };
    let tx = state.tx.clone();
    shortcuts
        .on_shortcut(key.as_str(), move |_app, _shortcut, event| {
            tx.set_ptt_pressed(event.state == ShortcutState::Pressed);
        })
        .map_err(|e| format!("не удалось назначить клавишу {key}: {e}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    connected: bool,
    /// Громкость микрофона, dBFS.
    level_db: f32,
    transmitting: bool,
    muted: bool,
    deafened: bool,
    /// Кто из собеседников сейчас говорит.
    speaking: Vec<PeerId>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PeerAudioStats {
    id: PeerId,
    /// Задержка джиттер-буфера, мс.
    delay_ms: f64,
    received: u64,
    late: u64,
    lost: u64,
    trimmed: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AudioStats {
    /// Сколько раз система теряла кусочки звука с микрофона.
    capture_glitches: u64,
    /// Сколько раз воспроизведению не хватило звука.
    playback_glitches: u64,
    capture_device: String,
    capture_rate: u32,
    playback_device: String,
    playback_rate: u32,
    peers: Vec<PeerAudioStats>,
}

/// Диагностика звука для настроек: сбои устройств и счётчики по собеседникам.
#[tauri::command]
async fn audio_stats(state: State<'_, AppState>) -> CmdResult<Option<AudioStats>> {
    let call = state.call.lock().await;
    let Some(call) = call.as_ref() else { return Ok(None) };
    let (capture, playback) = (call.audio.capture(), call.audio.playback());
    let peers = call
        .mixer()
        .stats()
        .into_iter()
        .map(|(id, s, delay_ms)| PeerAudioStats {
            id,
            delay_ms,
            received: s.received,
            late: s.late,
            lost: s.lost,
            trimmed: s.trimmed,
        })
        .collect();
    Ok(Some(AudioStats {
        capture_glitches: capture.glitches.load(Ordering::Relaxed),
        playback_glitches: playback.glitches.load(Ordering::Relaxed),
        capture_device: capture.device_name.clone(),
        capture_rate: capture.config.sample_rate,
        playback_device: playback.device_name.clone(),
        playback_rate: playback.config.sample_rate,
        peers,
    }))
}

/// Интерфейс опрашивает это ~10 раз в секунду для индикаторов.
#[tauri::command]
async fn status(state: State<'_, AppState>) -> CmdResult<Status> {
    let call = state.call.lock().await;
    let tx = &state.tx;
    Ok(Status {
        connected: call.is_some(),
        level_db: tx.level_db(),
        transmitting: call.is_some() && tx.transmitting(),
        muted: tx.muted(),
        deafened: state.deafened.load(Ordering::Relaxed),
        speaking: call.as_ref().map(|c| c.mixer().speaking()).unwrap_or_default(),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(windows)]
    {
        // Нас запустило обновление — дождаться, пока старая копия закроется.
        selfupdate::wait_for_previous();
        shortcut::ensure_start_menu_shortcut();
    }
    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder
        // Первым: второй запуск программы просто показывает уже открытое окно.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_window(app)))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build());
    builder
        // В релизной сборке окна не перезагружаются ни меню по правому клику,
        // ни клавишами (F5, Ctrl+R). В отладочной это нужно для разработки.
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("release-guard")
                .js_init_script(if cfg!(debug_assertions) {
                    String::new()
                } else {
                    include_str!("release_guard.js").to_string()
                })
                .build(),
        )
        .manage(AppState {
            tx: Arc::new(TxState::default()),
            call: tokio::sync::Mutex::new(None),
            deafened: AtomicBool::new(false),
            ptt_key: Mutex::new(None),
            host: tokio::sync::Mutex::new(None),
            rooms: Mutex::new(None),
            avatar: Mutex::new(Vec::new()),
            avatar_file: Mutex::new(None),
            known: Mutex::new(KnownServers::in_memory()),
            generation: AtomicU64::new(0),
            params: Mutex::new(None),
            cues: AtomicBool::new(true),
            close_to_tray: AtomicBool::new(true),
            tray_ready: AtomicBool::new(false),
            mic_test: Mutex::new(None),
        })
        .manage(screen::Screens::default())
        .manage(update::Updates::default())
        .manage(camera::Cameras::default())
        .setup(|app| {
            // Файл с запомненными сертификатами — в папке данных программы.
            #[cfg(target_os = "android")]
            android::init(app);
            // Остальные в комнате видят, у кого программа новее.
            vwf_client::net::set_app_version(format!(
                "{} {}",
                std::env::consts::OS,
                app.package_info().version
            ));
            let data_dir = app.path().app_data_dir()?;
            let state = app.state::<AppState>();
            *state.known.lock().unwrap() = KnownServers::load(data_dir.join("known_servers.txt"));
            *state.rooms.lock().unwrap() = Some(rooms::Rooms::load(&data_dir, "Моя комната"));
            let avatar_file = data_dir.join("avatar.img");
            *state.avatar.lock().unwrap() = std::fs::read(&avatar_file)
                .ok()
                .filter(|data| vwf_client::call::image_type(data).is_some())
                .unwrap_or_default();
            *state.avatar_file.lock().unwrap() = Some(avatar_file);
            // В Linux без библиотеки appindicator создание значка не ошибка,
            // а паника внутри tauri — ловим и её.
            #[cfg(desktop)]
            {
                let handle = app.handle().clone();
                let create = std::panic::AssertUnwindSafe(move || tray::create(&handle));
                match std::panic::catch_unwind(create) {
                    Ok(Ok(())) => app.state::<AppState>().tray_ready.store(true, Ordering::Relaxed),
                    Ok(Err(e)) => eprintln!("значок в трее недоступен: {e}"),
                    Err(_) => eprintln!("значок в трее недоступен"),
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_devices,
            connect,
            trust_server,
            disconnect,
            set_muted,
            set_deafened,
            set_volume,
            set_input_device,
            set_output_device,
            set_vad_threshold,
            set_echo_cancellation,
            set_noise_suppression,
            set_agc,
            set_monitor,
            audio_stats,
            mic_test_start,
            mic_test_status,
            mic_test_stop,
            set_cues,
            set_close_to_tray,
            send_chat,
            get_avatar,
            set_avatar,
            open_url,
            screen::screen_sources,
            screen::start_share,
            screen::stop_share,
            screen::open_viewer,
            screen::subscribe_screen,
            screen::request_keyframe,
            screen::stop_watching,
            update::check_update,
            update::install_update,
            camera::camera_sources,
            camera::start_camera,
            camera::stop_camera,
            camera::subscribe_camera,
            camera::set_camera_layer,
            camera::unsubscribe_camera,
            camera::request_camera_keyframe,
            discover_servers,
            check_rooms,
            set_mode,
            set_ptt_key,
            list_rooms,
            create_room,
            update_room,
            delete_room,
            set_room_lan,
            start_server,
            stop_server,
            set_lock,
            set_internet,
            internet_status,
            set_server_mute,
            kick,
            status,
        ])
        .register_uri_scheme_protocol("screen", |ctx, request| {
            screen::protocol(ctx.app_handle(), request)
        })
        .on_page_load(|webview, payload| {
            // Главное окно загрузилось заново (перезагрузка страницы): интерфейс
            // начал с чистого листа и о звонке не знает — кладём трубку и
            // закрываем свою комнату, а не оставляем их работать без окна.
            if webview.label() == "main" && payload.event() == PageLoadEvent::Started {
                let app = webview.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<AppState>();
                    let busy =
                        state.call.lock().await.is_some() || state.host.lock().await.is_some();
                    if busy {
                        screen::close_all_viewers(&app);
                        *state.params.lock().unwrap() = None;
                        shutdown(&app).await;
                    }
                });
            }
        })
        .on_window_event(|window, event| {
            #[cfg(mobile)]
            let _ = (window, event);
            #[cfg(desktop)]
            on_desktop_window_event(window, event);
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить приложение");
}

#[cfg(desktop)]
fn on_desktop_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    // Окна просмотра чужого экрана закрываются как обычно.
    if let Some(id) = screen::peer_of_window(window.label()) {
        if let tauri::WindowEvent::Destroyed = event {
            screen::viewer_closed(window.app_handle(), id);
        }
        return;
    }
    let tauri::WindowEvent::CloseRequested { api, .. } = event else { return };
    let app = window.app_handle();
    let state = app.state::<AppState>();
    // Есть значок в трее — прячем окно, звонок продолжается.
    if state.close_to_tray.load(Ordering::Relaxed) && state.tray_ready.load(Ordering::Relaxed) {
        api.prevent_close();
        let _ = window.hide();
        return;
    }
    // Иначе аккуратно кладём трубку и останавливаем свой сервер, чтобы
    // остальные сразу увидели, что мы ушли, а не ждали таймаута.
    tauri::async_runtime::block_on(shutdown(app));
}

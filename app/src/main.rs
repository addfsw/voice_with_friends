//! Графический клиент: окно на Tauri (интерфейс — HTML/CSS/JS в `ui/`),
//! а весь звук и сеть — та же библиотека `vwf-client`, что у консольной версии.
//!
//! Интерфейс вызывает команды из этого файла через `invoke(...)` и слушает
//! события комнаты (`peer-joined`, `peer-left`, `call-ended`).

// В release-сборке не показываем консольное окно рядом с приложением.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod host;
mod screen;
mod tray;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use vwf_client::call::{AudioSettings, Call, CallEvent};
use vwf_client::cues::Cue;
use vwf_client::known_servers::KnownServers;
use vwf_client::net::{CertificateChanged, Trust};
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
    /// Сервер, запущенный кнопкой «Создать комнату».
    host: tokio::sync::Mutex<Option<HostedServer>>,
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
    tray_ready: AtomicBool,
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

#[tauri::command]
fn list_devices() -> CmdResult<Devices> {
    let list = audio::device_list().map_err(err)?;
    Ok(Devices {
        inputs: list.inputs,
        outputs: list.outputs,
        default_input: list.default_input,
        default_output: list.default_output,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectArgs {
    server: String,
    name: String,
    /// Пустая строка или `null` — устройство по умолчанию.
    input: Option<String>,
    output: Option<String>,
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
    };
    // Новый звонок — новое поколение: события и переподключение старого
    // звонка, если он ещё где-то в пути, больше ни на что не влияют.
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    *state.params.lock().unwrap() = Some(params.clone());
    start_call(&app, &state, &params, generation).await
}

/// Подключается и кладёт звонок в `state.call`. Если за это время пользователь
/// нажал «Выйти» или подключился заново (сменилось поколение), новый звонок
/// сразу завершается.
///
/// Пока идёт подключение (до 20 с, если сервер не отвечает), `state.call` не
/// заблокирован: иначе кнопки «Выйти», микрофона и т.п. ждали бы его.
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
    let events = app.clone();
    let call = Call::start(
        &params.server,
        &params.name,
        admin_token,
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
                    events.emit("peer-left", PeerLeft { id })
                }
                CallEvent::PeerUpdated(p) => {
                    if !p.sharing {
                        screen::close_viewer(&events, p.id);
                    }
                    events.emit("peer-updated", Peer::from(p))
                }
                CallEvent::Chat(m) => events.emit("chat-message", ChatMessage::from(m)),
                CallEvent::ScreenFrame(frame) => {
                    screen::on_frame(&events, frame);
                    Ok(())
                }
                CallEvent::Viewers(count) => events.emit("share-viewers", ShareViewers { count }),
                CallEvent::ShareStopped(reason) => {
                    events.emit("share-stopped", ShareStopped { reason })
                }
                CallEvent::Ended { reason, lost: true } => {
                    screen::close_all_viewers(&events);
                    spawn_reconnect(events.clone(), generation, reason);
                    Ok(())
                }
                CallEvent::Ended { reason, lost: false } => {
                    screen::close_all_viewers(&events);
                    events.emit("call-ended", CallEnded { reason })
                }
            };
        },
    )
    .await?;
    call.mixer().set_deafened(state.deafened.load(Ordering::Relaxed));
    call.sync_status();

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Hosted {
    info: HostInfo,
    /// Куда подключаться самому хосту.
    local_address: String,
}

/// Запускает сервер на этом компьютере (если ещё не запущен).
#[tauri::command]
async fn start_server(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> CmdResult<Hosted> {
    let mut host = state.host.lock().await;
    if host.is_none() {
        // Сертификат сервера храним в папке данных приложения
        // (на Windows это %APPDATA%\io.github.addfsw.poigrulkiny).
        let data_dir = app.path().app_data_dir().map_err(err)?;
        let name = name.trim();
        let name =
            if name.is_empty() { vwf_server::default_server_name() } else { name.to_string() };
        *host = Some(HostedServer::start(name, data_dir).await.map_err(|e| format!("{e:#}"))?);
    }
    let server = host.as_ref().expect("только что запустили");
    Ok(Hosted { info: server.info.clone(), local_address: server.local_address() })
}

/// Только для своей комнаты: эхо — каждый слышит сам себя (проверка связи).
#[tauri::command]
async fn set_echo(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    if let Some(server) = state.host.lock().await.as_ref() {
        server.set_echo(enabled);
    }
    Ok(())
}

/// Только для своей комнаты: доступ через интернет (пробивка NAT, код приглашения).
#[tauri::command]
async fn set_internet(state: State<'_, AppState>, enabled: bool) -> CmdResult<InternetStatus> {
    let host = state.host.lock().await;
    let server = host.as_ref().ok_or("комната не создана")?;
    server.set_internet(enabled).map_err(|e| format!("{e:#}"))?;
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
            compatible: s.compatible,
        })
        .collect())
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

/// Регистрирует глобальную клавишу рации, только когда выбран режим «рация».
/// Пока клавиша зарегистрирована, Windows отдаёт её только нам: в других
/// программах она работать перестанет. Поэтому в остальных режимах её снимаем.
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

fn main() {
    tauri::Builder::default()
        // Первым: второй запуск программы просто показывает уже открытое окно.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_window(app)))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(AppState {
            tx: Arc::new(TxState::default()),
            call: tokio::sync::Mutex::new(None),
            deafened: AtomicBool::new(false),
            ptt_key: Mutex::new(None),
            host: tokio::sync::Mutex::new(None),
            known: Mutex::new(KnownServers::in_memory()),
            generation: AtomicU64::new(0),
            params: Mutex::new(None),
            cues: AtomicBool::new(true),
            close_to_tray: AtomicBool::new(true),
            tray_ready: AtomicBool::new(false),
        })
        .manage(screen::Frames::default())
        .manage(screen::FrameCounter::default())
        .setup(|app| {
            // Файл с запомненными сертификатами — в папке данных программы.
            let path = app.path().app_data_dir()?.join("known_servers.txt");
            *app.state::<AppState>().known.lock().unwrap() = KnownServers::load(path);
            // В Linux без библиотеки appindicator создание значка не ошибка,
            // а паника внутри tauri — ловим и её.
            let handle = app.handle().clone();
            let create = std::panic::AssertUnwindSafe(move || tray::create(&handle));
            match std::panic::catch_unwind(create) {
                Ok(Ok(())) => app.state::<AppState>().tray_ready.store(true, Ordering::Relaxed),
                Ok(Err(e)) => eprintln!("значок в трее недоступен: {e}"),
                Err(_) => eprintln!("значок в трее недоступен"),
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
            set_noise_suppression,
            set_cues,
            set_close_to_tray,
            send_chat,
            open_url,
            screen::screen_sources,
            screen::start_share,
            screen::stop_share,
            screen::open_viewer,
            discover_servers,
            set_mode,
            set_ptt_key,
            start_server,
            stop_server,
            set_echo,
            set_internet,
            internet_status,
            set_server_mute,
            kick,
            status,
        ])
        .register_uri_scheme_protocol("screen", |ctx, request| {
            screen::protocol(ctx.app_handle(), request)
        })
        .on_window_event(|window, event| {
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
            if state.close_to_tray.load(Ordering::Relaxed)
                && state.tray_ready.load(Ordering::Relaxed)
            {
                api.prevent_close();
                let _ = window.hide();
                return;
            }
            // Иначе аккуратно кладём трубку и останавливаем свой сервер, чтобы
            // остальные сразу увидели, что мы ушли, а не ждали таймаута.
            tauri::async_runtime::block_on(shutdown(app));
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить приложение");
}

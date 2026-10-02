//! Графический клиент: окно на Tauri (интерфейс — HTML/CSS/JS в `ui/`),
//! а весь звук и сеть — та же библиотека `vwf-client`, что у консольной версии.
//!
//! Интерфейс вызывает команды из этого файла через `invoke(...)` и слушает
//! события комнаты (`peer-joined`, `peer-left`, `call-ended`).

// В release-сборке не показываем консольное окно рядом с приложением.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use vwf_client::call::{AudioSettings, Call, CallEvent};
use vwf_client::pipeline::{TxMode, TxState};
use vwf_client::{audio, discovery};
use vwf_proto::{PeerId, PeerInfo};

struct AppState {
    /// Настройки передачи и индикаторы микрофона. Живут всё время работы
    /// программы, поэтому mute, режим и порог не сбрасываются между звонками.
    tx: Arc<TxState>,
    call: tokio::sync::Mutex<Option<Call>>,
    deafened: AtomicBool,
    /// Клавиша рации в формате `Ctrl+Alt+KeyV`, `F8` и т.п.
    ptt_key: Mutex<Option<String>>,
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
}

impl From<PeerInfo> for Peer {
    fn from(p: PeerInfo) -> Self {
        Self { id: p.id, name: p.name }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Joined {
    my_id: PeerId,
    peers: Vec<Peer>,
    input_device: String,
    output_device: String,
}

#[derive(Serialize, Clone)]
struct PeerLeft {
    id: PeerId,
}

#[derive(Serialize, Clone)]
struct CallEnded {
    reason: String,
}

#[tauri::command]
async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    args: ConnectArgs,
) -> CmdResult<Joined> {
    let mut slot = state.call.lock().await;
    if let Some(old) = slot.take() {
        old.hang_up().await;
    }

    let non_empty = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
    let settings = AudioSettings {
        input: non_empty(args.input),
        output: non_empty(args.output),
        ..AudioSettings::default()
    };
    let events = app.clone();
    let call = Call::start(
        args.server.trim(),
        args.name.trim(),
        &settings,
        state.tx.clone(),
        move |event| {
            let _ = match event {
                CallEvent::PeerJoined(p) => events.emit("peer-joined", Peer::from(p)),
                CallEvent::PeerLeft(id) => events.emit("peer-left", PeerLeft { id }),
                CallEvent::Ended(reason) => events.emit("call-ended", CallEnded { reason }),
            };
        },
    )
    .await
    .map_err(|e| format!("{e:#}"))?;
    call.mixer().set_deafened(state.deafened.load(Ordering::Relaxed));

    let joined = Joined {
        my_id: call.my_id,
        peers: call.peers().into_iter().map(Peer::from).collect(),
        input_device: call.audio.capture.device_name.clone(),
        output_device: call.audio.playback.device_name.clone(),
    };
    *slot = Some(call);
    Ok(joined)
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.take() {
        call.hang_up().await;
    }
    Ok(())
}

#[tauri::command]
fn set_muted(state: State<'_, AppState>, muted: bool) {
    state.tx.set_muted(muted);
}

#[tauri::command]
async fn set_deafened(state: State<'_, AppState>, deafened: bool) -> CmdResult<()> {
    state.deafened.store(deafened, Ordering::Relaxed);
    if let Some(call) = state.call.lock().await.as_ref() {
        call.mixer().set_deafened(deafened);
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
    /// Сбои звука с начала звонка (потерянные кусочки).
    mic_glitches: u64,
    output_glitches: u64,
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
        mic_glitches: call.as_ref().map_or(0, |c| c.audio.capture.glitches.load(Ordering::Relaxed)),
        output_glitches: call
            .as_ref()
            .map_or(0, |c| c.audio.playback.glitches.load(Ordering::Relaxed)),
    })
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(AppState {
            tx: Arc::new(TxState::default()),
            call: tokio::sync::Mutex::new(None),
            deafened: AtomicBool::new(false),
            ptt_key: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            list_devices,
            connect,
            disconnect,
            set_muted,
            set_deafened,
            set_volume,
            set_vad_threshold,
            set_noise_suppression,
            discover_servers,
            set_mode,
            set_ptt_key,
            status,
        ])
        .on_window_event(|window, event| {
            // Закрыли окно — аккуратно кладём трубку, чтобы остальные сразу
            // увидели, что мы ушли, а не ждали таймаута.
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let state = window.app_handle().state::<AppState>();
                if let Some(call) = tauri::async_runtime::block_on(state.call.lock()).take() {
                    tauri::async_runtime::block_on(call.hang_up());
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить приложение");
}

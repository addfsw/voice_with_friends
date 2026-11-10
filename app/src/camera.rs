//! Камера в окне программы: своя (превью) и чужие — в плитках участников.
//!
//! Кадры (H.264) плитка получает через канал Tauri и декодирует сама
//! (WebCodecs, `ui/screen-view.js`). Какой слой качества ей нужен, плитка
//! решает по своему размеру и сообщает командой [`set_camera_layer`]: так,
//! как в групповых звонках Telegram, маленькая плитка получает маленький
//! поток, а развёрнутая на весь экран — большой.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Manager, State};
use vwf_client::call::KeyframeRequester;
use vwf_proto::{PeerId, VideoHeader};

use crate::{AppState, CmdResult};

/// Куда отдавать кадры камер.
#[derive(Default)]
pub struct Cameras {
    /// Плитки, которые показывают камеру участника (и свою — по нашему id).
    sinks: Mutex<HashMap<PeerId, Channel<InvokeResponseBody>>>,
    requester: Mutex<Option<KeyframeRequester>>,
}

/// Новый звонок: старые плитки ему не нужны, опорные кадры просим у него.
pub fn set_requester(app: &AppHandle, requester: KeyframeRequester) {
    let cameras = app.state::<Cameras>();
    cameras.sinks.lock().unwrap().clear();
    *cameras.requester.lock().unwrap() = Some(requester);
}

/// Звонок закончился или участник ушёл — его плитки больше нет.
pub fn forget(app: &AppHandle, id: Option<PeerId>) {
    let cameras = app.state::<Cameras>();
    let mut sinks = cameras.sinks.lock().unwrap();
    match id {
        Some(id) => {
            sinks.remove(&id);
        }
        None => sinks.clear(),
    }
}

/// Пришёл кадр камеры (чужой или своё превью).
pub fn on_packet(app: &AppHandle, from: PeerId, header: VideoHeader, data: Vec<u8>) {
    let channel = app.state::<Cameras>().sinks.lock().unwrap().get(&from).cloned();
    let Some(channel) = channel else { return };
    let mut packet = Vec::with_capacity(VideoHeader::LEN + data.len());
    header.encode(&mut packet);
    packet.extend_from_slice(&data);
    let _ = channel.send(InvokeResponseBody::Raw(packet));
}

#[derive(Serialize)]
pub struct CameraEntry {
    id: String,
    name: String,
}

/// Какие камеры есть.
#[tauri::command]
pub async fn camera_sources() -> CmdResult<Vec<CameraEntry>> {
    let list = tauri::async_runtime::spawn_blocking(vwf_client::camera::cameras)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))?;
    Ok(list.into_iter().map(|c| CameraEntry { id: c.id, name: c.name }).collect())
}

/// Включает камеру `id` (пустой — первую).
#[tauri::command]
pub async fn start_camera(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let call = state.call.lock().await;
    let call = call.as_ref().ok_or("нет связи с комнатой")?;
    let id = if id.is_empty() {
        let list = tauri::async_runtime::spawn_blocking(vwf_client::camera::cameras)
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?;
        list.into_iter().next().ok_or("камера не найдена")?.id
    } else {
        id
    };
    call.start_camera(&id).map_err(|e| format!("камера не включилась: {e:#}"))
}

#[tauri::command]
pub async fn stop_camera(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.as_ref() {
        call.stop_camera();
    }
    Ok(())
}

/// Плитка участника `id` готова показывать его камеру в слое `layer`.
/// Своя плитка (наш id) получает превью — слой 0 мимо сети.
#[tauri::command]
pub async fn subscribe_camera(
    app: AppHandle,
    state: State<'_, AppState>,
    id: PeerId,
    layer: u8,
    channel: Channel<InvokeResponseBody>,
) -> CmdResult<()> {
    app.state::<Cameras>().sinks.lock().unwrap().insert(id, channel);
    let call = state.call.lock().await;
    let call = call.as_ref().ok_or("нет связи с комнатой")?;
    if id == call.my_id {
        call.camera_preview_keyframe();
    } else {
        // Начать показ можно только с опорного кадра — его попросит сервер.
        call.watch_camera(id, Some(layer));
    }
    Ok(())
}

/// Плитка стала другого размера (или развернули на весь экран): другой слой.
#[tauri::command]
pub async fn set_camera_layer(state: State<'_, AppState>, id: PeerId, layer: u8) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.as_ref() {
        call.watch_camera(id, Some(layer));
    }
    Ok(())
}

/// Плитку убрали (участник выключил камеру, плитка ушла с экрана).
#[tauri::command]
pub async fn unsubscribe_camera(
    app: AppHandle,
    state: State<'_, AppState>,
    id: PeerId,
) -> CmdResult<()> {
    app.state::<Cameras>().sinks.lock().unwrap().remove(&id);
    if let Some(call) = state.call.lock().await.as_ref() {
        call.watch_camera(id, None);
    }
    Ok(())
}

/// Картинка в плитке сломалась (потерялся кадр) — нужен опорный.
#[tauri::command]
pub async fn request_camera_keyframe(
    app: AppHandle,
    state: State<'_, AppState>,
    id: PeerId,
) -> CmdResult<()> {
    let call = state.call.lock().await;
    if let Some(call) = call.as_ref().filter(|c| c.my_id == id) {
        call.camera_preview_keyframe();
    } else if let Some(requester) = app.state::<Cameras>().requester.lock().unwrap().as_ref() {
        requester.request_camera(id);
    }
    Ok(())
}

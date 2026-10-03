//! Демонстрация экрана в окне программы.
//!
//! Чужой экран открывается в отдельном окне (`screen-<id>`, страница
//! `ui/viewer.html`): его удобно растянуть или унести на второй монитор.
//! Кадры уже декодированы в JPEG (см. `vwf_client::screen::view`); окно
//! узнаёт о новом кадре событием `screen-frame` и загружает картинку по
//! адресу `screen://localhost/<id>/<номер>` — через свой протокол, без
//! перекодирования в base64.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::http::{Request, Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use vwf_client::screen::{ShareSettings, ViewerFrame};
use vwf_proto::PeerId;

use crate::{AppState, CmdResult};

/// Последний кадр каждого, кого мы смотрим.
#[derive(Default)]
pub struct Frames(Mutex<HashMap<PeerId, Arc<Vec<u8>>>>);

/// Метка окна просмотра экрана участника `id`.
pub fn window_label(id: PeerId) -> String {
    format!("screen-{id}")
}

/// Участник из метки окна просмотра.
pub fn peer_of_window(label: &str) -> Option<PeerId> {
    label.strip_prefix("screen-")?.parse().ok()
}

#[derive(Serialize, Clone)]
struct FrameReady {
    id: PeerId,
    n: u64,
    width: u32,
    height: u32,
}

/// Пришёл кадр: запоминаем и сообщаем его окну.
pub fn on_frame(app: &AppHandle, frame: ViewerFrame) {
    let frames = app.state::<Frames>();
    frames.0.lock().unwrap().insert(frame.from, Arc::new(frame.jpeg));
    let n = app.state::<FrameCounter>().0.fetch_add(1, Ordering::Relaxed) + 1;
    let event = FrameReady { id: frame.from, n, width: frame.width, height: frame.height };
    let _ = app.emit_to(window_label(frame.from), "screen-frame", event);
}

/// Номер кадра для адреса картинки: каждый адрес новый, и окно не берёт
/// старый кадр из кэша.
#[derive(Default)]
pub struct FrameCounter(AtomicU64);

/// Участник закончил показ или ушёл: закрываем его окно.
pub fn close_viewer(app: &AppHandle, id: PeerId) {
    app.state::<Frames>().0.lock().unwrap().remove(&id);
    if let Some(window) = app.get_webview_window(&window_label(id)) {
        let _ = window.close();
    }
}

/// Закрывает все окна просмотра (звонок закончился).
pub fn close_all_viewers(app: &AppHandle) {
    app.state::<Frames>().0.lock().unwrap().clear();
    for (label, window) in app.webview_windows() {
        if peer_of_window(&label).is_some() {
            let _ = window.close();
        }
    }
}

/// Отдаёт окну просмотра кадр по адресу `/<id>/<номер>` (косая черта может
/// прийти закодированной: `/3%2F17`, поэтому берём просто первые цифры).
pub fn protocol(app: &AppHandle, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = request.uri().path().trim_start_matches('/');
    let digits: String = path.chars().take_while(char::is_ascii_digit).collect();
    let id = digits.parse().ok();
    let frame = id.and_then(|id: PeerId| app.state::<Frames>().0.lock().unwrap().get(&id).cloned());
    match frame {
        Some(jpeg) => Response::builder()
            .header("Content-Type", "image/jpeg")
            .header("Cache-Control", "no-store")
            .body(jpeg.as_ref().clone())
            .unwrap_or_default(),
        None => {
            Response::builder().status(StatusCode::NOT_FOUND).body(Vec::new()).unwrap_or_default()
        }
    }
}

#[derive(Serialize)]
pub struct SourceInfo {
    id: String,
    name: String,
    window: bool,
}

/// Что можно показать: мониторы и окна.
#[tauri::command]
pub async fn screen_sources() -> CmdResult<Vec<SourceInfo>> {
    let sources = tauri::async_runtime::spawn_blocking(vwf_client::screen::sources)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))?;
    Ok(sources
        .into_iter()
        .map(|s| SourceInfo { id: s.id, name: s.name, window: s.window })
        .collect())
}

/// Начинает показ. `max_height` — 720, 1080 или 0 (как на экране).
#[tauri::command]
pub async fn start_share(
    state: State<'_, AppState>,
    source: String,
    max_height: u32,
    fps: u32,
) -> CmdResult<()> {
    let call = state.call.lock().await;
    let call = call.as_ref().ok_or("нет связи с комнатой")?;
    let settings = ShareSettings { max_height, fps: fps.clamp(5, 60) };
    call.start_share(&source, settings).map_err(|e| format!("не удалось начать показ: {e:#}"))
}

#[tauri::command]
pub async fn stop_share(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(call) = state.call.lock().await.as_ref() {
        call.stop_share();
    }
    Ok(())
}

/// Открывает окно с экраном участника `id` (или показывает уже открытое).
#[tauri::command]
pub async fn open_viewer(
    app: AppHandle,
    state: State<'_, AppState>,
    id: PeerId,
    name: String,
) -> CmdResult<()> {
    let label = window_label(id);
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    {
        let call = state.call.lock().await;
        let call = call.as_ref().ok_or("нет связи с комнатой")?;
        call.watch(id, true);
    }
    let query: String = url_escape(&name);
    WebviewWindowBuilder::new(
        &app,
        &label,
        WebviewUrl::App(format!("viewer.html?id={id}&name={query}").into()),
    )
    .title(format!("{name} — экран · Поигрулькины"))
    .inner_size(1280.0, 760.0)
    .min_inner_size(480.0, 300.0)
    .build()
    .map_err(|e| format!("не удалось открыть окно: {e}"))?;
    Ok(())
}

/// Окно просмотра закрыли — перестаём смотреть.
pub fn viewer_closed(app: &AppHandle, id: PeerId) {
    app.state::<Frames>().0.lock().unwrap().remove(&id);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(call) = app.state::<AppState>().call.lock().await.as_ref() {
            call.watch(id, false);
        }
    });
}

/// Кодирует строку для адреса (имя участника в параметре окна просмотра).
fn url_escape(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

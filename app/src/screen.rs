//! Демонстрация экрана в окне программы.
//!
//! Чужой экран открывается в отдельном окне (`screen-<id>`, страница
//! `ui/viewer.html`): его удобно растянуть или унести на второй монитор.
//! На телефоне отдельных окон нет: там экран показывает слой поверх
//! комнаты в главном окне (`ui/app.js`), кадры те же.
//!
//! Кадры (H.264) окно получает двумя путями:
//! - **напрямую** — через канал Tauri, и декодирует их само видеокартой
//!   (WebCodecs в WebView2). Быстро и почти без нагрузки на процессор;
//! - **через JPEG**, если декодера в окне нет или он не справился: тогда
//!   кадры декодирует [`vwf_client::screen::Viewer`] в JPEG, окно узнаёт о
//!   новом кадре событием `screen-frame` и загружает картинку по адресу
//!   `screen://localhost/<id>/<номер>`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::http::{Request, Response, StatusCode};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager, State};
#[cfg(desktop)]
use tauri::{WebviewUrl, WebviewWindowBuilder};
use vwf_client::call::KeyframeRequester;
use vwf_client::screen::{ShareSettings, Viewer, ViewerFrame};
use vwf_proto::{PeerId, VideoHeader};

use crate::{AppState, CmdResult};

/// Куда отдавать кадры экрана участника.
enum Sink {
    /// Окно декодирует само: кадры как есть, `[заголовок][H.264]`.
    Direct(Channel<InvokeResponseBody>),
    /// Окно показывает JPEG.
    Jpeg,
}

/// Всё о просмотре чужих экранов.
#[derive(Default)]
pub struct Screens {
    sinks: Mutex<HashMap<PeerId, Sink>>,
    /// Последний JPEG-кадр каждого, кого смотрим через JPEG.
    frames: Mutex<HashMap<PeerId, Arc<Vec<u8>>>>,
    /// Номер кадра для адреса картинки: каждый адрес новый, и окно не берёт
    /// старый кадр из кэша.
    counter: AtomicU64,
    /// Декодер в JPEG; запускается, только если он кому-то нужен.
    jpeg: Mutex<Option<Viewer>>,
    /// Как попросить опорный кадр у показывающего (у текущего звонка).
    requester: Mutex<Option<KeyframeRequester>>,
}

impl Screens {
    fn request_keyframe(&self, id: PeerId) {
        if let Some(requester) = self.requester.lock().unwrap().as_ref() {
            requester.request(id);
        }
    }

    fn forget(&self, id: PeerId) {
        self.sinks.lock().unwrap().remove(&id);
        self.frames.lock().unwrap().remove(&id);
        if let Some(viewer) = self.jpeg.lock().unwrap().as_ref() {
            viewer.forget(id);
        }
    }
}

/// Новый звонок: опорные кадры просим уже у него.
pub fn set_requester(app: &AppHandle, requester: KeyframeRequester) {
    *app.state::<Screens>().requester.lock().unwrap() = Some(requester);
}

/// Метка окна просмотра экрана участника `id`.
pub fn window_label(id: PeerId) -> String {
    format!("screen-{id}")
}

/// Участник из метки окна просмотра.
pub fn peer_of_window(label: &str) -> Option<PeerId> {
    label.strip_prefix("screen-")?.parse().ok()
}

/// Пришёл кадр экрана, который мы смотрим.
pub fn on_packet(app: &AppHandle, from: PeerId, header: VideoHeader, data: Vec<u8>) {
    let screens = app.state::<Screens>();
    let direct = match screens.sinks.lock().unwrap().get(&from) {
        Some(Sink::Direct(channel)) => Some(channel.clone()),
        Some(Sink::Jpeg) => None,
        // Окно ещё не открылось — подписавшись, оно само попросит опорный кадр.
        None => return,
    };
    if let Some(channel) = direct {
        let mut packet = Vec::with_capacity(VideoHeader::LEN + data.len());
        header.encode(&mut packet);
        packet.extend_from_slice(&data);
        let _ = channel.send(InvokeResponseBody::Raw(packet));
        return;
    }
    let mut jpeg = screens.jpeg.lock().unwrap();
    if jpeg.is_none() {
        let frames_app = app.clone();
        let keyframe_app = app.clone();
        *jpeg = Viewer::start(
            move |frame| on_frame(&frames_app, frame),
            move |id| keyframe_app.state::<Screens>().request_keyframe(id),
        )
        .map_err(|e| eprintln!("{e:#}"))
        .ok();
    }
    if let Some(viewer) = jpeg.as_ref() {
        // Не успевает — кадр выброшен; декодер заметит пропуск и попросит
        // опорный кадр.
        viewer.push(from, header, data);
    }
}

#[derive(Serialize, Clone)]
struct FrameReady {
    id: PeerId,
    n: u64,
    width: u32,
    height: u32,
}

/// Готов JPEG-кадр: запоминаем и сообщаем его окну.
fn on_frame(app: &AppHandle, frame: ViewerFrame) {
    let screens = app.state::<Screens>();
    screens.frames.lock().unwrap().insert(frame.from, Arc::new(frame.jpeg));
    let n = screens.counter.fetch_add(1, Ordering::Relaxed) + 1;
    let event = FrameReady { id: frame.from, n, width: frame.width, height: frame.height };
    #[cfg(desktop)]
    let target = window_label(frame.from);
    #[cfg(mobile)]
    let target = "main";
    let _ = app.emit_to(target, "screen-frame", event);
}

/// Окно просмотра готово принимать кадры: `direct` — само декодирует H.264
/// (кадры придут в `channel`), иначе — показывает JPEG.
#[tauri::command]
pub fn subscribe_screen(
    app: AppHandle,
    id: PeerId,
    direct: bool,
    channel: Channel<InvokeResponseBody>,
) {
    let screens = app.state::<Screens>();
    let sink = if direct { Sink::Direct(channel) } else { Sink::Jpeg };
    screens.sinks.lock().unwrap().insert(id, sink);
    // Декодеру нужно начать с опорного кадра.
    screens.request_keyframe(id);
}

/// Окно просмотра потеряло кадр — нужен опорный.
#[tauri::command]
pub fn request_keyframe(app: AppHandle, id: PeerId) {
    app.state::<Screens>().request_keyframe(id);
}

/// Участник закончил показ или ушёл: закрываем его окно.
pub fn close_viewer(app: &AppHandle, id: PeerId) {
    app.state::<Screens>().forget(id);
    if let Some(window) = app.get_webview_window(&window_label(id)) {
        let _ = window.close();
    }
}

/// Закрывает все окна просмотра (звонок закончился).
pub fn close_all_viewers(app: &AppHandle) {
    for (label, window) in app.webview_windows() {
        if let Some(id) = peer_of_window(&label) {
            app.state::<Screens>().forget(id);
            let _ = window.close();
        }
    }
}

/// Отдаёт окну просмотра JPEG-кадр по адресу `/<id>/<номер>` (косая черта
/// может прийти закодированной: `/3%2F17`, поэтому берём просто первые цифры).
pub fn protocol(app: &AppHandle, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = request.uri().path().trim_start_matches('/');
    let digits: String = path.chars().take_while(char::is_ascii_digit).collect();
    let id = digits.parse().ok();
    let frame =
        id.and_then(|id: PeerId| app.state::<Screens>().frames.lock().unwrap().get(&id).cloned());
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
    open_viewer_window(app, state, id, name).await
}

/// На телефоне отдельных окон нет: только просим сервер присылать кадры,
/// а показывает их сам интерфейс (он же подписывается на них).
#[cfg(mobile)]
async fn open_viewer_window(
    _app: AppHandle,
    state: State<'_, AppState>,
    id: PeerId,
    _name: String,
) -> CmdResult<()> {
    let call = state.call.lock().await;
    let call = call.as_ref().ok_or("нет связи с комнатой")?;
    call.watch(id, true);
    Ok(())
}

/// Просмотр закрыт (на телефоне — слой в главном окне): кадры больше не нужны.
#[tauri::command]
pub async fn stop_watching(app: AppHandle, id: PeerId) {
    stop(&app, id).await;
}

async fn stop(app: &AppHandle, id: PeerId) {
    app.state::<Screens>().forget(id);
    if let Some(call) = app.state::<AppState>().call.lock().await.as_ref() {
        call.watch(id, false);
    }
}

#[cfg(desktop)]
async fn open_viewer_window(
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
#[cfg(desktop)]
pub fn viewer_closed(app: &AppHandle, id: PeerId) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { stop(&app, id).await });
}

#[cfg(desktop)]
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

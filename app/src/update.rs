//! Обновления. Новые версии собирает GitHub Actions (`.github/workflows/release.yml`)
//! и выкладывает на страницу релизов вместе с файлом `latest.json`. Программа
//! читает его, сравнивает версию со своей и по кнопке скачивает установщик.
//!
//! Установщик подписан ключом автора: открытая часть ключа вшивается в программу
//! при сборке на GitHub (переменная `POIGRULKINY_UPDATER_PUBKEY`), и чужой файл,
//! даже подложенный по правильному адресу, программа не установит. В сборках
//! без ключа (обычный `cargo build`) обновления просто выключены.

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

use crate::CmdResult;

/// Открытый ключ для проверки подписи обновлений.
const PUBKEY: Option<&str> = option_env!("POIGRULKINY_UPDATER_PUBKEY");

/// Где лежит описание последней версии.
const DEFAULT_URL: &str =
    "https://github.com/addfsw/voice_with_friends/releases/latest/download/latest.json";

fn url() -> &'static str {
    option_env!("POIGRULKINY_UPDATE_URL").filter(|u| !u.is_empty()).unwrap_or(DEFAULT_URL)
}

fn pubkey() -> Option<&'static str> {
    PUBKEY.filter(|k| !k.is_empty())
}

fn updater(app: &AppHandle) -> anyhow::Result<tauri_plugin_updater::Updater> {
    let pubkey = pubkey().ok_or_else(|| anyhow::anyhow!("в этой сборке обновления выключены"))?;
    Ok(app.updater_builder().pubkey(pubkey).endpoints(vec![url().parse()?])?.build()?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// Версия этой программы.
    current: String,
    /// Умеет ли эта сборка обновляться (собрана с ключом).
    enabled: bool,
    /// Более новая версия, если есть.
    available: Option<String>,
    /// Что в ней нового.
    notes: Option<String>,
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> CmdResult<UpdateInfo> {
    let current = app.package_info().version.to_string();
    if pubkey().is_none() {
        return Ok(UpdateInfo { current, enabled: false, available: None, notes: None });
    }
    let update = updater(&app)
        .map_err(|e| format!("{e:#}"))?
        .check()
        .await
        .map_err(|e| format!("не удалось проверить обновления: {e}"))?;
    Ok(UpdateInfo {
        current,
        enabled: true,
        available: update.as_ref().map(|u| u.version.clone()),
        notes: update.and_then(|u| u.body),
    })
}

#[derive(Serialize, Clone)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

/// Скачивает и ставит новую версию. Программа при этом закрывается
/// (звонок и своя комната — аккуратно) и запускается уже обновлённой.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> CmdResult<()> {
    let update = updater(&app)
        .map_err(|e| format!("{e:#}"))?
        .check()
        .await
        .map_err(|e| format!("не удалось проверить обновления: {e}"))?
        .ok_or("обновлений нет")?;
    let mut downloaded = 0u64;
    let events = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = events.emit("update-progress", Progress { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(|e| format!("не удалось скачать обновление: {e}"))?;
    // Перед установкой кладём трубку и закрываем свою комнату, чтобы друзья
    // сразу увидели, что мы вышли, а не ждали таймаута.
    crate::shutdown(&app).await;
    update.install(bytes).map_err(|e| format!("не удалось установить обновление: {e}"))?;
    // На Windows установщик сам закрывает программу и запускает новую версию.
    app.restart();
}

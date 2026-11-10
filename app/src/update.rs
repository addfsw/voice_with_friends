//! Обновление программы из последнего релиза на GitHub.
//!
//! Релиз собирает GitHub Actions (`.github/workflows/android.yml`): в нём
//! установщик для Windows, APK и `latest.json` — номер версии, описание и
//! ссылки на файлы. Проверка — один запрос за `latest.json`.
//!
//! - **Компьютер:** скачивает и ставит `tauri-plugin-updater`. Он проверяет
//!   подпись установщика открытым ключом из `tauri.conf.json`, так что
//!   подменённый файл не установится. Установщик NSIS ставит программу
//!   поверх и запускает её снова.
//! - **Android:** APK скачивает и открывает системный установщик
//!   `MainActivity.kt`. Подпись проверяет сам Android: поверх установленной
//!   программы встанет только APK, подписанный тем же ключом.

use serde::Serialize;
use tauri::AppHandle;

/// `latest.json` последнего релиза. GitHub отдаёт его без входа в аккаунт,
/// только пока репозиторий открытый.
const LATEST_JSON: &str =
    "https://github.com/addfsw/voice_with_friends/releases/latest/download/latest.json";

/// Найденное обновление — для окна.
#[derive(Serialize, Clone)]
pub struct UpdateInfo {
    version: String,
    notes: String,
    /// Android: откуда скачать APK.
    #[serde(skip_serializing_if = "Option::is_none")]
    apk: Option<String>,
}

/// Найденное, но ещё не установленное обновление (на компьютере).
#[derive(Default)]
pub struct Updates {
    #[cfg(desktop)]
    pending: tokio::sync::Mutex<Option<tauri_plugin_updater::Update>>,
}

/// Ход скачивания для окна: сколько байт скачано и сколько всего.
#[cfg(desktop)]
#[derive(Serialize, Clone)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

/// Есть ли версия новее установленной. `None` — уже последняя.
#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
) -> Result<Option<UpdateInfo>, String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_updater::UpdaterExt;
        let url = LATEST_JSON.parse().expect("адрес latest.json");
        let updater = app
            .updater_builder()
            .endpoints(vec![url])
            .and_then(|builder| builder.build())
            .map_err(|e| format!("не удалось проверить обновления: {e}"))?;
        let update = updater.check().await.map_err(describe)?;
        let info = update.as_ref().map(|update| UpdateInfo {
            version: update.version.clone(),
            notes: update.body.clone().unwrap_or_default(),
            apk: None,
        });
        *updates.pending.lock().await = update;
        Ok(info)
    }
    #[cfg(mobile)]
    {
        let _ = updates;
        android_check(&app).await.map_err(|e| format!("не удалось проверить обновления: {e:#}"))
    }
}

/// Скачивает и ставит найденное обновление. Перед установкой кладёт трубку
/// и закрывает свою комнату: установщик закроет программу и откроет снова.
#[cfg(desktop)]
#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
) -> Result<(), String> {
    use tauri::Emitter;
    let Some(update) = updates.pending.lock().await.clone() else {
        return Err("сначала проверьте обновления".into());
    };
    let mut downloaded = 0u64;
    let progress_app = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress_app.emit("update-progress", Progress { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(|e| format!("не удалось скачать обновление: {e}"))?;
    crate::shutdown(&app).await;
    // На Windows install запускает установщик и сразу завершает программу.
    update.install(bytes).map_err(|e| format!("не удалось установить обновление: {e}"))?;
    app.restart()
}

/// На Android APK ставит MainActivity.kt (`installUpdate` из окна).
#[cfg(mobile)]
#[tauri::command]
pub async fn install_update() -> Result<(), String> {
    Err("на Android обновление ставит системный установщик".into())
}

/// Ошибка проверки понятными словами.
#[cfg(desktop)]
fn describe(e: tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error;
    match e {
        // 404: релизов ещё нет или репозиторий закрыт.
        Error::ReleaseNotFound => "на GitHub пока нет опубликованных обновлений".into(),
        Error::TargetNotFound(_) | Error::TargetsNotFound(_) => {
            "в последнем релизе нет версии для этой системы".into()
        }
        e => format!("не удалось проверить обновления: {e}"),
    }
}

#[cfg(mobile)]
async fn android_check(app: &AppHandle) -> anyhow::Result<Option<UpdateInfo>> {
    use std::time::Duration;

    use anyhow::{Context, bail};

    let response = vwf_nat::http_client()?
        .get(LATEST_JSON)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("нет связи с GitHub")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("на GitHub пока нет опубликованных обновлений");
    }
    let text = response.error_for_status()?.text().await?;
    android_update(&text, &app.package_info().version.to_string())
}

/// Разбирает `latest.json` (его пишет workflow, задача `release`): есть ли
/// APK новее версии `current`.
#[cfg_attr(desktop, allow(dead_code))]
fn android_update(latest_json: &str, current: &str) -> anyhow::Result<Option<UpdateInfo>> {
    use std::collections::HashMap;

    use anyhow::{Context, bail};

    #[derive(serde::Deserialize)]
    struct Latest {
        version: String,
        #[serde(default)]
        notes: String,
        #[serde(default)]
        platforms: HashMap<String, Platform>,
    }
    #[derive(serde::Deserialize)]
    struct Platform {
        url: String,
    }

    let latest: Latest = serde_json::from_str(latest_json).context("непонятный ответ GitHub")?;
    let version = semver::Version::parse(latest.version.trim_start_matches('v'))
        .context("непонятный номер версии в релизе")?;
    if version <= semver::Version::parse(current)? {
        return Ok(None);
    }
    let Some(apk) = latest.platforms.get("android-aarch64") else {
        bail!("в последнем релизе нет версии для Android");
    };
    Ok(Some(UpdateInfo { version: version.to_string(), notes: latest.notes, apk: Some(apk.url.clone()) }))
}

#[cfg(test)]
mod tests {
    use super::android_update;

    /// Так latest.json пишет workflow (.github/workflows/android.yml).
    const LATEST: &str = r#"{
      "version": "0.1.57",
      "notes": "Обновление из программы",
      "pub_date": "2026-11-10T12:00:00Z",
      "platforms": {
        "windows-x86_64": {
          "url": "https://github.com/addfsw/voice_with_friends/releases/download/v0.1.57/Poigrulkiny-setup.exe",
          "signature": "dW50cnVzdGVk"
        },
        "android-aarch64": {
          "url": "https://github.com/addfsw/voice_with_friends/releases/download/v0.1.57/Poigrulkiny.apk",
          "signature": ""
        }
      }
    }"#;

    #[test]
    fn newer_apk() {
        let update = android_update(LATEST, "0.1.9").unwrap().expect("0.1.57 новее 0.1.9");
        assert_eq!(update.version, "0.1.57");
        assert_eq!(update.notes, "Обновление из программы");
        assert!(update.apk.unwrap().ends_with("/v0.1.57/Poigrulkiny.apk"));
    }

    #[test]
    fn same_or_older() {
        assert!(android_update(LATEST, "0.1.57").unwrap().is_none());
        assert!(android_update(LATEST, "0.2.0").unwrap().is_none());
    }

    #[test]
    fn no_android_in_release() {
        let latest = r#"{"version": "0.1.60", "platforms": {}}"#;
        assert!(android_update(latest, "0.1.57").is_err());
    }

    /// Windows-часть того же файла понимает tauri-plugin-updater.
    #[cfg(desktop)]
    #[test]
    fn updater_reads_manifest() {
        let release: tauri_plugin_updater::RemoteRelease = serde_json::from_str(LATEST).unwrap();
        assert_eq!(release.version.to_string(), "0.1.57");
        assert!(release.download_url("windows-x86_64").unwrap().as_str().ends_with("Poigrulkiny-setup.exe"));
        assert_eq!(release.signature("windows-x86_64").unwrap(), "dW50cnVzdGVk");
    }
}

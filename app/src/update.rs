//! Обновление программы из последнего релиза на GitHub.
//!
//! Релиз собирает GitHub Actions (`.github/workflows/android.yml`): в нём
//! `Poigrulkiny.exe` с подписью, установщик для Windows, APK и `latest.json`
//! — номер версии, описание и ссылки на файлы. Проверка — один запрос за
//! `latest.json`.
//!
//! - **Windows:** обновляется сама запущенная программа, где бы она ни лежала
//!   ([`crate::selfupdate`]): скачиваем новый exe, проверяем подпись,
//!   подменяем им свой файл и перезапускаемся. Ничего не ставится в другую
//!   папку, ярлыки и привычный способ запуска не меняются.
//! - **Android:** APK скачивает и открывает системный установщик
//!   `MainActivity.kt`. Подпись проверяет сам Android: поверх установленной
//!   программы встанет только APK, подписанный тем же ключом.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

/// `latest.json` последнего релиза. GitHub отдаёт его без входа в аккаунт,
/// только пока репозиторий открытый. `VWF_UPDATE_URL` — другой адрес (для
/// проверки обновления на своём сервере).
const LATEST_JSON: &str =
    "https://github.com/addfsw/voice_with_friends/releases/latest/download/latest.json";

fn latest_url() -> String {
    std::env::var("VWF_UPDATE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| LATEST_JSON.to_string())
}

/// Под каким именем в `latest.json` лежит файл для этой системы. Старые
/// версии (0.1.63–0.1.66) на Windows берут `windows-x86_64` — установщик;
/// новые — `windows-x86_64-exe`, саму программу.
fn platform_key() -> Option<&'static str> {
    if cfg!(target_os = "android") {
        Some("android-aarch64")
    } else if cfg!(windows) {
        Some("windows-x86_64-exe")
    } else {
        None
    }
}

/// Найденное обновление — для окна.
#[derive(Serialize, Clone)]
pub struct UpdateInfo {
    version: String,
    notes: String,
    /// Android: откуда скачать APK.
    #[serde(skip_serializing_if = "Option::is_none")]
    apk: Option<String>,
}

/// Версия новее установленной: что показать и откуда скачать.
#[derive(Debug, Clone, PartialEq)]
struct Release {
    version: String,
    notes: String,
    url: String,
    signature: String,
}

/// Найденное, но ещё не установленное обновление.
#[derive(Default)]
pub struct Updates {
    #[cfg_attr(not(windows), allow(dead_code))]
    pending: std::sync::Mutex<Option<Release>>,
}

/// Ход скачивания для окна: сколько байт скачано и сколько всего.
#[cfg(windows)]
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
    let release = fetch(&app.package_info().version.to_string())
        .await
        .map_err(|e| format!("{e:#}"))?;
    let info = release.as_ref().map(|r| UpdateInfo {
        version: r.version.clone(),
        notes: r.notes.clone(),
        apk: cfg!(target_os = "android").then(|| r.url.clone()),
    });
    *updates.pending.lock().unwrap() = release;
    Ok(info)
}

async fn fetch(current: &str) -> anyhow::Result<Option<Release>> {
    use anyhow::{Context, bail};

    let Some(key) = platform_key() else {
        bail!("обновление из программы есть только на Windows и Android");
    };
    let response = vwf_nat::http_client()?
        .get(latest_url())
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .context("не удалось проверить обновления: нет связи с GitHub")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("на GitHub пока нет опубликованных обновлений");
    }
    let text = response.error_for_status()?.text().await?;
    find_release(&text, key, current)
}

/// Разбирает `latest.json` (его пишет workflow, задача `release`): есть ли
/// версия новее `current` для системы `key`.
fn find_release(latest_json: &str, key: &str, current: &str) -> anyhow::Result<Option<Release>> {
    use anyhow::{Context, bail};

    #[derive(Deserialize)]
    struct Latest {
        version: String,
        #[serde(default)]
        notes: String,
        #[serde(default)]
        platforms: HashMap<String, Platform>,
    }
    #[derive(Deserialize)]
    struct Platform {
        url: String,
        #[serde(default)]
        signature: String,
    }

    let latest: Latest = serde_json::from_str(latest_json).context("непонятный ответ GitHub")?;
    let version = semver::Version::parse(latest.version.trim_start_matches('v'))
        .context("непонятный номер версии в релизе")?;
    if version <= semver::Version::parse(current)? {
        return Ok(None);
    }
    let Some(file) = latest.platforms.get(key) else {
        bail!("в последнем релизе нет версии для этой системы");
    };
    Ok(Some(Release {
        version: version.to_string(),
        notes: latest.notes,
        url: file.url.clone(),
        signature: file.signature.clone(),
    }))
}

/// Скачивает новую программу, проверяет подпись, кладёт трубку и закрывает
/// свою комнату, подменяет запущенный exe и перезапускается.
#[cfg(windows)]
#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
) -> Result<(), String> {
    use tauri::Emitter;

    let Some(release) = updates.pending.lock().unwrap().clone() else {
        return Err("сначала проверьте обновления".into());
    };
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .ok_or("в программе нет ключа для проверки обновлений")?
        .to_string();
    let progress_app = app.clone();
    let data = crate::selfupdate::download(&release.url, move |downloaded, total| {
        let _ = progress_app.emit("update-progress", Progress { downloaded, total });
    })
    .await
    .map_err(|e| format!("не удалось скачать обновление: {e:#}"))?;
    crate::selfupdate::verify(&data, &release.signature, &pubkey).map_err(|e| format!("{e:#}"))?;
    crate::shutdown(&app).await;
    crate::selfupdate::replace_self(&data).map_err(|e| format!("{e:#}"))?;
    crate::selfupdate::restart()
}

/// Android: APK ставит MainActivity.kt (`installUpdate` из окна). Linux:
/// обновления из программы нет.
#[cfg(not(windows))]
#[tauri::command]
pub async fn install_update() -> Result<(), String> {
    Err("здесь обновление ставится не из программы".into())
}

#[cfg(test)]
mod tests {
    use super::find_release;

    /// Так latest.json пишет workflow (.github/workflows/android.yml).
    const LATEST: &str = r#"{
      "version": "0.1.67",
      "notes": "Камера",
      "pub_date": "2026-11-11T12:00:00Z",
      "platforms": {
        "windows-x86_64": {
          "url": "https://github.com/addfsw/voice_with_friends/releases/download/v0.1.67/Poigrulkiny-setup.exe",
          "signature": "c2V0dXA="
        },
        "windows-x86_64-exe": {
          "url": "https://github.com/addfsw/voice_with_friends/releases/download/v0.1.67/Poigrulkiny.exe",
          "signature": "ZXhl"
        },
        "android-aarch64": {
          "url": "https://github.com/addfsw/voice_with_friends/releases/download/v0.1.67/Poigrulkiny.apk",
          "signature": ""
        }
      }
    }"#;

    #[test]
    fn windows_takes_the_program_itself() {
        let release = find_release(LATEST, "windows-x86_64-exe", "0.1.66").unwrap().unwrap();
        assert_eq!(release.version, "0.1.67");
        assert!(release.url.ends_with("/v0.1.67/Poigrulkiny.exe"));
        assert_eq!(release.signature, "ZXhl");
    }

    #[test]
    fn android_takes_apk() {
        let release = find_release(LATEST, "android-aarch64", "0.1.9").unwrap().unwrap();
        assert_eq!(release.notes, "Камера");
        assert!(release.url.ends_with("/v0.1.67/Poigrulkiny.apk"));
    }

    #[test]
    fn same_or_older() {
        assert!(find_release(LATEST, "windows-x86_64-exe", "0.1.67").unwrap().is_none());
        assert!(find_release(LATEST, "android-aarch64", "0.2.0").unwrap().is_none());
    }

    #[test]
    fn no_file_for_this_system() {
        let latest = r#"{"version": "0.1.70", "platforms": {}}"#;
        assert!(find_release(latest, "windows-x86_64-exe", "0.1.67").is_err());
    }

    /// Установщик для старых версий (0.1.63–0.1.66) по-прежнему понимает
    /// tauri-plugin-updater: ключ `windows-x86_64` с url и подписью.
    #[test]
    fn old_versions_still_find_the_installer() {
        let json: serde_json::Value = serde_json::from_str(LATEST).unwrap();
        let old = &json["platforms"]["windows-x86_64"];
        assert!(old["url"].as_str().unwrap().ends_with("Poigrulkiny-setup.exe"));
        assert!(!old["signature"].as_str().unwrap().is_empty());
    }
}

//! Windows: обновление на месте. Скачиваем новый `Poigrulkiny.exe`,
//! проверяем подпись и подменяем им тот exe, который сейчас запущен, — где бы
//! он ни лежал (рабочий стол, «Загрузки», папка установки). Затем программа
//! запускается заново, уже новая.
//!
//! Подпись — minisign, тот же ключ, что у установщика (`tauri signer`):
//! открытый ключ в `tauri.conf.json` (`plugins.updater.pubkey`), закрытый —
//! в секрете сборки. Подменённый по дороге файл не пройдёт проверку.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine;

/// Переменная окружения для нового процесса: номер старого, которого надо
/// дождаться. Иначе новый увидел бы старый ещё живым и закрылся (одна копия
/// программы).
const WAIT_PID: &str = "VWF_WAIT_PID";

/// Скачивает файл, сообщая ход: `(скачано, всего)`.
pub async fn download(
    url: &str,
    mut progress: impl FnMut(u64, Option<u64>),
) -> anyhow::Result<Vec<u8>> {
    let mut response = vwf_nat::http_client()?
        .get(url)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .context("нет связи с GitHub")?
        .error_for_status()?;
    let total = response.content_length();
    let mut data = Vec::with_capacity(total.unwrap_or(0) as usize);
    while let Some(chunk) = response.chunk().await.context("скачивание прервалось")? {
        data.extend_from_slice(&chunk);
        progress(data.len() as u64, total);
    }
    Ok(data)
}

/// Проверяет подпись `signature` (как её пишет `tauri signer sign`: base64
/// от текста подписи minisign) открытым ключом `pubkey` (тоже base64).
pub fn verify(data: &[u8], signature: &str, pubkey: &str) -> anyhow::Result<()> {
    let text = |b64: &str| -> anyhow::Result<String> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())?;
        Ok(String::from_utf8(bytes)?)
    };
    let key = minisign_verify::PublicKey::decode(&text(pubkey)?)
        .map_err(|e| anyhow::anyhow!("открытый ключ не читается: {e}"))?;
    let signature = minisign_verify::Signature::decode(&text(signature)?)
        .map_err(|e| anyhow::anyhow!("подпись не читается: {e}"))?;
    key.verify(data, &signature, true)
        .map_err(|_| anyhow::anyhow!("подпись обновления не сходится — файл не наш"))
}

/// Подменяет запущенный exe новым (`data`). Сам запущенный файл Windows
/// удалить не даёт, но переименовать — можно; этим и пользуется `self-replace`.
pub fn replace_self(data: &[u8]) -> anyhow::Result<()> {
    if data.len() < 1024 * 1024 || !data.starts_with(b"MZ") {
        bail!("скачанный файл — не программа");
    }
    let current = std::env::current_exe().context("не знаю, где лежит программа")?;
    // Новый файл — рядом со старым: та же папка, тот же диск, права те же.
    let dir = current.parent().map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let staged = dir.join(format!(".poigrulkiny-update-{}.exe", std::process::id()));
    std::fs::write(&staged, data).with_context(|| {
        format!("нет прав записать в папку {} — переложите программу туда, где можно", dir.display())
    })?;
    let result = self_replace::self_replace(&staged)
        .context("не удалось заменить программу новой версией");
    let _ = std::fs::remove_file(&staged);
    result
}

/// Запускает программу заново (уже новый exe) и выходит.
pub fn restart() -> ! {
    if let Ok(exe) = std::env::current_exe() {
        let mut command = std::process::Command::new(exe);
        command.args(std::env::args_os().skip(1)).env(WAIT_PID, std::process::id().to_string());
        let _ = command.spawn();
    }
    std::process::exit(0);
}

/// В начале запуска: если нас запустило обновление — ждём, пока старая
/// копия закроется (не дольше 10 секунд).
pub fn wait_for_previous() {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

    let Some(pid) = std::env::var(WAIT_PID).ok().and_then(|v| v.parse::<u32>().ok()) else {
        return;
    };
    // SAFETY: в переменной окружения теперь только наш процесс; дескриптор
    // процесса закрываем сразу после ожидания.
    unsafe {
        std::env::remove_var(WAIT_PID);
        if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(handle, 10_000);
            let _ = CloseHandle(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::verify;

    /// Подписано настоящим ключом обновлений (`tauri signer sign`).
    const DATA: &[u8] = include_bytes!("../tests/update-signature/sample.txt");
    const SIGNATURE: &str = include_str!("../tests/update-signature/sample.txt.sig");

    fn pubkey() -> String {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        conf["plugins"]["updater"]["pubkey"].as_str().unwrap().to_string()
    }

    #[test]
    fn signed_file_passes() {
        verify(DATA, SIGNATURE, &pubkey()).unwrap();
    }

    #[test]
    fn changed_file_fails() {
        let mut changed = DATA.to_vec();
        changed[0] ^= 1;
        assert!(verify(&changed, SIGNATURE, &pubkey()).is_err());
    }
}

//! Windows: ярлык в «Пуске» для установленной программы. Установщик в режиме
//! обновления (так ставят себя версии 0.1.63–0.1.66) ярлыков не создаёт —
//! поэтому установленная программа при запуске сама кладёт ярлык, если его нет.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IPersistFile,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::Shell::{
    FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink,
};
use windows::core::{HSTRING, Interface, w};

/// Название программы — как `productName` в `tauri.conf.json`: так называются
/// папка установки, ключ в реестре и ярлык.
const PRODUCT: &str = "Поигрулькины";

/// Где лежит установленная программа: путь записывает установщик в реестр
/// (`DisplayIcon`), а если записи нет — папка по умолчанию.
fn installed_exe() -> Option<PathBuf> {
    registry_exe().or_else(|| {
        let base = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(base).join(PRODUCT).join("poigrulkiny.exe"))
    })
}

fn registry_exe() -> Option<PathBuf> {
    let key = HSTRING::from(format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{PRODUCT}"));
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: буфер и его размер в байтах — наши, функция пишет не больше `size`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            w!("DisplayIcon"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    let text = String::from_utf16_lossy(&buf[..len]);
    let path = text.trim().trim_matches('"');
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Один ли это файл (пути бывают в разном регистре и виде).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase(),
        _ => false,
    }
}

/// Установленная программа: если в «Пуске» нет ярлыка, кладём его (в фоне,
/// ошибки не важны — программа работает и без ярлыка).
pub fn ensure_start_menu_shortcut() {
    if cfg!(debug_assertions) {
        return;
    }
    let (Ok(current), Some(installed)) = (std::env::current_exe(), installed_exe()) else {
        return;
    };
    if !same_file(&current, &installed) {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = create_shortcut(&installed) {
            eprintln!("ярлык в «Пуске» не создан: {e}");
        }
    });
}

fn create_shortcut(exe: &Path) -> windows::core::Result<()> {
    // SAFETY: COM запускается в этом (отдельном) потоке и там же закрывается;
    // строка из SHGetKnownFolderPath освобождается сразу после копирования.
    unsafe {
        let folder = SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None)?;
        let programs = PathBuf::from(folder.to_string().unwrap_or_default());
        CoTaskMemFree(Some(folder.0.cast()));
        let lnk = programs.join(format!("{PRODUCT}.lnk"));
        if programs.as_os_str().is_empty() || lnk.exists() {
            return Ok(());
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            if let Some(dir) = exe.parent() {
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            link.SetDescription(w!("Голосовой чат для своих"))?;
            link.cast::<IPersistFile>()?.Save(&HSTRING::from(lnk.as_os_str()), true)
        })();
        CoUninitialize();
        result
    }
}

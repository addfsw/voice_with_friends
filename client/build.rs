//! Проверка перед сборкой: есть ли ассемблер NASM.
//!
//! Кодек H.264 (OpenH264) собирается из исходников. Если NASM не найден, он
//! молча собирается без ассемблерных оптимизаций и сжимает кадр в 3–5 раз
//! медленнее — демонстрация экрана в 1080p выдаёт тогда около 10 к/с.
//! Поэтому без NASM сборка останавливается и объясняет, что не так.
//!
//! Сборка OpenH264 ищет NASM только в переменной `NASM` (путь к nasm.exe)
//! и в `PATH`, так что и мы проверяем ровно это.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=NASM");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=POIGRULKINY_SLOW_H264");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if !matches!(arch.as_str(), "x86" | "x86_64") || env::var_os("POIGRULKINY_SLOW_H264").is_some()
    {
        return;
    }

    let exe = if cfg!(windows) { "nasm.exe" } else { "nasm" };
    let from_env = env::var_os("NASM");
    if let Some(value) = &from_env {
        if Path::new(value).is_file() {
            return;
        }
    } else if in_path(exe).is_some() {
        return;
    }

    // Что не так с переменной NASM.
    let problem = match &from_env {
        Some(value) if Path::new(value).is_dir() => format!(
            "Переменная NASM = {} — это папка, а нужен путь к самому файлу {exe}.",
            show(value)
        ),
        Some(value) => format!("Переменная NASM = {}, но такого файла нет.", show(value)),
        None => "NASM не найден: переменная NASM не задана, и в PATH его тоже нет.".to_string(),
    };

    // Где NASM обычно лежит на Windows.
    let installed = if cfg!(windows) { installed_nasm() } else { None };
    let fix = match installed {
        Some(path) => format!(
            "NASM установлен здесь: {}\nВыполните (в PowerShell или cmd):\n\n    setx NASM \"{}\"\n",
            path.display(),
            path.display()
        ),
        None if cfg!(windows) => "Установите NASM:\n\n    winget install NASM.NASM\n\n\
             Затем найдите nasm.exe (обычно C:\\Program Files\\NASM или\n\
             %LOCALAPPDATA%\\bin\\NASM) и выполните setx NASM \"полный\\путь\\к\\nasm.exe\".\n"
            .to_string(),
        None => {
            "Установите NASM: sudo apt install nasm (или аналог для вашей системы).\n".to_string()
        }
    };

    panic!(
        "для демонстрации экрана нужен ассемблер NASM — без него кодек H.264 работает \
         в 3–5 раз медленнее.\n\n{problem}\n\n{fix}\n\
         ВАЖНО: setx действует только в НОВЫХ окнах. Закройте терминал (если собираете \
         из VS Code — перезапустите VS Code целиком) и в новом окне:\n\n\
         \x20   cargo clean -p openh264-sys2\n\
         \x20   cargo build --release\n\n\
         (Собрать без NASM, с медленной демонстрацией, можно с переменной \
         POIGRULKINY_SLOW_H264=1.)\n"
    );
}

fn in_path(exe: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?).map(|dir| dir.join(exe)).find(|p| p.is_file())
}

/// Обычные места установки NASM на Windows: для всех пользователей
/// (установщик запущен от администратора) и только для себя.
fn installed_nasm() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(dir) = env::var_os(var) {
            candidates.push(PathBuf::from(dir).join("NASM").join("nasm.exe"));
        }
    }
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(&local).join("bin").join("NASM").join("nasm.exe"));
        candidates.push(PathBuf::from(&local).join("Programs").join("NASM").join("nasm.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn show(value: &OsString) -> String {
    format!("\"{}\"", value.to_string_lossy())
}

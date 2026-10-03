//! Проверка перед сборкой: есть ли ассемблер NASM.
//!
//! Кодек H.264 (OpenH264) собирается из исходников. Если NASM не найден, он
//! молча собирается без ассемблерных оптимизаций и сжимает кадр в 3–5 раз
//! медленнее — демонстрация экрана в 1080p выдаёт тогда около 10 к/с.
//! Поэтому без NASM сборка останавливается с инструкцией.

use std::env;
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
    if find_nasm().is_some() {
        return;
    }

    let windows = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let default = Path::new(r"C:\Program Files\NASM\nasm.exe");
    let how = if windows && default.exists() {
        format!(
            "NASM установлен, но сборка его не видит. Выполните:\n\
             \n    setx NASM \"{}\"\n",
            default.display()
        )
    } else if windows {
        "Установите его:\n\
         \n    winget install NASM.NASM\
         \n    setx NASM \"C:\\Program Files\\NASM\\nasm.exe\"\n"
            .to_string()
    } else {
        "Установите его: sudo apt install nasm (или аналог для вашей системы).\n".to_string()
    };
    panic!(
        "\n\nДля демонстрации экрана нужен ассемблер NASM: без него кодек H.264 \
         работает в 3–5 раз медленнее.\n{how}\n\
         Затем откройте НОВОЕ окно терминала и пересоберите кодек:\n\
         \n    cargo clean -p openh264-sys2\
         \n    cargo build --release\n\n\
         (Собрать без NASM можно, задав переменную POIGRULKINY_SLOW_H264=1.)\n\n"
    );
}

/// Ищет NASM так же, как сборка OpenH264: переменная NASM или PATH.
fn find_nasm() -> Option<PathBuf> {
    if let Some(path) = env::var_os("NASM") {
        let path = PathBuf::from(path);
        return path.exists().then_some(path);
    }
    let exe = if cfg!(windows) { "nasm.exe" } else { "nasm" };
    env::split_paths(&env::var_os("PATH")?).map(|dir| dir.join(exe)).find(|p| p.is_file())
}

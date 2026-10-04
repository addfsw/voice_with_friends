//! Точка входа на компьютере. Сама программа — в `lib.rs`: на Android её
//! запускает не `main`, а Android-обёртка (см. `tauri::mobile_entry_point`).

// В release-сборке не показываем консольное окно рядом с приложением.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    vwf_app_lib::run();
}

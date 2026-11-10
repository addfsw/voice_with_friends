//! Android: звуковой библиотеке (cpal) нужны Java-машина и Android-контекст —
//! через них она спрашивает у системы список микрофонов и динамиков. Tauri
//! их сам не передаёт, поэтому при запуске берём их у WebView и кладём в
//! `ndk_context`. Пока этого не случилось, к звуку обращаться нельзя:
//! [`wait_ready`] ждёт.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use tauri::Manager;

static READY: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// Передаёт Java-машину и Activity звуковой библиотеке (один раз за запуск).
pub fn init(app: &tauri::App) {
    let Some(window) = app.get_webview_window("main") else {
        eprintln!("android: нет главного окна, звук будет недоступен");
        return;
    };
    let result = window.with_webview(|webview| {
        webview.jni_handle().exec(|env, activity, _webview| {
            let (Ok(vm), Ok(activity)) = (env.get_java_vm(), env.new_global_ref(activity)) else {
                eprintln!("android: не удалось получить JavaVM или Activity");
                return;
            };
            // SAFETY: указатели живут всё время работы процесса: JavaVM
            // одна, а глобальную ссылку на Activity мы намеренно не
            // освобождаем.
            unsafe {
                ndk_context::initialize_android_context(
                    vm.get_java_vm_pointer().cast(),
                    activity.as_obj().as_raw().cast(),
                );
            }
            std::mem::forget(activity);
            let (ready, changed) = &READY;
            *ready.lock().unwrap() = true;
            changed.notify_all();
        });
    });
    if let Err(e) = result {
        eprintln!("android: {e}");
    }
}

/// Ждёт, пока звуковая библиотека получит Android-контекст (обычно он
/// готов раньше, чем интерфейс успевает что-то попросить).
pub fn wait_ready() -> Result<(), String> {
    let (ready, changed) = &READY;
    let guard = ready.lock().unwrap();
    let (guard, _) =
        changed.wait_timeout_while(guard, Duration::from_secs(10), |ready| !*ready).unwrap();
    if *guard {
        Ok(())
    } else {
        Err("звук ещё не готов: перезапустите программу".into())
    }
}

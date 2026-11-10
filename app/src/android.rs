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

/// Кадр с камеры телефона (`CameraCapture.kt`, CameraX `YUV_420_888`):
/// собираем I420 и отдаём камере звонка. Плоскости читаем прямо из памяти
/// Java (direct `ByteBuffer`), без копирования в Kotlin.
///
/// Паника не должна уйти в Java — там это падение всей программы.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_io_github_addfsw_poigrulkiny_CameraCapture_nativeCameraFrame(
    env: jni::JNIEnv,
    _this: jni::objects::JObject,
    width: jni::sys::jint,
    height: jni::sys::jint,
    y: jni::objects::JByteBuffer,
    y_stride: jni::sys::jint,
    u: jni::objects::JByteBuffer,
    v: jni::objects::JByteBuffer,
    uv_stride: jni::sys::jint,
    uv_pixel_stride: jni::sys::jint,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let plane = |buffer: &jni::objects::JByteBuffer| -> Option<&[u8]> {
            let ptr = env.get_direct_buffer_address(buffer).ok()?;
            let len = env.get_direct_buffer_capacity(buffer).ok()?;
            // SAFETY: буфер живёт, пока Kotlin не закрыл кадр, а закрывает он
            // его после возврата из этой функции.
            (!ptr.is_null()).then(|| unsafe { std::slice::from_raw_parts(ptr, len) })
        };
        let (Some(y), Some(u), Some(v)) = (plane(&y), plane(&u), plane(&v)) else { return };
        let (w, h) = (width.max(0) as usize & !1, height.max(0) as usize & !1);
        let (y_stride, uv_stride, step) =
            (y_stride.max(0) as usize, uv_stride.max(0) as usize, uv_pixel_stride.max(1) as usize);
        // Проверяем, что плоскости не короче, чем обещают размеры.
        let (cw, ch) = (w / 2, h / 2);
        let y_ok = h == 0 || y.len() >= (h - 1) * y_stride + w;
        let uv_need = if ch == 0 { 0 } else { (ch - 1) * uv_stride + (cw.max(1) - 1) * step + 1 };
        if w < 2 || h < 2 || !y_ok || u.len() < uv_need || v.len() < uv_need {
            return;
        }
        let frame = vwf_client::camera::I420::from_planes(
            w as u32, h as u32, y, y_stride, u, v, uv_stride, step,
        );
        vwf_client::camera::android::push_frame(frame);
    }));
}

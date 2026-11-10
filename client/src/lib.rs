//! Клиентская часть voice_with_friends. Вынесена в библиотеку, чтобы потом
//! её же использовал графический клиент (Tauri) на Windows и Android.

#[cfg(target_os = "android")]
mod aaudio;
pub mod audio;
pub mod call;
pub mod camera;
pub mod cues;
pub mod discovery;
pub mod jitter;
pub mod known_servers;
pub mod mic_test;
pub mod net;
pub mod pipeline;
pub mod resample;
pub mod screen;

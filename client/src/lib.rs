//! Клиентская часть voice_with_friends. Вынесена в библиотеку, чтобы потом
//! её же использовал графический клиент (Tauri) на Windows и Android.

pub mod audio;
pub mod call;
pub mod discovery;
pub mod jitter;
pub mod known_servers;
pub mod net;
pub mod pipeline;
pub mod resample;

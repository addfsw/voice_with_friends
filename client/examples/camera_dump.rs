//! Записывает кадры проверочной камеры во всех слоях в файл — для проверки
//! интерфейса без звонка (плитки, развёрнутая камера):
//!
//! ```text
//! cargo run -p vwf-client --example camera_dump -- кадры.bin
//! ```
//!
//! Формат: подряд `[длина u32 LE][заголовок VideoHeader][H.264]`.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vwf_client::camera::CameraSharer;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "camera.bin".into());
    let packets: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let sink = packets.clone();
    let camera = CameraSharer::start_with(
        || vwf_client::camera::open("fake"),
        3,
        move |frame| {
            let mut packet = Vec::new();
            frame.header.encode(&mut packet);
            packet.extend_from_slice(&frame.data);
            sink.lock().unwrap().push(packet);
            true
        },
        |_| {},
        |e| eprintln!("камера остановилась: {e}"),
    )?;
    camera.set_viewers([1, 1, 1]);
    std::thread::sleep(Duration::from_secs(4));
    drop(camera);
    let packets = packets.lock().unwrap();
    let mut file = std::fs::File::create(&path)?;
    for packet in packets.iter() {
        file.write_all(&(packet.len() as u32).to_le_bytes())?;
        file.write_all(packet)?;
    }
    println!("{} кадров → {path}", packets.len());
    Ok(())
}

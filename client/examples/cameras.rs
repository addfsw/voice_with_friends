//! Список камер и проверка, что камера отдаёт кадры:
//!
//! ```text
//! cargo run -p vwf-client --example cameras            # список
//! cargo run -p vwf-client --example cameras -- <id>    # 3 секунды кадров с камеры
//! ```

use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let cameras = vwf_client::camera::cameras()?;
    if cameras.is_empty() {
        println!("камер нет (проверочная — VWF_FAKE_CAMERA=1)");
    }
    for camera in &cameras {
        println!("{}\n    id: {}", camera.name, camera.id);
    }
    let Some(id) = std::env::args().nth(1) else { return Ok(()) };
    let mut source = vwf_client::camera::open(&id)?;
    let start = Instant::now();
    let mut frames = 0;
    let mut size = (0, 0);
    while start.elapsed() < Duration::from_secs(3) {
        if let Some(frame) = source.frame(Instant::now() + Duration::from_millis(500))? {
            frames += 1;
            size = (frame.width, frame.height);
        }
    }
    println!("{frames} кадров за 3 с, {}×{}", size.0, size.1);
    Ok(())
}

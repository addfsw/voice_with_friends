use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use tokio::sync::mpsc as tokio_mpsc;
use vwf_client::pipeline::{Mixer, Transmitter, TxSettings, TxState};
use vwf_client::{audio, net};
use vwf_proto::{PeerId, ServerMsg};

/// Голосовой чат voice_with_friends (консольный клиент).
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Показать микрофоны и устройства вывода.
    Devices,
    /// Проверка звука без сети: микрофон → Opus → джиттер-буфер → динамики.
    /// Говорите в микрофон (лучше в наушниках) и слушайте себя.
    Loopback {
        #[command(flatten)]
        audio: AudioArgs,
        /// Имитация потери пакетов, %.
        #[arg(long, default_value_t = 0.0)]
        sim_loss: f32,
        /// Имитация неравномерной задержки сети: случайно от 0 до N мс.
        #[arg(long, default_value_t = 0)]
        sim_jitter_ms: u32,
    },
    /// Подключиться к серверу.
    Connect {
        /// Адрес сервера: IP или имя, порт можно не указывать (например 192.168.1.10).
        server: String,
        /// Ваше имя, которое увидят остальные.
        #[arg(long, short)]
        name: String,
        #[command(flatten)]
        audio: AudioArgs,
    },
}

#[derive(Args)]
struct AudioArgs {
    /// Микрофон (часть имени из `devices`). По умолчанию — системный.
    #[arg(long)]
    input: Option<String>,
    /// Устройство вывода (часть имени из `devices`). По умолчанию — системное.
    #[arg(long)]
    output: Option<String>,
    /// Битрейт голоса, кбит/с.
    #[arg(long, default_value_t = 32)]
    bitrate: i32,
    /// Порог активации голосом, dBFS: тише — не передаём.
    #[arg(long, default_value_t = -45.0, allow_negative_numbers = true)]
    vad_db: f32,
    /// Передавать всегда, без активации голосом.
    #[arg(long)]
    no_vad: bool,
    /// Глубина джиттер-буфера, мс. Больше — устойчивее к плохой сети, но больше задержка.
    #[arg(long, default_value_t = 60)]
    jitter_ms: u32,
}

impl AudioArgs {
    fn tx_settings(&self) -> TxSettings {
        TxSettings {
            bitrate: self.bitrate.clamp(6, 128) * 1000,
            vad_threshold_db: (!self.no_vad).then_some(self.vad_db),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Подробный журнал для диагностики: `set RUST_LOG=debug` перед запуском.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Devices => audio::list_devices(),
        Command::Loopback { audio, sim_loss, sim_jitter_ms } => {
            loopback(audio, sim_loss, sim_jitter_ms).await
        }
        Command::Connect { server, name, audio } => connect(&server, &name, audio).await,
    }
}

async fn loopback(args: AudioArgs, sim_loss: f32, sim_jitter_ms: u32) -> anyhow::Result<()> {
    let (capture, mic) = audio::start_capture(args.input.as_deref())?;
    let (playback, speakers) = audio::start_playback(args.output.as_deref())?;
    print_devices(&capture, &playback);

    let mixer = Arc::new(Mixer::new(args.jitter_ms));
    let _output = mixer.spawn_output(speakers, playback.config.sample_rate)?;
    let network = SimulatedNetwork::spawn(mixer.clone(), sim_loss, sim_jitter_ms);
    let tx = Transmitter::spawn(mic, args.tx_settings(), move |seq, opus| network.send(seq, opus))?;

    println!("Говорите — вы должны слышать себя с небольшой задержкой.");
    println!("Если голос не передаётся, подберите порог --vad-db по индикатору ниже.");
    println!("m + Enter — выключить/включить микрофон, Ctrl+C — выход.\n");

    let mut commands = spawn_stdin_reader();
    let mut meter = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = meter.tick() => print_meter(&tx.state),
            Some(cmd) = commands.recv() => handle_command(&cmd, &tx.state),
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    println!();
    Ok(())
}

async fn connect(server: &str, name: &str, args: AudioArgs) -> anyhow::Result<()> {
    println!("Подключаюсь к {server}...");
    let mut session = net::connect(server, name).await?;
    println!("Подключено.");

    let mut names: HashMap<PeerId, String> =
        session.peers.iter().map(|p| (p.id, p.name.clone())).collect();
    if names.is_empty() {
        println!("В комнате пока никого нет.");
    } else {
        let list: Vec<&str> = names.values().map(String::as_str).collect();
        println!("В комнате: {}", list.join(", "));
    }

    let (capture, mic) = audio::start_capture(args.input.as_deref())?;
    let (playback, speakers) = audio::start_playback(args.output.as_deref())?;
    print_devices(&capture, &playback);

    let mixer = Arc::new(Mixer::new(args.jitter_ms));
    let output = mixer.spawn_output(speakers, playback.config.sample_rate)?;

    let sender = session.voice_sender();
    let tx = Transmitter::spawn(mic, args.tx_settings(), move |seq, opus| sender.send(seq, opus))?;

    let receiver = session.voice_receiver();
    let rx_mixer = mixer.clone();
    tokio::spawn(async move {
        while let Some((from, seq, opus)) = receiver.recv().await {
            rx_mixer.push_packet(from, seq, &opus);
        }
    });

    println!("m + Enter — выключить/включить микрофон, Ctrl+C — выход.\n");
    let mut commands = spawn_stdin_reader();
    loop {
        tokio::select! {
            event = session.next_event() => match event {
                Ok(Some(ServerMsg::PeerJoined(peer))) => {
                    println!("→ {} подключился", peer.name);
                    names.insert(peer.id, peer.name);
                }
                Ok(Some(ServerMsg::PeerLeft { id })) => {
                    let name = names.remove(&id).unwrap_or_else(|| format!("#{id}"));
                    println!("← {name} отключился");
                    mixer.remove_speaker(id);
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    println!("Сервер закрыл соединение.");
                    break;
                }
                Err(e) => {
                    println!("Соединение потеряно: {e:#}");
                    break;
                }
            },
            Some(cmd) = commands.recv() => handle_command(&cmd, &tx.state),
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    drop(tx);
    drop(output);
    session.close().await;
    Ok(())
}

fn print_devices(capture: &audio::Capture, playback: &audio::Playback) {
    println!(
        "Микрофон: {} ({} Гц, каналов: {})",
        capture.device_name, capture.config.sample_rate, capture.config.channels
    );
    println!(
        "Вывод:    {} ({} Гц, каналов: {})",
        playback.device_name, playback.config.sample_rate, playback.config.channels
    );
}

fn print_meter(state: &TxState) {
    let db = state.level_db().max(-60.0);
    let filled = ((db + 60.0) / 2.0).round() as usize; // -60..0 dB → 0..30 делений
    let status = if state.muted.load(Ordering::Relaxed) {
        "[микрофон выключен]"
    } else if state.transmitting.load(Ordering::Relaxed) {
        "[ПЕРЕДАЧА]"
    } else {
        "[тишина]"
    };
    print!("\r{db:6.1} dBFS |{:<30}| {status:<20}", "#".repeat(filled));
    let _ = std::io::stdout().flush();
}

fn handle_command(cmd: &str, state: &TxState) {
    if cmd.trim().eq_ignore_ascii_case("m") {
        let muted = !state.muted.load(Ordering::Relaxed);
        state.muted.store(muted, Ordering::Relaxed);
        println!(
            "{}",
            if muted {
                "Микрофон выключен."
            } else {
                "Микрофон включён."
            }
        );
    }
}

/// Читает строки из консоли в отдельном потоке (чтение stdin блокирующее).
fn spawn_stdin_reader() -> tokio_mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio_mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// «Плохая сеть» для режима loopback: теряет и задерживает пакеты, чтобы было
/// слышно, как с этим справляются джиттер-буфер, FEC и PLC.
struct SimulatedNetwork {
    queue: mpsc::Sender<(Instant, u32, Vec<u8>)>,
    loss: f32,
    jitter_ms: u32,
    rng: std::cell::Cell<u64>,
}

impl SimulatedNetwork {
    fn spawn(mixer: Arc<Mixer>, loss_percent: f32, jitter_ms: u32) -> Self {
        let (queue, incoming) = mpsc::channel::<(Instant, u32, Vec<u8>)>();
        std::thread::spawn(move || {
            let mut in_flight = Vec::new();
            loop {
                match incoming.recv_timeout(Duration::from_millis(1)) {
                    Ok(packet) => in_flight.push(packet),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                let now = Instant::now();
                in_flight.retain(|(due, seq, opus)| {
                    if *due <= now {
                        mixer.push_packet(0, *seq, opus);
                        false
                    } else {
                        true
                    }
                });
            }
        });
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        Self { queue, loss: loss_percent / 100.0, jitter_ms, rng: std::cell::Cell::new(seed | 1) }
    }

    /// Псевдослучайное число от 0 до 1 (xorshift — для имитации хватает).
    fn random(&self) -> f32 {
        let mut x = self.rng.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng.set(x);
        (x >> 40) as f32 / (1u64 << 24) as f32
    }

    fn send(&self, seq: u32, opus: &[u8]) {
        if self.random() < self.loss {
            return;
        }
        let delay = Duration::from_millis((self.random() * self.jitter_ms as f32) as u64);
        let _ = self.queue.send((Instant::now() + delay, seq, opus.to_vec()));
    }
}

use std::collections::HashMap;
use std::io::Write;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use tokio::sync::mpsc as tokio_mpsc;
use vwf_client::audio::{self, GlitchCounter};
use vwf_client::call::{AudioEngine, AudioSettings, Call, CallEvent};
use vwf_client::jitter::Delay;
use vwf_client::known_servers::{self, KnownServers};
use vwf_client::net::{CertificateChanged, Trust};
use vwf_client::pipeline::{Mixer, TxMode, TxSettings, TxState};
use vwf_proto::PeerId;

/// Поигрулькины: голосовой чат (консольный клиент).
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
    /// Найти серверы в локальной сети.
    Discover,
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
        /// Секрет хоста, если сервер запущен с `--admin-token`.
        #[arg(long)]
        admin_token: Option<String>,
        /// Сертификат сервера изменился, и вы уверены, что это не подмена
        /// (хост подтвердил новый код): запомнить новый и подключиться.
        #[arg(long)]
        trust_new_certificate: bool,
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
    /// Отключить шумоподавление.
    #[arg(long)]
    no_denoise: bool,
    /// Постоянная задержка джиттер-буфера, мс. По умолчанию подстраивается под сеть
    /// сама: в локальной сети это ~10 мс, в плохом Wi-Fi больше.
    #[arg(long)]
    jitter_ms: Option<u32>,
    /// Сколько звука держать готовым для звуковой карты, мс. Если появляются
    /// сбои вывода (щелчки), увеличьте до 30–40.
    #[arg(long, default_value_t = 20)]
    playback_ms: u32,
}

impl AudioArgs {
    fn settings(&self) -> AudioSettings {
        AudioSettings {
            input: self.input.clone(),
            output: self.output.clone(),
            tx: TxSettings { bitrate: self.bitrate.clamp(6, 128) * 1000 },
            jitter: match self.jitter_ms {
                Some(ms) => Delay::Fixed(ms as f64),
                None => Delay::default(),
            },
            playback_ms: self.playback_ms,
        }
    }

    fn tx_state(&self) -> Arc<TxState> {
        let mode = if self.no_vad { TxMode::Always } else { TxMode::Voice };
        let state = TxState::new(mode, self.vad_db);
        state.set_noise_suppression(!self.no_denoise);
        Arc::new(state)
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
        Command::Discover => discover().await,
        Command::Loopback { audio, sim_loss, sim_jitter_ms } => {
            loopback(audio, sim_loss, sim_jitter_ms).await
        }
        Command::Connect { server, name, admin_token, trust_new_certificate, audio } => {
            connect(&server, &name, admin_token.as_deref(), trust_new_certificate, audio).await
        }
    }
}

async fn discover() -> anyhow::Result<()> {
    println!("Ищу серверы в локальной сети...");
    let servers = vwf_client::discovery::discover(Duration::from_secs(1)).await?;
    if servers.is_empty() {
        println!(
            "Ничего не найдено. Сервер запущен? Брандмауэр пропускает UDP-порт {}?",
            vwf_proto::DISCOVERY_PORT
        );
    }
    for server in servers {
        let note =
            if server.compatible { "" } else { "  (другая версия программы)" };
        println!("  {}  —  {}  —  участников: {}{note}", server.name, server.address, server.users);
    }
    Ok(())
}

async fn loopback(args: AudioArgs, sim_loss: f32, sim_jitter_ms: u32) -> anyhow::Result<()> {
    // Сеть создаём до звука, а микшер подключаем к ней сразу после запуска.
    let mixer_slot: Arc<std::sync::OnceLock<Arc<Mixer>>> = Arc::default();
    let network = SimulatedNetwork::spawn(mixer_slot.clone(), sim_loss, sim_jitter_ms);
    let audio = AudioEngine::start(&args.settings(), args.tx_state(), move |seq, opus| {
        network.send(seq, opus)
    })?;
    let _ = mixer_slot.set(audio.mixer.clone());
    print_devices(&audio);
    let names = HashMap::from([(LOOPBACK_ID, "вы".to_string())]);

    println!("Говорите — вы должны слышать себя с небольшой задержкой.");
    println!("Если голос не передаётся, подберите порог --vad-db по индикатору ниже.");
    println!("{COMMANDS_HELP}\n");

    let mut glitches = glitch_counters(&audio);
    let mut glitch_check = tokio::time::interval(GLITCH_REPORT_INTERVAL);
    let mut commands = spawn_stdin_reader();
    let mut meter = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = meter.tick() => print_meter(&audio.tx_state),
            _ = glitch_check.tick() => report_glitches(&mut glitches, true),
            Some(cmd) = commands.recv() => handle_command(&cmd, &audio, &names),
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    println!();
    Ok(())
}

async fn connect(
    server: &str,
    name: &str,
    admin_token: Option<&str>,
    trust_new_certificate: bool,
    args: AudioArgs,
) -> anyhow::Result<()> {
    println!("Подключаюсь к {server}...");
    let known_path = known_servers::default_path();
    let known = Mutex::new(KnownServers::load(&known_path));
    let (events_tx, mut events) = tokio_mpsc::unbounded_channel();
    let on_event = move |event| {
        let _ = events_tx.send(event);
    };
    let settings = args.settings();
    let tx_state = args.tx_state();
    // Новый сертификат принимаем не больше одного раза за запуск.
    let mut accept_new_certificate = trust_new_certificate;
    let call = loop {
        let trust = Trust::Known(&known);
        let result = Call::start(
            server,
            name,
            admin_token,
            trust,
            &settings,
            tx_state.clone(),
            on_event.clone(),
        )
        .await;
        match result {
            Ok(call) => break call,
            Err(e) => match e.downcast_ref::<CertificateChanged>() {
                Some(changed) if accept_new_certificate => {
                    accept_new_certificate = false;
                    println!("Запоминаю новый сертификат сервера {}.", changed.address);
                    known.lock().unwrap().remember(&changed.address, &changed.presented)?;
                }
                Some(_) => {
                    println!("ВНИМАНИЕ: {e}.");
                    println!(
                        "Если хост подтвердил новый код, подключитесь с --trust-new-certificate \
                         (запомненные сертификаты: {}).",
                        known_path.display()
                    );
                    return Ok(());
                }
                None => return Err(e),
            },
        }
    };
    println!("Подключено. Код сервера: {}", vwf_proto::short_fingerprint(&call.fingerprint));
    if call.me().admin {
        println!("Вы хост этой комнаты.");
    }
    print_devices(&call.audio);

    let mut names: HashMap<PeerId, String> =
        call.peers().into_iter().map(|p| (p.id, p.name)).collect();
    let mut server_muted = false;
    if names.is_empty() {
        println!("В комнате пока никого нет.");
    } else {
        let list: Vec<&str> = names.values().map(String::as_str).collect();
        println!("В комнате: {}", list.join(", "));
    }

    println!("{COMMANDS_HELP}\n");
    let mut glitches = glitch_counters(&call.audio);
    let mut glitch_check = tokio::time::interval(GLITCH_REPORT_INTERVAL);
    let mut commands = spawn_stdin_reader();
    loop {
        tokio::select! {
            _ = glitch_check.tick() => report_glitches(&mut glitches, false),
            Some(event) = events.recv() => match event {
                CallEvent::PeerJoined(peer) => {
                    println!("→ {} подключился", peer.name);
                    names.insert(peer.id, peer.name);
                }
                CallEvent::PeerLeft(id) => {
                    let name = names.remove(&id).unwrap_or_else(|| format!("#{id}"));
                    println!("← {name} отключился");
                }
                CallEvent::PeerUpdated(peer) if peer.id == call.my_id => {
                    if peer.server_muted != server_muted {
                        server_muted = peer.server_muted;
                        println!("{}", if server_muted {
                            "Хост заглушил вас: остальные вас не слышат."
                        } else {
                            "Хост вернул вам голос."
                        });
                    }
                }
                CallEvent::PeerUpdated(_) => {}
                CallEvent::Ended(reason) => {
                    println!("Звонок завершён: {reason}.");
                    break;
                }
            },
            Some(cmd) = commands.recv() => {
                handle_command(&cmd, &call.audio, &names);
                call.sync_status();
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    call.hang_up().await;
    Ok(())
}

/// Как часто сообщать о сбоях звука (чтобы не засорять консоль).
const GLITCH_REPORT_INTERVAL: Duration = Duration::from_secs(5);

fn glitch_counters(audio: &AudioEngine) -> [GlitchCounter; 2] {
    [
        GlitchCounter::new("микрофон", audio.capture.glitches.clone()),
        GlitchCounter::new("вывод", audio.playback.glitches.clone()),
    ]
}

fn report_glitches(counters: &mut [GlitchCounter], after_meter: bool) {
    for counter in counters {
        if let Some(report) = counter.take_report() {
            // Индикатор громкости рисуется без перевода строки — не затираем его.
            if after_meter {
                println!();
            }
            println!("{report}");
        }
    }
}

fn print_devices(audio: &AudioEngine) {
    let (capture, playback) = (&audio.capture, &audio.playback);
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
    let status = if state.muted() {
        "[микрофон выключен]"
    } else if state.transmitting() {
        "[ПЕРЕДАЧА]"
    } else {
        "[тишина]"
    };
    print!("\r{db:6.1} dBFS |{:<30}| {status:<20}", "#".repeat(filled));
    let _ = std::io::stdout().flush();
}

const COMMANDS_HELP: &str = "Команды (+ Enter): m — выключить/включить микрофон, \
                             s — задержка джиттер-буфера. Ctrl+C — выход.";

fn handle_command(cmd: &str, audio: &AudioEngine, names: &HashMap<PeerId, String>) {
    match cmd.trim().to_lowercase().as_str() {
        "m" => {
            let state = &audio.tx_state;
            let muted = !state.muted();
            state.set_muted(muted);
            println!(
                "{}",
                if muted {
                    "Микрофон выключен."
                } else {
                    "Микрофон включён."
                }
            );
        }
        "s" => {
            let delays = audio.mixer.delays_ms();
            if delays.is_empty() {
                println!("Пока никого не слышно.");
            }
            for (id, delay) in delays {
                let name = names.get(&id).cloned().unwrap_or_else(|| format!("#{id}"));
                println!("{name}: задержка джиттер-буфера {delay:.0} мс");
            }
        }
        _ => {}
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

/// Под каким id в режиме loopback микшер слышит собственный голос.
const LOOPBACK_ID: PeerId = 0;

/// «Плохая сеть» для режима loopback: теряет и задерживает пакеты, чтобы было
/// слышно, как с этим справляются джиттер-буфер, FEC и PLC.
struct SimulatedNetwork {
    queue: mpsc::Sender<(Instant, u32, Vec<u8>)>,
    loss: f32,
    jitter_ms: u32,
    rng: std::cell::Cell<u64>,
}

impl SimulatedNetwork {
    fn spawn(
        mixer: Arc<std::sync::OnceLock<Arc<Mixer>>>,
        loss_percent: f32,
        jitter_ms: u32,
    ) -> Self {
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
                        if let Some(mixer) = mixer.get() {
                            mixer.push_packet(LOOPBACK_ID, *seq, opus);
                        }
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

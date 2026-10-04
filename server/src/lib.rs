//! Сервер: принимает QUIC-подключения, держит одну общую комнату и пересылает
//! голосовые пакеты каждого участника всем остальным.
//!
//! Сервер звук не декодирует и не микширует — он просто «почтальон» (так устроены
//! и Mumble, и SFU в WebRTC). Поэтому нагрузка на процессор почти нулевая,
//! а качество звука определяется только клиентами.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use bytes::Bytes;
use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::sync::mpsc;
use tracing::{info, warn};
use vwf_proto::{
    Announcement, ChatMessage, ClientMsg, PROTOCOL_VERSION, PeerId, PeerInfo, ServerMsg,
    VideoHeader,
};

const MAX_NAME_CHARS: usize = 32;

pub struct Config {
    pub bind: SocketAddr,
    /// Возвращать отправителю его же голос. Удобно, чтобы проверить всю цепочку
    /// через сеть в одиночку.
    pub echo: bool,
    /// Где хранить сертификат и ключ. `None` — сгенерировать временные (для тестов).
    pub data_dir: Option<PathBuf>,
    /// Название сервера, которое видят клиенты при поиске в локальной сети.
    pub name: String,
    /// Где отвечать на поиск серверов (обычно `0.0.0.0:47778`); `None` — не отвечать.
    pub discovery: Option<SocketAddr>,
    /// Секрет, предъявив который при подключении, участник получает права хоста
    /// (заглушать и выгонять других). `None` — хоста нет.
    pub admin_token: Option<String>,
}

pub struct Server {
    endpoint: quinn::Endpoint,
    fingerprint: String,
    /// Режим эха можно переключать и на работающем сервере (см. `echo_switch`).
    echo: Arc<AtomicBool>,
    /// «Никого больше не пускать» (см. `lock_switch`).
    locked: Arc<AtomicBool>,
    /// Отвечать ли на поиск в локальной сети (см. `discovery_switch`).
    discoverable: Arc<AtomicBool>,
    name: String,
    discovery: Option<std::net::UdpSocket>,
    admin_token: Option<String>,
}

impl Server {
    pub fn bind(config: Config) -> anyhow::Result<Self> {
        Self::bind_with_socket(config, None)
    }

    /// То же, но поверх готового сокета (`config.bind` тогда не используется).
    /// Так сервер работает на сокете с пробивкой NAT (см. `vwf-nat`).
    pub fn bind_with_socket(
        config: Config,
        socket: Option<Arc<dyn quinn::AsyncUdpSocket>>,
    ) -> anyhow::Result<Self> {
        let (cert, key) = match &config.data_dir {
            Some(dir) => load_or_create_cert(dir)?,
            None => generate_cert()?,
        };
        let fingerprint = vwf_proto::cert_fingerprint(&cert);

        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
        tls.alpn_protocols = vec![vwf_proto::ALPN.to_vec()];

        let mut server_config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
        server_config.transport_config(Arc::new(transport_config()));

        let endpoint = match socket {
            Some(socket) => quinn::Endpoint::new_with_abstract_socket(
                quinn::EndpointConfig::default(),
                Some(server_config),
                socket,
                Arc::new(quinn::TokioRuntime),
            )?,
            None => quinn::Endpoint::server(server_config, config.bind)
                .with_context(|| format!("не удалось открыть UDP-порт {}", config.bind))?,
        };

        // Без поиска сервер прекрасно работает, поэтому ошибку здесь не считаем
        // фатальной: например, на одном компьютере запущены два сервера.
        let discovery = config.discovery.and_then(|addr| {
            match std::net::UdpSocket::bind(addr).and_then(|s| s.set_nonblocking(true).map(|_| s)) {
                Ok(socket) => Some(socket),
                Err(e) => {
                    warn!("поиск в локальной сети отключён: не удалось открыть UDP {addr}: {e}");
                    None
                }
            }
        });

        Ok(Self {
            endpoint,
            fingerprint,
            echo: Arc::new(AtomicBool::new(config.echo)),
            locked: Arc::new(AtomicBool::new(false)),
            discoverable: Arc::new(AtomicBool::new(true)),
            name: config.name,
            discovery,
            admin_token: config.admin_token.filter(|t| !t.is_empty()),
        })
    }

    /// Адрес, на котором сервер отвечает на поиск (если отвечает).
    pub fn discovery_addr(&self) -> Option<SocketAddr> {
        self.discovery.as_ref().and_then(|s| s.local_addr().ok())
    }

    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    /// SHA-256 отпечаток сертификата. Клиенты запоминают его при первом
    /// подключении и так замечают подмену сервера.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Принимает подключения, пока endpoint не закроют
    /// (`server.endpoint().close(...)` — так сервер и останавливают).
    pub async fn run(self) {
        let room = Arc::new(Mutex::new(Room::new()));

        let mut discovery_task = None;
        if let Some(socket) = self.discovery {
            match tokio::net::UdpSocket::from_std(socket) {
                Ok(socket) => {
                    let port = self.endpoint.local_addr().map(|a| a.port()).unwrap_or_default();
                    let announcement = Announcement {
                        instance: random_u64(),
                        name: self.name.clone(),
                        port,
                        protocol: PROTOCOL_VERSION,
                        users: 0,
                    };
                    discovery_task = Some(tokio::spawn(answer_discovery(
                        socket,
                        room.clone(),
                        announcement,
                        self.discoverable.clone(),
                    )));
                }
                Err(e) => warn!("поиск в локальной сети отключён: {e}"),
            }
        }

        let settings = Arc::new(RoomSettings {
            echo: self.echo,
            locked: self.locked,
            admin_token: self.admin_token,
        });
        while let Some(incoming) = self.endpoint.accept().await {
            let room = room.clone();
            let settings = settings.clone();
            tokio::spawn(async move {
                let remote = incoming.remote_address();
                if let Err(e) = handle_connection(incoming, room, settings).await {
                    warn!("{remote}: {e:#}");
                }
            });
        }
        // Сервер остановили: перестаём отвечать на поиск и дожидаемся, пока
        // задача завершится и закроет сокет, — тогда порт сразу свободен.
        if let Some(task) = discovery_task {
            task.abort();
            let _ = task.await;
        }
    }

    /// Переключатель режима эха; действует и после запуска `run`.
    pub fn echo_switch(&self) -> Arc<AtomicBool> {
        self.echo.clone()
    }

    /// Переключатель «никого больше не пускать»: пока включён, сервер
    /// отказывает новым участникам. Хост и те, кто только что был в комнате
    /// (переподключаются после обрыва связи), проходят.
    pub fn lock_switch(&self) -> Arc<AtomicBool> {
        self.locked.clone()
    }

    /// Переключатель видимости в локальной сети: выключен — сервер молчит
    /// на поиск, и в списке «В локальной сети» его нет.
    pub fn discovery_switch(&self) -> Arc<AtomicBool> {
        self.discoverable.clone()
    }

    pub fn endpoint(&self) -> &quinn::Endpoint {
        &self.endpoint
    }
}

/// Отвечает на поисковые запросы клиентов. Ответ уходит лично спросившему.
async fn answer_discovery(
    socket: tokio::net::UdpSocket,
    room: Arc<Mutex<Room>>,
    mut announcement: Announcement,
    discoverable: Arc<AtomicBool>,
) {
    let mut buf = [0u8; 64];
    loop {
        let (len, from) = match socket.recv_from(&mut buf).await {
            Ok(received) => received,
            // На Windows recv_from на UDP-сокете может вернуть ошибку, если
            // какой-то из прошлых ответов не дошёл (ICMP «порт недоступен»).
            // Сокет при этом исправен — просто продолжаем.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };
        if &buf[..len] != vwf_proto::DISCOVERY_PROBE || !discoverable.load(Ordering::Relaxed) {
            continue;
        }
        announcement.users = room.lock().unwrap().peers.len() as u32;
        let _ = socket.send_to(&vwf_proto::encode_announcement(&announcement), from).await;
    }
}

/// Случайное число без отдельной библиотеки: хэш от времени и номера процесса.
fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    hasher.write_u32(std::process::id());
    hasher.finish()
}

/// Название компьютера — название сервера по умолчанию.
pub fn default_server_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Поигрулькины".to_string())
}

/// Общие настройки QUIC: регулярные keep-alive, чтобы NAT-роутеры не забывали
/// про соединение, пока все молчат, и быстрое обнаружение обрыва.
fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(5)));
    t.max_idle_timeout(Some(Duration::from_secs(20).try_into().expect("валидный таймаут")));
    t
}

struct Room {
    next_id: PeerId,
    peers: HashMap<PeerId, Peer>,
    /// Кто и когда недавно ушёл (по имени) — для закрытой комнаты.
    recently_left: HashMap<String, Instant>,
    /// Последние сообщения чата (старые — первыми).
    chat: VecDeque<ChatMessage>,
    next_message_id: u64,
}

struct RoomSettings {
    echo: Arc<AtomicBool>,
    locked: Arc<AtomicBool>,
    admin_token: Option<String>,
}

/// Кто ушёл из комнаты не дольше этого назад, может вернуться и в закрытую:
/// так переподключение после обрыва связи не упирается в замок.
const REJOIN_WINDOW: Duration = Duration::from_secs(180);

struct Peer {
    info: PeerInfo,
    conn: quinn::Connection,
    /// Очередь управляющих сообщений этому участнику (пишет отдельная задача).
    control: mpsc::UnboundedSender<ServerMsg>,
    /// Когда участник отправлял последние сообщения в чат — против флуда.
    recent_chat: VecDeque<Instant>,
    /// Чьи экраны он смотрит.
    watching: HashSet<PeerId>,
    /// Чьи кадры ему сейчас нельзя слать до опорного: он пропустил кадр,
    /// и без опорного картинка у него развалится.
    waiting_keyframe: HashSet<PeerId>,
    /// Очередь кадров демонстрации ему (пишет отдельная задача).
    video: mpsc::Sender<Bytes>,
    /// Когда у него последний раз просили опорный кадр (если он показывает экран).
    keyframe_asked: Option<Instant>,
    /// Его выгнал хост: в закрытую комнату он не вернётся.
    kicked: bool,
}

/// Сколько кадров демонстрации может ждать отправки одному зрителю. Больше —
/// значит, его канал не успевает, и лишние кадры пропускаются до опорного.
const VIDEO_QUEUE: usize = 6;

/// Не чаще этого просить у показывающего опорный кадр (он тяжёлый).
const KEYFRAME_INTERVAL: Duration = Duration::from_millis(500);

/// Не больше стольких сообщений в чат...
const CHAT_BURST: usize = 10;
/// ...за это время от одного участника. Лишние сервер молча отбрасывает.
const CHAT_WINDOW: Duration = Duration::from_secs(10);

impl Room {
    fn new() -> Self {
        Self {
            next_id: 0,
            peers: HashMap::new(),
            recently_left: HashMap::new(),
            chat: VecDeque::new(),
            // Номера сообщений начинаются со случайного числа: если сервер
            // перезапустят, новые сообщения не совпадут по номеру со старыми,
            // которые остались у клиентов на экране.
            next_message_id: random_u64() >> 16,
        }
    }

    fn send_to(&self, id: PeerId, msg: ServerMsg) {
        if let Some(peer) = self.peers.get(&id) {
            let _ = peer.control.send(msg);
        }
    }

    /// Просит показывающего `sharer` прислать опорный кадр (не слишком часто).
    fn ask_keyframe(&mut self, sharer: PeerId) {
        let Some(peer) = self.peers.get_mut(&sharer) else { return };
        if peer.keyframe_asked.is_some_and(|t| t.elapsed() < KEYFRAME_INTERVAL) {
            return;
        }
        peer.keyframe_asked = Some(Instant::now());
        let _ = peer.control.send(ServerMsg::KeyframeNeeded);
    }

    /// Сообщает показывающему, сколько у него зрителей.
    fn send_viewer_count(&self, sharer: PeerId) {
        let count = self.peers.values().filter(|p| p.watching.contains(&sharer)).count();
        self.send_to(sharer, ServerMsg::Viewers { count: count as u32 });
    }

    /// `viewer` начинает или перестаёт смотреть экран `sharer`.
    fn watch(&mut self, viewer: PeerId, sharer: PeerId, watch: bool) {
        let sharing = self.peers.get(&sharer).is_some_and(|p| p.info.sharing);
        let Some(peer) = self.peers.get_mut(&viewer) else { return };
        let changed = if watch && sharing {
            // Начинать показ можно только с опорного кадра.
            peer.waiting_keyframe.insert(sharer);
            peer.watching.insert(sharer)
        } else {
            peer.waiting_keyframe.remove(&sharer);
            peer.watching.remove(&sharer)
        };
        if changed {
            if watch {
                self.ask_keyframe(sharer);
            }
            self.send_viewer_count(sharer);
        }
    }

    /// Участник перестал показывать экран (или ушёл): зрители больше не смотрят.
    fn stop_sharing(&mut self, sharer: PeerId) {
        for peer in self.peers.values_mut() {
            peer.watching.remove(&sharer);
            peer.waiting_keyframe.remove(&sharer);
        }
    }

    /// Рассылает кадр демонстрации от `from` его зрителям. `packet` — уже
    /// готовый поток для зрителя: `[from][заголовок][H.264]`.
    fn forward_video(&mut self, from: PeerId, keyframe: bool, packet: Bytes) {
        if !self.peers.get(&from).is_some_and(|p| p.info.sharing) {
            return;
        }
        let mut lagging = false;
        for peer in self.peers.values_mut() {
            if !peer.watching.contains(&from) {
                continue;
            }
            if peer.waiting_keyframe.contains(&from) && !keyframe {
                continue;
            }
            match peer.video.try_send(packet.clone()) {
                Ok(()) => {
                    peer.waiting_keyframe.remove(&from);
                }
                // Зритель не успевает: пропускаем кадры до следующего опорного.
                Err(_) => {
                    peer.waiting_keyframe.insert(from);
                    lagging = true;
                }
            }
        }
        if lagging {
            self.ask_keyframe(from);
        }
    }

    /// Можно ли `name` войти в закрытую комнату: он только что в ней был
    /// или всё ещё числится в ней (связь оборвалась, а сервер этого пока не
    /// заметил, — клиент переподключается).
    fn may_rejoin(&mut self, name: &str) -> bool {
        self.recently_left.retain(|_, left| left.elapsed() < REJOIN_WINDOW);
        self.recently_left.contains_key(name)
            || self.peers.values().any(|p| !p.kicked && p.info.name == name)
    }

    /// Принимает сообщение в чат от участника `from` и рассылает всем.
    fn chat(&mut self, from: PeerId, text: &str) {
        let Some(peer) = self.peers.get_mut(&from) else { return };
        let text = sanitize_chat(text);
        if text.is_empty() {
            return;
        }
        let now = Instant::now();
        while peer.recent_chat.front().is_some_and(|t| now - *t > CHAT_WINDOW) {
            peer.recent_chat.pop_front();
        }
        if peer.recent_chat.len() >= CHAT_BURST {
            warn!("id {from}: слишком много сообщений в чат, пропускаю");
            return;
        }
        peer.recent_chat.push_back(now);

        let message = ChatMessage {
            id: self.next_message_id,
            from,
            name: peer.info.name.clone(),
            text,
            time_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
        };
        self.next_message_id += 1;
        if self.chat.len() == vwf_proto::CHAT_HISTORY {
            self.chat.pop_front();
        }
        self.chat.push_back(message.clone());
        let msg = ServerMsg::Chat(message);
        for peer in self.peers.values() {
            let _ = peer.control.send(msg.clone());
        }
    }

    fn broadcast(&self, except: PeerId, msg: &ServerMsg) {
        for (id, peer) in &self.peers {
            if *id != except {
                let _ = peer.control.send(msg.clone());
            }
        }
    }

    /// Меняет статус участника и сообщает об этом всем, включая его самого.
    fn update(&mut self, id: PeerId, change: impl FnOnce(&mut PeerInfo)) {
        let Some(peer) = self.peers.get_mut(&id) else { return };
        let before = peer.info.clone();
        change(&mut peer.info);
        if peer.info != before {
            let msg = ServerMsg::PeerUpdated(peer.info.clone());
            for peer in self.peers.values() {
                let _ = peer.control.send(msg.clone());
            }
        }
    }

    /// Выполняет управляющую команду участника `from`.
    fn handle_command(&mut self, from: PeerId, msg: ClientMsg) {
        let from_admin = self.peers.get(&from).is_some_and(|p| p.info.admin);
        match msg {
            ClientMsg::Hello { .. } => {}
            ClientMsg::SetStatus { muted, deafened } => self.update(from, |p| {
                p.muted = muted;
                p.deafened = deafened;
            }),
            ClientMsg::SetServerMute { id, muted } if from_admin => {
                self.update(id, |p| p.server_muted = muted);
            }
            ClientMsg::Chat { text } => self.chat(from, &text),
            ClientMsg::SetSharing { sharing } => {
                if !sharing {
                    self.stop_sharing(from);
                }
                self.update(from, |p| p.sharing = sharing);
                if sharing {
                    self.send_viewer_count(from);
                }
            }
            ClientMsg::Watch { id, watch } => self.watch(from, id, watch),
            ClientMsg::RequestKeyframe { id } => {
                if self.peers.get(&from).is_some_and(|p| p.watching.contains(&id)) {
                    self.ask_keyframe(id);
                }
            }
            ClientMsg::Kick { id } if from_admin && id != from => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.kicked = true;
                    info!("хост выгнал {} (id {id})", peer.info.name);
                    // Остальное (удаление из комнаты, PeerLeft) сделает
                    // обработчик соединения этого участника, когда оно закроется.
                    peer.conn.close(1u32.into(), vwf_proto::KICKED_REASON.as_bytes());
                }
            }
            other => warn!("id {from}: нет прав на {other:?}"),
        }
    }
}

async fn handle_connection(
    incoming: quinn::Incoming,
    room: Arc<Mutex<Room>>,
    settings: Arc<RoomSettings>,
) -> anyhow::Result<()> {
    let conn = incoming.await?;
    let remote = conn.remote_address();

    // Клиент открывает управляющий поток и первым делом шлёт Hello.
    let (mut send, mut recv, hello) = tokio::time::timeout(Duration::from_secs(10), async {
        let (send, mut recv) = conn.accept_bi().await?;
        let hello = vwf_proto::read_msg::<_, ClientMsg>(&mut recv).await?;
        anyhow::Ok((send, recv, hello))
    })
    .await
    .context("клиент не представился вовремя")??;
    let Some(ClientMsg::Hello { protocol, name, admin_token }) = hello else {
        anyhow::bail!("клиент закрыл поток, не представившись");
    };

    if protocol != PROTOCOL_VERSION {
        let reason = format!(
            "версия протокола клиента {protocol}, а сервера {PROTOCOL_VERSION}: обновите программу"
        );
        vwf_proto::write_msg(&mut send, &ServerMsg::Rejected { reason: reason.clone() }).await?;
        let _ = send.finish();
        // Даём сообщению дойти, прежде чем рвать соединение.
        let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
        anyhow::bail!(reason);
    }
    let name = sanitize_name(&name);
    let admin = settings.admin_token.is_some() && admin_token == settings.admin_token;
    if !admin && settings.locked.load(Ordering::Relaxed) && !room.lock().unwrap().may_rejoin(&name)
    {
        let reason = vwf_proto::LOCKED_REASON.to_string();
        vwf_proto::write_msg(&mut send, &ServerMsg::Rejected { reason: reason.clone() }).await?;
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(2), conn.closed()).await;
        anyhow::bail!("{name}: {reason}");
    }

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let (video_tx, mut video_rx) = mpsc::channel::<Bytes>(VIDEO_QUEUE);
    let id = {
        let mut room = room.lock().unwrap();
        let id = room.next_id;
        room.next_id += 1;
        let info = PeerInfo { admin, ..PeerInfo::new(id, name.clone()) };
        let mut peers: Vec<PeerInfo> = room.peers.values().map(|p| p.info.clone()).collect();
        peers.sort_by_key(|p| p.id);
        // Welcome кладём в очередь до того, как участник попадёт в комнату, —
        // так он гарантированно получит его раньше любых PeerJoined/PeerLeft.
        let chat = room.chat.iter().cloned().collect();
        let _ = control_tx.send(ServerMsg::Welcome { you: info.clone(), peers, chat });
        room.broadcast(id, &ServerMsg::PeerJoined(info.clone()));
        room.peers.insert(
            id,
            Peer {
                info,
                conn: conn.clone(),
                control: control_tx,
                recent_chat: VecDeque::new(),
                watching: HashSet::new(),
                waiting_keyframe: HashSet::new(),
                video: video_tx,
                keyframe_asked: None,
                kicked: false,
            },
        );
        id
    };
    let role = if admin { ", хост" } else { "" };
    info!("+ {name} (id {id}, {remote}{role})");

    // Писатель управляющих сообщений.
    tokio::spawn(async move {
        while let Some(msg) = control_rx.recv().await {
            if vwf_proto::write_msg(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });

    // Кадры демонстрации этому участнику: каждый — отдельным потоком, по порядку.
    let video_conn = conn.clone();
    tokio::spawn(async move {
        while let Some(packet) = video_rx.recv().await {
            let Ok(mut stream) = video_conn.open_uni().await else { break };
            if stream.write_all(&packet).await.is_err() {
                break;
            }
            let _ = stream.finish();
        }
    });

    // Кадры демонстрации от этого участника. Читаем потоки по одному, чтобы
    // кадры уходили зрителям в том же порядке, в каком были сняты.
    let video_conn = conn.clone();
    let video_room = room.clone();
    tokio::spawn(async move {
        while let Ok(mut stream) = video_conn.accept_uni().await {
            let limit = vwf_proto::MAX_VIDEO_FRAME + VideoHeader::LEN;
            let Ok(data) = stream.read_to_end(limit).await else { continue };
            let Some((header, _)) = VideoHeader::decode(&data) else { continue };
            let mut packet = Vec::with_capacity(4 + data.len());
            packet.extend_from_slice(&id.to_le_bytes());
            packet.extend_from_slice(&data);
            video_room.lock().unwrap().forward_video(id, header.keyframe, Bytes::from(packet));
        }
    });

    // Читатель управляющих сообщений. Если клиент закрыл поток — значит, уходит.
    let reader_conn = conn.clone();
    let reader_room = room.clone();
    tokio::spawn(async move {
        while let Ok(Some(msg)) = vwf_proto::read_msg::<_, ClientMsg>(&mut recv).await {
            reader_room.lock().unwrap().handle_command(id, msg);
        }
        reader_conn.close(0u32.into(), b"bye");
    });

    // Основной цикл: голосовые датаграммы.
    let reason = loop {
        let data = match conn.read_datagram().await {
            Ok(data) => data,
            Err(e) => break e,
        };
        if vwf_proto::decode_client_voice(&data).is_none() {
            continue;
        }
        let packet = Bytes::from(vwf_proto::encode_server_voice(id, &data));
        let targets: Vec<quinn::Connection> = {
            let room = room.lock().unwrap();
            // Заглушённого хостом не пересылаем никому.
            if room.peers.get(&id).is_none_or(|p| p.info.server_muted) {
                continue;
            }
            let echo = settings.echo.load(Ordering::Relaxed);
            room.peers
                .iter()
                .filter(|(pid, _)| echo || **pid != id)
                .map(|(_, p)| p.conn.clone())
                .collect()
        };
        for target in targets {
            // Ошибку игнорируем: если у получателя проблемы, это его соединение,
            // и оно само закроется по таймауту.
            let _ = target.send_datagram(packet.clone());
        }
    };

    {
        let mut room = room.lock().unwrap();
        if let Some(peer) = room.peers.remove(&id) {
            if !peer.kicked {
                room.recently_left.insert(peer.info.name.clone(), Instant::now());
            }
            // Ушедший больше никого не смотрит, а его зрители — его.
            for sharer in peer.watching {
                room.send_viewer_count(sharer);
            }
            room.stop_sharing(id);
        }
        room.broadcast(id, &ServerMsg::PeerLeft { id });
    }
    info!("- {name} (id {id}): {reason}");
    Ok(())
}

/// Убирает управляющие символы (кроме переноса строки и табуляции), пустые
/// строки по краям и обрезает слишком длинное сообщение.
fn sanitize_chat(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(vwf_proto::MAX_CHAT_CHARS)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn sanitize_name(name: &str) -> String {
    let name: String =
        name.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
    if name.is_empty() { "Аноним".to_string() } else { name }
}

type CertAndKey = (CertificateDer<'static>, PrivateKeyDer<'static>);

fn generate_cert() -> anyhow::Result<CertAndKey> {
    let certified = rcgen::generate_simple_self_signed(vec!["vwf-server".to_string()])?;
    let cert = certified.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()).into();
    Ok((cert, key))
}

/// Самоподписанный сертификат создаётся один раз и сохраняется, чтобы отпечаток
/// сервера не менялся между перезапусками.
fn load_or_create_cert(dir: &Path) -> anyhow::Result<CertAndKey> {
    let cert_path = dir.join("server_cert.der");
    let key_path = dir.join("server_key.der");
    if cert_path.exists() && key_path.exists() {
        let cert = CertificateDer::from(std::fs::read(&cert_path)?);
        let key = PrivatePkcs8KeyDer::from(std::fs::read(&key_path)?).into();
        return Ok((cert, key));
    }
    let certified = rcgen::generate_simple_self_signed(vec!["vwf-server".to_string()])?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&cert_path, certified.cert.der())?;
    std::fs::write(&key_path, certified.signing_key.serialize_der())?;
    info!("создан новый сертификат сервера: {}", cert_path.display());
    load_or_create_cert(dir)
}

/// Адреса этого компьютера, по которым к серверу могут подключиться друзья
/// (локальная сеть, Tailscale/ZeroTier и т.п.).
pub fn reachable_addresses(port: u16) -> Vec<(String, SocketAddr)> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else { return Vec::new() };
    ifaces
        .into_iter()
        .filter(|i| i.ip().is_ipv4() && !i.is_loopback() && !i.is_link_local())
        .map(|i| (i.name.clone(), SocketAddr::new(i.ip(), port)))
        .collect()
}

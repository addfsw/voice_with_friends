//! Сервер: принимает QUIC-подключения, держит одну общую комнату и пересылает
//! голосовые пакеты каждого участника всем остальным.
//!
//! Сервер звук не декодирует и не микширует — он просто «почтальон» (так устроены
//! и Mumble, и SFU в WebRTC). Поэтому нагрузка на процессор почти нулевая,
//! а качество звука определяется только клиентами.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tracing::{info, warn};
use vwf_proto::{Announcement, ClientMsg, PROTOCOL_VERSION, PeerId, PeerInfo, ServerMsg};

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
}

pub struct Server {
    endpoint: quinn::Endpoint,
    fingerprint: String,
    echo: bool,
    name: String,
    discovery: Option<std::net::UdpSocket>,
}

impl Server {
    pub fn bind(config: Config) -> anyhow::Result<Self> {
        let (cert, key) = match &config.data_dir {
            Some(dir) => load_or_create_cert(dir)?,
            None => generate_cert()?,
        };
        let fingerprint = fingerprint(&cert);

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

        let endpoint = quinn::Endpoint::server(server_config, config.bind)
            .with_context(|| format!("не удалось открыть UDP-порт {}", config.bind))?;

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

        Ok(Self { endpoint, fingerprint, echo: config.echo, name: config.name, discovery })
    }

    /// Адрес, на котором сервер отвечает на поиск (если отвечает).
    pub fn discovery_addr(&self) -> Option<SocketAddr> {
        self.discovery.as_ref().and_then(|s| s.local_addr().ok())
    }

    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    /// SHA-256 отпечаток сертификата. Позже клиенты смогут его запоминать
    /// и замечать подмену сервера.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Принимает подключения, пока endpoint не закроют
    /// (`server.endpoint().close(...)` — так сервер и останавливают).
    pub async fn run(self) {
        let room = Arc::new(Mutex::new(Room::default()));

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
                    discovery_task =
                        Some(tokio::spawn(answer_discovery(socket, room.clone(), announcement)));
                }
                Err(e) => warn!("поиск в локальной сети отключён: {e}"),
            }
        }

        while let Some(incoming) = self.endpoint.accept().await {
            let room = room.clone();
            let echo = self.echo;
            tokio::spawn(async move {
                let remote = incoming.remote_address();
                if let Err(e) = handle_connection(incoming, room, echo).await {
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

    pub fn endpoint(&self) -> &quinn::Endpoint {
        &self.endpoint
    }
}

/// Отвечает на поисковые запросы клиентов. Ответ уходит лично спросившему.
async fn answer_discovery(
    socket: tokio::net::UdpSocket,
    room: Arc<Mutex<Room>>,
    mut announcement: Announcement,
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
        if &buf[..len] != vwf_proto::DISCOVERY_PROBE {
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
        .unwrap_or_else(|| "vwf-server".to_string())
}

/// Общие настройки QUIC: регулярные keep-alive, чтобы NAT-роутеры не забывали
/// про соединение, пока все молчат, и быстрое обнаружение обрыва.
fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(5)));
    t.max_idle_timeout(Some(Duration::from_secs(20).try_into().expect("валидный таймаут")));
    t
}

#[derive(Default)]
struct Room {
    next_id: PeerId,
    peers: HashMap<PeerId, Peer>,
}

struct Peer {
    name: String,
    conn: quinn::Connection,
    /// Очередь управляющих сообщений этому участнику (пишет отдельная задача).
    control: mpsc::UnboundedSender<ServerMsg>,
}

impl Room {
    fn broadcast(&self, except: PeerId, msg: &ServerMsg) {
        for (id, peer) in &self.peers {
            if *id != except {
                let _ = peer.control.send(msg.clone());
            }
        }
    }
}

async fn handle_connection(
    incoming: quinn::Incoming,
    room: Arc<Mutex<Room>>,
    echo: bool,
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
    let Some(ClientMsg::Hello { protocol, name }) = hello else {
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let id = {
        let mut room = room.lock().unwrap();
        let id = room.next_id;
        room.next_id += 1;
        let peers =
            room.peers.iter().map(|(id, p)| PeerInfo { id: *id, name: p.name.clone() }).collect();
        // Welcome кладём в очередь до того, как участник попадёт в комнату, —
        // так он гарантированно получит его раньше любых PeerJoined/PeerLeft.
        let _ = control_tx.send(ServerMsg::Welcome { your_id: id, peers });
        room.broadcast(id, &ServerMsg::PeerJoined(PeerInfo { id, name: name.clone() }));
        room.peers.insert(id, Peer { name: name.clone(), conn: conn.clone(), control: control_tx });
        id
    };
    info!("+ {name} (id {id}, {remote})");

    // Писатель управляющих сообщений.
    tokio::spawn(async move {
        while let Some(msg) = control_rx.recv().await {
            if vwf_proto::write_msg(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });

    // Читатель управляющих сообщений: пока клиенту нечего сказать после Hello,
    // но если он закрыл поток — значит, уходит.
    let reader_conn = conn.clone();
    tokio::spawn(async move {
        // Повторный Hello и прочее пока игнорируем.
        while let Ok(Some(_)) = vwf_proto::read_msg::<_, ClientMsg>(&mut recv).await {}
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
        room.peers.remove(&id);
        room.broadcast(id, &ServerMsg::PeerLeft { id });
    }
    info!("- {name} (id {id}): {reason}");
    Ok(())
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

fn fingerprint(cert: &CertificateDer<'_>) -> String {
    Sha256::digest(cert.as_ref()).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
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

//! Подключение к серверу по QUIC.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use bytes::Bytes;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use vwf_proto::{
    ChatMessage, ClientMsg, PROTOCOL_VERSION, PeerId, PeerInfo, ServerMsg, VideoHeader,
};

use crate::known_servers::KnownServers;

/// Имя, которое клиент ожидает в сертификате сервера.
const SERVER_NAME: &str = "vwf-server";

/// Активное подключение к серверу.
pub struct Session {
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    control_recv: quinn::RecvStream,
    control: Control,
    /// Адрес сервера (после разрешения имени).
    pub address: SocketAddr,
    /// Отпечаток сертификата сервера.
    pub fingerprint: String,
    /// Мы сами, как нас видит сервер (id, права хоста...).
    pub me: PeerInfo,
    /// Кто уже был в комнате в момент подключения.
    pub peers: Vec<PeerInfo>,
    /// Последние сообщения чата на момент подключения.
    pub chat: Vec<ChatMessage>,
}

/// Отправка управляющих сообщений серверу. Дешёво клонируется.
///
/// Сообщения пишет в поток отдельная задача. Когда все `Control` удалены,
/// она закрывает поток, и сервер понимает, что мы ушли.
#[derive(Clone)]
pub struct Control(tokio::sync::mpsc::UnboundedSender<ClientMsg>);

impl Control {
    pub fn send(&self, msg: ClientMsg) {
        // Ошибка означает, что соединение уже закрыто, — это заметит основной цикл.
        let _ = self.0.send(msg);
    }
}

/// Позволяет закрыть соединение, когда сама `Session` уже отдана другой задаче.
#[derive(Clone)]
pub struct Closer {
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
}

impl Closer {
    /// Почему соединение закрылось, если оно уже закрыто.
    pub fn close_reason(&self) -> Option<String> {
        Some(match self.conn.close_reason()? {
            quinn::ConnectionError::ApplicationClosed(close) => {
                String::from_utf8_lossy(&close.reason).into_owned()
            }
            quinn::ConnectionError::TimedOut => "сервер перестал отвечать".to_string(),
            other => other.to_string(),
        })
    }

    /// Сервер сам закрыл соединение (остановился, выгнал нас), а не связь
    /// пропала. После такого переподключаться не нужно.
    pub fn closed_by_server(&self) -> bool {
        matches!(
            self.conn.close_reason(),
            Some(
                quinn::ConnectionError::ApplicationClosed(_)
                    | quinn::ConnectionError::ConnectionClosed(_)
            )
        )
    }

    pub async fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
        // Даём серверу узнать о закрытии, но не ждём вечно.
        let _ = tokio::time::timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }
}

/// Отправка голоса. Дешёво клонируется и вызывается из потока кодера.
#[derive(Clone)]
pub struct VoiceSender(quinn::Connection);

impl VoiceSender {
    pub fn send(&self, seq: u32, opus: &[u8]) {
        let packet = Bytes::from(vwf_proto::encode_client_voice(seq, opus));
        // Ошибка означает, что соединение уже закрыто, — это заметит основной цикл.
        let _ = self.0.send_datagram(packet);
    }
}

/// Сколько кадров демонстрации может ждать отправки. Больше — сеть не
/// успевает, и показывающий пропускает кадры до опорного.
const VIDEO_SEND_QUEUE: usize = 3;

/// Отправка кадров демонстрации экрана: каждый кадр — отдельный
/// однонаправленный поток, по порядку. Создаётся внутри рантайма tokio.
#[derive(Clone)]
pub struct VideoSender(tokio::sync::mpsc::Sender<crate::screen::VideoFrame>);

impl VideoSender {
    fn spawn(conn: quinn::Connection) -> Self {
        let (tx, mut rx) =
            tokio::sync::mpsc::channel::<crate::screen::VideoFrame>(VIDEO_SEND_QUEUE);
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let mut packet = Vec::with_capacity(VideoHeader::LEN + frame.data.len());
                frame.header.encode(&mut packet);
                packet.extend_from_slice(&frame.data);
                let Ok(mut stream) = conn.open_uni().await else { break };
                // Голос (датаграммы) QUIC и так кладёт в пакет раньше потоков,
                // а приоритет ниже обычного пропускает вперёд управляющие
                // сообщения (чат, статусы).
                let _ = stream.set_priority(-1);
                if stream.write_all(&packet).await.is_err() {
                    break;
                }
                let _ = stream.finish();
            }
        });
        Self(tx)
    }

    /// Отдаёт кадр в сеть. `false` — очередь полна, кадр выброшен.
    /// Можно вызывать из любого потока.
    pub fn send(&self, frame: crate::screen::VideoFrame) -> bool {
        self.0.try_send(frame).is_ok()
    }
}

/// Приём кадров демонстрации экрана.
pub struct VideoReceiver(quinn::Connection);

impl VideoReceiver {
    /// Ждёт следующий кадр: `(показывающий, заголовок, H.264)`.
    /// `None` — соединение закрыто.
    pub async fn recv(&self) -> Option<(PeerId, VideoHeader, Vec<u8>)> {
        loop {
            let mut stream = self.0.accept_uni().await.ok()?;
            let limit = 4 + VideoHeader::LEN + vwf_proto::MAX_VIDEO_FRAME;
            let Ok(data) = stream.read_to_end(limit).await else { continue };
            let Some((from, rest)) = data.split_first_chunk::<4>() else { continue };
            let Some((header, h264)) = VideoHeader::decode(rest) else { continue };
            return Some((PeerId::from_le_bytes(*from), header, h264.to_vec()));
        }
    }
}

/// Приём голоса.
pub struct VoiceReceiver(quinn::Connection);

impl VoiceReceiver {
    /// Ждёт следующий голосовой пакет: `(отправитель, seq, opus)`.
    /// `None` — соединение закрыто.
    pub async fn recv(&self) -> Option<(PeerId, u32, Bytes)> {
        loop {
            let data = self.0.read_datagram().await.ok()?;
            if let Some((from, seq, opus)) = vwf_proto::decode_server_voice(&data) {
                let offset = data.len() - opus.len();
                return Some((from, seq, data.slice(offset..)));
            }
        }
    }
}

/// Добавляет порт по умолчанию, если он не указан, и резолвит имя.
async fn resolve(server: &str) -> anyhow::Result<SocketAddr> {
    if let Ok(addr) = server.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let with_port = if server.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        server.to_string()
    } else {
        format!("{server}:{}", vwf_proto::DEFAULT_PORT)
    };
    tokio::net::lookup_host(&with_port)
        .await
        .with_context(|| format!("не удалось найти адрес {with_port}"))?
        .next()
        .with_context(|| format!("у {with_port} нет IP-адресов"))
}

/// Как проверять сертификат сервера.
pub enum Trust<'a> {
    /// Принять любой (только для тестов и экспериментов).
    Any,
    /// Ожидаем ровно этот отпечаток (например, свой собственный сервер).
    Exact(&'a str),
    /// Запомнить при первом подключении, дальше сверять с запомненным.
    Known(&'a Mutex<KnownServers>),
}

/// Сертификат сервера не совпал с ожидаемым. Возможна подмена сервера —
/// или его честно переустановили. Решать пользователю.
#[derive(Debug, Clone)]
pub struct CertificateChanged {
    /// Адрес сервера, как он хранится в списке запомненных.
    pub address: String,
    pub known: String,
    pub presented: String,
}

impl std::fmt::Display for CertificateChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "сертификат сервера {} изменился: был код {}, теперь {}. Возможна подмена сервера, \
             или его переустановили — сверьте код с хостом",
            self.address,
            vwf_proto::short_fingerprint(&self.known),
            vwf_proto::short_fingerprint(&self.presented),
        )
    }
}

impl std::error::Error for CertificateChanged {}

static APP_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Платформа и версия программы, которые видят остальные в комнате
/// (`PeerInfo::app`), например `android 0.1.42`. Задаётся один раз при запуске.
pub fn set_app_version(app: String) {
    let _ = APP_VERSION.set(app);
}

fn app_version() -> String {
    APP_VERSION.get().cloned().unwrap_or_else(|| format!("cli {}", env!("CARGO_PKG_VERSION")))
}

/// С чем входить в комнату: секрет хоста (если мы хост) и пароль комнаты.
#[derive(Clone, Copy, Default, Debug)]
pub struct Credentials<'a> {
    pub admin_token: Option<&'a str>,
    pub password: Option<&'a str>,
}

pub async fn connect(server: &str, name: &str) -> anyhow::Result<Session> {
    connect_with(server, name, None, Trust::Any).await
}

/// Подключение с секретом хоста (см. `vwf_server::Config::admin_token`).
pub async fn connect_with_token(
    server: &str,
    name: &str,
    admin_token: Option<&str>,
) -> anyhow::Result<Session> {
    connect_with(server, name, admin_token, Trust::Any).await
}

/// Подключение с проверкой сертификата сервера. Вместо адреса можно передать
/// код приглашения (`poi-...`) — тогда подключение идёт через пробивку NAT.
pub async fn connect_with(
    server: &str,
    name: &str,
    admin_token: Option<&str>,
    trust: Trust<'_>,
) -> anyhow::Result<Session> {
    connect_as(server, name, Credentials { admin_token, password: None }, trust).await
}

/// То же, с паролем комнаты.
pub async fn connect_as(
    server: &str,
    name: &str,
    credentials: Credentials<'_>,
    trust: Trust<'_>,
) -> anyhow::Result<Session> {
    if vwf_nat::invite::looks_like_invite(server) {
        let mailbox = vwf_nat::mailbox::Mailbox::from_env()?;
        return connect_invite_as(server, name, credentials, &mailbox).await;
    }
    let addr = resolve(server).await?;
    let bind: SocketAddr =
        if addr.is_ipv6() { "[::]:0".parse().unwrap() } else { "0.0.0.0:0".parse().unwrap() };
    let endpoint = quinn::Endpoint::client(bind)?;

    let conn = tokio::time::timeout(
        CONNECT_WAIT,
        endpoint.connect_with(client_config()?, addr, SERVER_NAME)?,
    )
    .await
    .with_context(|| {
        format!(
            "сервер {addr} не отвечает. Проверьте, что сервер запущен (комната создана \
             или работает poigrulkiny-server) и что UDP-порт {} не закрыт брандмауэром",
            addr.port()
        )
    })?
    .with_context(|| format!("не удалось подключиться к {addr}"))?;

    handshake(endpoint, conn, addr, name, credentials, trust).await
}

/// Подключение по коду приглашения: встреча через почтовый ящик, пробивка
/// NAT, затем QUIC сразу на все известные адреса хоста — кто первый ответит.
/// Сертификат сверяется с отпечатком из кода: подменить сервер нельзя даже
/// при первом подключении.
pub async fn connect_invite(
    code: &str,
    name: &str,
    admin_token: Option<&str>,
    mailbox: &vwf_nat::mailbox::Mailbox,
) -> anyhow::Result<Session> {
    connect_invite_as(code, name, Credentials { admin_token, password: None }, mailbox).await
}

async fn connect_invite_as(
    code: &str,
    name: &str,
    credentials: Credentials<'_>,
    mailbox: &vwf_nat::mailbox::Mailbox,
) -> anyhow::Result<Session> {
    let invite = vwf_nat::invite::Invite::decode(code)?;
    let socket = vwf_nat::socket::NatSocket::bind("0.0.0.0:0".parse().unwrap())?;
    let endpoint = socket.endpoint(None)?;
    // Подписываемся до встречи: «пробойники» хоста начнут приходить сразу.
    let mut punches = socket.watch_punches();
    let found =
        vwf_nat::rendezvous::find_host(&socket, mailbox, &invite.topic(), FIND_HOST_WAIT).await?;

    let config = client_config()?;
    let mut attempts = tokio::task::JoinSet::new();
    let mut tried = std::collections::HashSet::new();
    let mut attempt = |attempts: &mut tokio::task::JoinSet<_>, addr: SocketAddr| {
        if !tried.insert(addr) {
            return;
        }
        let endpoint = endpoint.clone();
        let config = config.clone();
        attempts.spawn(async move {
            let attempt = async {
                let connecting = endpoint.connect_with(config, addr, SERVER_NAME)?;
                let conn = tokio::time::timeout(Duration::from_secs(10), connecting)
                    .await
                    .context("нет ответа")??;
                anyhow::Ok(conn)
            };
            (addr, attempt.await)
        });
    };
    for addr in found.candidates.iter().copied() {
        attempt(&mut attempts, addr);
    }
    let mut failures = Vec::new();
    let (conn, addr) = loop {
        let joined = tokio::select! {
            joined = attempts.join_next() => joined,
            // Хост стучится не с того адреса, что назвал (например, STUN у него
            // ходит через VPN, а к нам — напрямую). Пробуем и этот адрес.
            Some(from) = punches.recv() => {
                attempt(&mut attempts, from);
                continue;
            }
        };
        match joined {
            Some(Ok((addr, Ok(conn)))) => break (conn, addr),
            Some(Ok((addr, Err(e)))) => failures.push(format!("{addr}: {e:#}")),
            Some(Err(_)) => {}
            None if found.me.symmetric => bail!(
                "не удалось пробить NAT: ваш роутер или оператор выдаёт для каждого \
                 собеседника новый внешний порт («симметричный» NAT, так бывает у мобильного \
                 интернета). Попробуйте другую сеть (например, домашний Wi-Fi) или Tailscale"
            ),
            None => bail!(
                "не удалось пробить NAT до хоста ({}). Похоже, роутер хоста или ваш \
                 такого не позволяет. Если у вас или у хоста включён VPN (на компьютере, \
                 телефоне или роутере), исключите из него Поигрулькины. Иначе остаётся \
                 Tailscale или сервер на VPS",
                failures.join("; ")
            ),
        }
    };
    // Остальные попытки больше не нужны.
    attempts.abort_all();

    handshake(endpoint, conn, addr, name, credentials, Trust::Exact(&invite.fingerprint)).await
}

/// Запущена ли комната — для списка недавних комнат.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Online,
    Offline,
    /// Проверить не удалось (например, нет интернета у нас самих).
    Unknown,
}

/// Сколько ждать ответа на проверку по адресу. QUIC-рукопожатие с живым
/// сервером занимает доли секунды даже через интернет.
const PROBE_WAIT: Duration = Duration::from_millis(1500);
/// Сколько ждать ответа хоста через почтовый ящик: обычно 1–2 секунды.
const PROBE_INVITE_WAIT: Duration = Duration::from_secs(4);
/// Подключение по адресу: живой сервер отвечает за доли секунды, поэтому
/// не держим человека долго, если сервер не запущен.
const CONNECT_WAIT: Duration = Duration::from_secs(5);
/// Сколько ждать ответа хоста при входе по коду приглашения.
const FIND_HOST_WAIT: Duration = Duration::from_secs(10);

/// Быстро проверяет, запущена ли комната по адресу или коду приглашения, не
/// входя в неё: по адресу — только QUIC-рукопожатие, по коду — читает, что
/// хост недавно писал в почтовый ящик. `ask` — если хост давно молчит,
/// спросить его «ты тут?» (это сообщение из дневного лимита ntfy.sh, поэтому
/// только когда человек сам нажал на комнату).
pub async fn probe(server: &str, ask: bool) -> Availability {
    if vwf_nat::invite::looks_like_invite(server) {
        let Ok(mailbox) = vwf_nat::mailbox::Mailbox::from_env() else {
            return Availability::Unknown;
        };
        return probe_invite(server, &mailbox, ask, PROBE_INVITE_WAIT).await;
    }
    probe_address(server, PROBE_WAIT).await
}

pub async fn probe_address(server: &str, wait: Duration) -> Availability {
    let Ok(addr) = resolve(server).await else { return Availability::Unknown };
    let bind: SocketAddr =
        if addr.is_ipv6() { "[::]:0".parse().unwrap() } else { "0.0.0.0:0".parse().unwrap() };
    let Ok(endpoint) = quinn::Endpoint::client(bind) else { return Availability::Unknown };
    let Ok(connecting) = client_config()
        .and_then(|config| endpoint.connect_with(config, addr, SERVER_NAME).map_err(Into::into))
    else {
        return Availability::Unknown;
    };
    match tokio::time::timeout(wait, connecting).await {
        Ok(Ok(conn)) => {
            conn.close(vwf_proto::PROBE_CLOSE_CODE.into(), b"probe");
            // Даём закрытию дойти до сервера, но ответ не задерживаем.
            tokio::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
            });
            Availability::Online
        }
        _ => Availability::Offline,
    }
}

pub async fn probe_invite(
    code: &str,
    mailbox: &vwf_nat::mailbox::Mailbox,
    ask: bool,
    wait: Duration,
) -> Availability {
    let Ok(invite) = vwf_nat::invite::Invite::decode(code) else { return Availability::Unknown };
    let topic = invite.topic();
    match vwf_nat::rendezvous::host_seen(mailbox, &topic).await {
        Ok(Some(true)) => return Availability::Online,
        Ok(Some(false)) => return Availability::Offline,
        Ok(None) if !ask => return Availability::Offline,
        Ok(None) => {}
        Err(_) => return Availability::Unknown,
    }
    match vwf_nat::rendezvous::ping_host(mailbox, &topic, wait).await {
        Ok(true) => Availability::Online,
        Ok(false) => Availability::Offline,
        Err(_) => Availability::Unknown,
    }
}

/// Общая часть подключения после того, как QUIC-соединение установлено.
async fn handshake(
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    addr: SocketAddr,
    name: &str,
    credentials: Credentials<'_>,
    trust: Trust<'_>,
) -> anyhow::Result<Session> {
    // Сверяем сертификат сразу после рукопожатия — до того, как отправить
    // серверу хоть что-то (имя, секрет хоста).
    let fingerprint = presented_fingerprint(&conn)?;
    let address = addr.to_string();
    let expected = match &trust {
        Trust::Any => None,
        Trust::Exact(fp) => Some(fp.to_string()),
        Trust::Known(known) => known.lock().unwrap().get(&address).map(str::to_string),
    };
    if let Some(known) = expected
        && known != fingerprint
    {
        conn.close(0u32.into(), b"certificate changed");
        return Err(CertificateChanged { address, known, presented: fingerprint }.into());
    }

    let (mut send, mut recv) = conn.open_bi().await?;
    vwf_proto::write_msg(
        &mut send,
        &ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            name: name.to_string(),
            admin_token: credentials.admin_token.map(str::to_string),
        },
    )
    .await?;
    let password = credentials.password.unwrap_or_default().to_string();
    vwf_proto::write_msg(&mut send, &ClientMsg::Auth { password, app: app_version() }).await?;

    let reply = tokio::time::timeout(Duration::from_secs(10), vwf_proto::read_msg(&mut recv))
        .await
        .context("сервер не ответил на приветствие")?
        // Не разобрали ответ — у сервера почти наверняка другая версия
        // программы (новее или старее, чем у нас).
        .map_err(|e| {
            anyhow::anyhow!(
                "у сервера другая версия программы: обновите «Поигрулькиных» у всех \
                 до одной версии ({e})"
            )
        })?;
    match reply {
        Some(ServerMsg::Welcome { you, peers, chat }) => {
            let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                while let Some(msg) = control_rx.recv().await {
                    if vwf_proto::write_msg(&mut send, &msg).await.is_err() {
                        break;
                    }
                }
                let _ = send.finish();
            });
            // Первое подключение к серверу: запоминаем его сертификат.
            if let Trust::Known(known) = trust {
                let mut known = known.lock().unwrap();
                if known.get(&address).is_none() {
                    // Не смогли записать файл — подключиться это не мешает.
                    let _ = known.remember(&address, &fingerprint);
                }
            }
            Ok(Session {
                endpoint,
                conn,
                control_recv: recv,
                control: Control(control_tx),
                address: addr,
                fingerprint,
                me: you,
                peers,
                chat,
            })
        }
        Some(ServerMsg::Rejected { reason }) => bail!("сервер отказал: {reason}"),
        Some(other) => bail!("неожиданный ответ сервера: {other:?}"),
        None => bail!("сервер закрыл соединение"),
    }
}

impl Session {
    pub fn voice_sender(&self) -> VoiceSender {
        VoiceSender(self.conn.clone())
    }

    pub fn voice_receiver(&self) -> VoiceReceiver {
        VoiceReceiver(self.conn.clone())
    }

    /// Отправка кадров демонстрации (вызывать внутри рантайма tokio).
    pub fn video_sender(&self) -> VideoSender {
        VideoSender::spawn(self.conn.clone())
    }

    pub fn video_receiver(&self) -> VideoReceiver {
        VideoReceiver(self.conn.clone())
    }

    pub fn control(&self) -> Control {
        self.control.clone()
    }

    pub fn closer(&self) -> Closer {
        Closer { endpoint: self.endpoint.clone(), conn: self.conn.clone() }
    }

    /// Ждёт следующее событие от сервера. `Ok(None)` — сервер закрыл соединение.
    pub async fn next_event(&mut self) -> anyhow::Result<Option<ServerMsg>> {
        vwf_proto::read_msg(&mut self.control_recv).await
    }

    pub async fn close(self) {
        self.closer().close().await;
    }
}

/// Отпечаток сертификата, который предъявил сервер.
fn presented_fingerprint(conn: &quinn::Connection) -> anyhow::Result<String> {
    let certs = conn
        .peer_identity()
        .and_then(|identity| identity.downcast::<Vec<CertificateDer<'static>>>().ok())
        .context("сервер не предъявил сертификат")?;
    let cert = certs.first().context("сервер не предъявил сертификат")?;
    Ok(vwf_proto::cert_fingerprint(cert))
}

fn client_config() -> anyhow::Result<quinn::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert(provider)))
        .with_no_client_auth();
    tls.alpn_protocols = vec![vwf_proto::ALPN.to_vec()];

    let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    transport.max_idle_timeout(Some(Duration::from_secs(20).try_into()?));
    config.transport_config(Arc::new(transport));
    Ok(config)
}

/// Сервер использует самоподписанный сертификат, удостоверяющего центра нет,
/// поэтому на этапе рукопожатия принимаем любой. Настоящая проверка — сразу
/// после него: отпечаток сверяется с запомненным (см. `Trust`).
#[derive(Debug)]
struct AcceptAnyServerCert(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

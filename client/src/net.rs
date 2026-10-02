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
use vwf_proto::{ClientMsg, PROTOCOL_VERSION, PeerId, PeerInfo, ServerMsg};

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
    if vwf_nat::invite::looks_like_invite(server) {
        let mailbox = vwf_nat::mailbox::Mailbox::from_env()?;
        return connect_invite(server, name, admin_token, &mailbox).await;
    }
    let addr = resolve(server).await?;
    let bind: SocketAddr =
        if addr.is_ipv6() { "[::]:0".parse().unwrap() } else { "0.0.0.0:0".parse().unwrap() };
    let endpoint = quinn::Endpoint::client(bind)?;

    let conn = tokio::time::timeout(
        Duration::from_secs(10),
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

    handshake(endpoint, conn, addr, name, admin_token, trust).await
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
    let invite = vwf_nat::invite::Invite::decode(code)?;
    let socket = vwf_nat::socket::NatSocket::bind("0.0.0.0:0".parse().unwrap())?;
    let endpoint = socket.endpoint(None)?;
    let found =
        vwf_nat::rendezvous::find_host(&socket, mailbox, &invite.topic(), Duration::from_secs(20))
            .await?;

    let config = client_config()?;
    let mut attempts = tokio::task::JoinSet::new();
    for addr in found.candidates.iter().copied() {
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
    }
    let mut failures = Vec::new();
    let (conn, addr) = loop {
        match attempts.join_next().await {
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
                 такого не позволяет — остаётся Tailscale или сервер на VPS",
                failures.join("; ")
            ),
        }
    };
    // Остальные попытки больше не нужны.
    attempts.abort_all();

    handshake(endpoint, conn, addr, name, admin_token, Trust::Exact(&invite.fingerprint)).await
}

/// Общая часть подключения после того, как QUIC-соединение установлено.
async fn handshake(
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    addr: SocketAddr,
    name: &str,
    admin_token: Option<&str>,
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
            admin_token: admin_token.map(str::to_string),
        },
    )
    .await?;

    let reply = tokio::time::timeout(Duration::from_secs(10), vwf_proto::read_msg(&mut recv))
        .await
        .context("сервер не ответил на приветствие")??;
    match reply {
        Some(ServerMsg::Welcome { you, peers }) => {
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

//! Подключение к серверу по QUIC.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use bytes::Bytes;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use vwf_proto::{ClientMsg, PROTOCOL_VERSION, PeerId, PeerInfo, ServerMsg};

/// Имя, которое клиент ожидает в сертификате сервера.
const SERVER_NAME: &str = "vwf-server";

/// Активное подключение к серверу.
pub struct Session {
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    control_recv: quinn::RecvStream,
    control: Control,
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

pub async fn connect(server: &str, name: &str) -> anyhow::Result<Session> {
    connect_with_token(server, name, None).await
}

/// Подключение с секретом хоста (см. `vwf_server::Config::admin_token`).
pub async fn connect_with_token(
    server: &str,
    name: &str,
    admin_token: Option<&str>,
) -> anyhow::Result<Session> {
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
            Ok(Session {
                endpoint,
                conn,
                control_recv: recv,
                control: Control(control_tx),
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

/// Сервер использует самоподписанный сертификат, поэтому пока принимаем любой.
/// Трафик всё равно зашифрован, но подмену сервера клиент не заметит.
/// TODO: запоминать отпечаток сертификата при первом подключении (как SSH).
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

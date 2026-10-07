//! Встреча хоста и гостя через почтовый ящик и пробивка NAT.
//!
//! 1. Каждый узнаёт у STUN-сервера свой внешний адрес.
//! 2. Гость кладёт в ящик «я тут: адрес такой-то», хост отвечает своим адресом.
//! 3. Хост несколько секунд шлёт гостю «пробивные» пакеты, а гость сразу
//!    стучится к хосту по QUIC. Свой роутер каждый «открывает» исходящими
//!    пакетами, а встречные пакеты чужой роутер принимает за ответы.
//!
//! Не сработает, если роутер «симметричный» (для каждого собеседника выдаёт
//! новый внешний порт) — такое бывает у мобильных операторов и части провайдеров.
//! Тогда нужен посредник: Tailscale или сервер на VPS.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::mailbox::{Mailbox, MailboxEvent};
use crate::socket::NatSocket;
use crate::stun;

/// Пакет-«пробойник»: не QUIC и не STUN, получатель его просто выбросит.
const PUNCH: &[u8] = b"\0poigrulkiny-punch";
const PUNCH_COUNT: u32 = 20;
const PUNCH_INTERVAL: Duration = Duration::from_millis(200);
/// Как часто хост обновляет внешний адрес. Заодно запрос к STUN не даёт
/// роутеру забыть открытый порт, пока никто не разговаривает.
const HOST_REFRESH: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Signal {
    Join {
        id: u64,
        public: SocketAddr,
        local: Vec<SocketAddr>,
    },
    Host {
        reply_to: u64,
        public: SocketAddr,
        local: Vec<SocketAddr>,
    },
    /// «Комната открыта?» — проверка из списка недавних, без пробивки NAT.
    Ping {
        id: u64,
    },
    Pong {
        reply_to: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NatInfo {
    pub public: SocketAddr,
    /// Разные STUN-серверы видят нас с разных портов — NAT «симметричный».
    pub symmetric: bool,
}

/// Узнаёт внешний адрес у двух STUN-серверов и заодно тип NAT.
pub async fn discover(socket: &NatSocket) -> anyhow::Result<NatInfo> {
    let mut seen = Vec::new();
    for server in stun::servers() {
        let Some(addr) = resolve_v4(&server).await else { continue };
        match socket.stun(addr, Duration::from_millis(1500)).await {
            Ok(public) => seen.push(public),
            Err(e) => debug!("{e:#}"),
        }
        if seen.len() == 2 {
            break;
        }
    }
    let Some(&public) = seen.first() else {
        bail!(
            "не удалось узнать внешний адрес: STUN-серверы не отвечают \
             (нет интернета или исходящий UDP закрыт)"
        );
    };
    Ok(NatInfo { public, symmetric: seen.len() == 2 && seen[0] != seen[1] })
}

async fn resolve_v4(server: &str) -> Option<SocketAddr> {
    tokio::net::lookup_host(server).await.ok()?.find(SocketAddr::is_ipv4)
}

/// Адреса этого компьютера в локальных сетях — на случай, если гость окажется
/// в той же сети, что и хост.
fn local_addrs(port: u16) -> Vec<SocketAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.ip().is_ipv4() && !i.is_loopback() && !i.is_link_local())
        .map(|i| SocketAddr::new(i.ip(), port))
        .collect()
}

fn spawn_punch(socket: &Arc<NatSocket>, to: SocketAddr) {
    // Слабая ссылка: если комнату закрыли, сокет (и порт) освобождается сразу,
    // не дожидаясь конца пробивки.
    let socket = Arc::downgrade(socket);
    tokio::spawn(async move {
        for _ in 0..PUNCH_COUNT {
            let Some(socket) = socket.upgrade() else { return };
            socket.send_raw(to, PUNCH);
            drop(socket);
            tokio::time::sleep(PUNCH_INTERVAL).await;
        }
    });
}

// ---------------------------------------------------------------------------
// Хост
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct HostStatus {
    pub public: Option<SocketAddr>,
    pub symmetric: bool,
    /// Последняя ошибка (нет интернета, ящик недоступен…).
    pub error: Option<String>,
    /// Сколько раз гости стучались.
    pub guests: u32,
}

/// Работающая «встреча» на стороне хоста; останавливается при удалении.
pub struct HostRendezvous {
    status: Arc<Mutex<HostStatus>>,
    task: JoinHandle<()>,
}

impl HostRendezvous {
    pub fn start(socket: Arc<NatSocket>, mailbox: Mailbox, topic: String) -> Self {
        let status = Arc::new(Mutex::new(HostStatus::default()));
        let task = tokio::spawn(host_loop(socket, mailbox, topic, status.clone()));
        Self { status, task }
    }

    pub fn status(&self) -> HostStatus {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for HostRendezvous {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn host_loop(
    socket: Arc<NatSocket>,
    mailbox: Mailbox,
    topic: String,
    status: Arc<Mutex<HostStatus>>,
) {
    let port = socket.local_addr().map(|a| a.port()).unwrap_or_default();
    let mut public = None;
    let mut sub = mailbox.subscribe(&topic);
    let mut refresh = tokio::time::interval(HOST_REFRESH);

    loop {
        tokio::select! {
            _ = refresh.tick() => match discover(&socket).await {
                Ok(info) => {
                    if public != Some(info.public) {
                        info!("внешний адрес: {} (NAT симметричный: {})", info.public, info.symmetric);
                    }
                    public = Some(info.public);
                    let mut s = status.lock().unwrap();
                    s.public = Some(info.public);
                    s.symmetric = info.symmetric;
                    s.error = None;
                }
                Err(e) => {
                    warn!("{e:#}");
                    status.lock().unwrap().error = Some(format!("{e:#}"));
                }
            },
            event = sub.recv() => {
                let Some(event) = event else { break };
                let MailboxEvent::Message(text) = event else { continue };
                let signal = serde_json::from_str(&text);
                if let Ok(Signal::Ping { id }) = signal {
                    publish_reply(&mailbox, &topic, Signal::Pong { reply_to: id });
                    continue;
                }
                let Ok(Signal::Join { id, public: guest, local: _ }) = signal else {
                    continue;
                };
                let Some(me) = public else { continue };
                info!("гость стучится с {guest}");
                status.lock().unwrap().guests += 1;
                spawn_punch(&socket, guest);
                let reply = Signal::Host { reply_to: id, public: me, local: local_addrs(port) };
                publish_reply(&mailbox, &topic, reply);
            }
        }
    }
}

/// Отвечает гостю в ящик, не задерживая цикл хоста.
fn publish_reply(mailbox: &Mailbox, topic: &str, reply: Signal) {
    let mailbox = mailbox.clone();
    let topic = topic.to_string();
    tokio::spawn(async move {
        let text = serde_json::to_string(&reply).expect("сериализация не падает");
        if let Err(e) = mailbox.publish(&topic, &text).await {
            warn!("не удалось ответить гостю: {e:#}");
        }
    });
}

// ---------------------------------------------------------------------------
// Гость
// ---------------------------------------------------------------------------

/// Что гость узнал о хосте и о себе.
#[derive(Debug, Clone)]
pub struct FoundHost {
    /// Адреса хоста в том порядке, в каком их стоит пробовать.
    pub candidates: Vec<SocketAddr>,
    /// Наш собственный NAT.
    pub me: NatInfo,
}

/// Открыта ли комната с этой темой: хост, если он запущен и слушает ящик,
/// отвечает на `Ping` за секунду-две. `Ok(false)` — за `wait` ответа не было;
/// ошибка — сам ящик недоступен (например, нет интернета), и тогда про
/// комнату ничего сказать нельзя.
pub async fn ping_host(mailbox: &Mailbox, topic: &str, wait: Duration) -> anyhow::Result<bool> {
    let deadline = Instant::now() + wait;
    let mut sub = mailbox.subscribe(topic);
    loop {
        match tokio::time::timeout_at(deadline, sub.recv()).await {
            Ok(Some(MailboxEvent::Open)) => break,
            Ok(Some(_)) => continue,
            _ => bail!("сервис обмена адресами недоступен — проверьте интернет"),
        }
    }

    let id = u64::from_le_bytes(crate::random_bytes());
    let ping = serde_json::to_string(&Signal::Ping { id }).expect("сериализация не падает");
    // Повтор — на случай, если хост пропустил первое сообщение.
    let mut resend = tokio::time::interval(Duration::from_secs(3));
    loop {
        tokio::select! {
            _ = resend.tick() => {
                mailbox.publish(topic, &ping).await.context("не удалось связаться с хостом")?;
            }
            event = tokio::time::timeout_at(deadline, sub.recv()) => match event {
                Ok(Some(MailboxEvent::Message(text))) => {
                    if let Ok(Signal::Pong { reply_to }) = serde_json::from_str(&text)
                        && reply_to == id
                    {
                        return Ok(true);
                    }
                }
                Ok(Some(MailboxEvent::Open)) => {}
                Ok(None) | Err(_) => return Ok(false),
            },
        }
    }
}

/// Находит хоста через почтовый ящик и начинает пробивку.
pub async fn find_host(
    socket: &Arc<NatSocket>,
    mailbox: &Mailbox,
    topic: &str,
    wait: Duration,
) -> anyhow::Result<FoundHost> {
    let me = discover(socket).await?;
    if me.symmetric {
        warn!("у вас «симметричный» NAT: пробивка может не получиться");
    }
    let deadline = Instant::now() + wait;

    let mut sub = mailbox.subscribe(topic);
    // Ждём, пока подписка установится, — иначе можно пропустить ответ хоста.
    loop {
        match tokio::time::timeout_at(deadline, sub.recv()).await {
            Ok(Some(MailboxEvent::Open)) => break,
            Ok(Some(_)) => continue,
            _ => bail!("сервис обмена адресами недоступен — проверьте интернет"),
        }
    }

    let id = u64::from_le_bytes(crate::random_bytes());
    let port = socket.local_addr()?.port();
    let join =
        serde_json::to_string(&Signal::Join { id, public: me.public, local: local_addrs(port) })
            .expect("сериализация не падает");
    let mut resend = tokio::time::interval(Duration::from_secs(3));

    let host = loop {
        tokio::select! {
            _ = resend.tick() => {
                // Хост мог пропустить первое сообщение — повторяем.
                mailbox.publish(topic, &join).await.context("не удалось связаться с хостом")?;
            }
            event = tokio::time::timeout_at(deadline, sub.recv()) => match event {
                Ok(Some(MailboxEvent::Message(text))) => {
                    if let Ok(Signal::Host { reply_to, public, local }) = serde_json::from_str(&text)
                        && reply_to == id
                    {
                        break (public, local);
                    }
                }
                Ok(Some(MailboxEvent::Open)) => {}
                Ok(None) | Err(_) => bail!(
                    "хост не ответил: комната не создана, у хоста выключен доступ через \
                     интернет или у него нет интернета"
                ),
            },
        }
    };

    let (public, local) = host;
    spawn_punch(socket, public);
    // Хост за тем же роутером (один внешний IP) — сначала пробуем его
    // локальные адреса: многие роутеры не умеют «развернуть» пакет внутрь.
    let same_network = public.ip() == me.public.ip() && !matches!(public.ip(), IpAddr::V6(_));
    let mut candidates = Vec::new();
    if same_network {
        candidates.extend(local.iter().copied());
        candidates.push(public);
    } else {
        candidates.push(public);
        candidates.extend(local.iter().copied());
    }
    candidates.dedup();
    Ok(FoundHost { candidates, me })
}

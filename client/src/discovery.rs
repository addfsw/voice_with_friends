//! Поиск серверов в локальной сети (см. описание протокола в `vwf_proto`).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::Instant;
use vwf_proto::{DISCOVERY_PROBE, PROTOCOL_VERSION};

/// Сколько раз повторять запрос за время поиска: UDP может терять пакеты.
const PROBES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundServer {
    pub name: String,
    /// Адрес для подключения.
    pub address: SocketAddr,
    pub users: u32,
    /// Совпадает ли версия протокола с нашей (иначе подключиться не получится).
    pub compatible: bool,
}

/// Ищет серверы в локальной сети, ожидая ответов `wait`.
pub async fn discover(wait: Duration) -> anyhow::Result<Vec<FoundServer>> {
    discover_on(vwf_proto::DISCOVERY_PORT, wait).await
}

/// То же, но с другим портом поиска (для тестов).
pub async fn discover_on(port: u16, wait: Duration) -> anyhow::Result<Vec<FoundServer>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_broadcast(true)?;
    let targets = probe_targets(port);

    let start = Instant::now();
    let deadline = start + wait;
    let mut probes_sent = 0;
    let mut next_probe = start;
    // Один сервер может ответить несколько раз — через разные сетевые
    // интерфейсы. Склеиваем ответы по номеру запуска сервера.
    let mut found: HashMap<u64, FoundServer> = HashMap::new();
    let mut buf = [0u8; 1024];

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if probes_sent < PROBES && now >= next_probe {
            for target in &targets {
                // Ошибки отправки (например, интерфейс без сети) не мешают остальным.
                let _ = socket.send_to(DISCOVERY_PROBE, target).await;
            }
            probes_sent += 1;
            next_probe = start + wait * probes_sent / PROBES;
        }
        let wake_at = if probes_sent < PROBES { next_probe.min(deadline) } else { deadline };
        let received = tokio::time::timeout_at(wake_at, socket.recv_from(&mut buf)).await;
        // Таймаут — пора слать следующий запрос или заканчивать. Ошибка приёма
        // на Windows бывает, если на 127.0.0.1 никто не слушает, — пропускаем.
        let Ok(Ok((len, from))) = received else { continue };
        let Some(announcement) = vwf_proto::decode_announcement(&buf[..len]) else { continue };

        let server = FoundServer {
            name: announcement.name,
            address: SocketAddr::new(from.ip(), announcement.port),
            users: announcement.users,
            compatible: announcement.protocol == PROTOCOL_VERSION,
        };
        // Если сервер на этом же компьютере, он ответит и с 127.0.0.1, и с
        // адреса в локальной сети. Показываем сетевой: его можно сказать друзьям.
        match found.get(&announcement.instance) {
            Some(known) if !known.address.ip().is_loopback() => {}
            _ => {
                found.insert(announcement.instance, server);
            }
        }
    }

    let mut servers: Vec<FoundServer> = found.into_values().collect();
    servers.sort_by(|a, b| a.name.cmp(&b.name).then(a.address.cmp(&b.address)));
    Ok(servers)
}

/// Куда слать запрос: общий широковещательный адрес, широковещательные адреса
/// каждого интерфейса (на компьютерах с несколькими сетями общий уходит не во все)
/// и 127.0.0.1 — на случай сервера на этом же компьютере.
fn probe_targets(port: u16) -> Vec<SocketAddr> {
    let mut targets = vec![
        SocketAddr::from((Ipv4Addr::BROADCAST, port)),
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
    ];
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for interface in interfaces {
            if let if_addrs::IfAddr::V4(v4) = interface.addr
                && let Some(broadcast) = v4.broadcast
                && !v4.ip.is_loopback()
            {
                targets.push(SocketAddr::from((broadcast, port)));
            }
        }
    }
    targets.sort();
    targets.dedup();
    targets
}

//! Открытие порта на домашнем роутере по UPnP (IGD).
//!
//! Пробивке хватает STUN, пока весь трафик идёт одной дорогой. Но если VPN на
//! роутере или компьютере ведёт через туннель только часть сайтов, STUN-серверы
//! (Google, Cloudflare) видят адрес VPN, а пакеты к собеседнику уходят напрямую
//! через провайдера, и встречные пакеты стучатся не туда. Роутер же по UPnP сам
//! называет свой настоящий внешний адрес и пробрасывает порт к нам: по этому
//! адресу нас достанут без всякой пробивки, если только сам роутер не за NAT
//! провайдера. UPnP включён не везде, поэтому это лишь ещё один адрес-кандидат.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, bail};
use igd_next::aio::Gateway;
use igd_next::aio::tokio::{Tokio, search_gateway};
use igd_next::{GatewayIpVersion, PortMappingProtocol, SearchOptions};

/// Роутер в той же сети отвечает за доли секунды.
const SEARCH_WAIT: Duration = Duration::from_secs(2);
const DESCRIPTION: &str = "Poigrulkiny";

/// UPnP можно выключить переменной `VWF_UPNP=0` (испытательный стенд).
pub fn enabled() -> bool {
    std::env::var("VWF_UPNP").map_or(true, |v| v.trim() != "0")
}

/// Проброшенный порт на роутере. Сам по себе не закрывается: у проброса есть
/// срок, и роутер убирает его, если не продлевать.
pub struct PortMapping {
    gateway: Gateway<Tokio>,
    local: SocketAddr,
    /// Адрес, по которому нас видно из интернета.
    pub public: SocketAddr,
}

impl PortMapping {
    /// Просит роутер пробросить UDP-порт `local_port` этого компьютера.
    pub async fn open(local_port: u16, lease: Duration) -> anyhow::Result<Self> {
        if !enabled() {
            bail!("UPnP выключен");
        }
        let mut options = SearchOptions::default();
        options.timeout = Some(SEARCH_WAIT);
        options.single_search_timeout = Some(SEARCH_WAIT);
        options.gateway_ip_version = GatewayIpVersion::V4;
        let gateway = search_gateway(options).await.context("роутер не отвечает по UPnP")?;
        let IpAddr::V4(external) = gateway.get_external_ip().await? else {
            bail!("роутер назвал адрес IPv6");
        };
        if !is_public(external) {
            bail!("у роутера нет своего внешнего адреса ({external}): он сам за NAT провайдера");
        }
        let local = SocketAddr::new(local_ip_towards(gateway.addr)?, local_port);
        let secs = lease.as_secs() as u32;
        // Тот же внешний порт, что и у нас, если он свободен: так понятнее в настройках роутера.
        let port = match gateway
            .add_port(PortMappingProtocol::UDP, local_port, local, secs, DESCRIPTION)
            .await
        {
            Ok(()) => local_port,
            Err(_) => {
                gateway.add_any_port(PortMappingProtocol::UDP, local, secs, DESCRIPTION).await?
            }
        };
        Ok(Self { gateway, local, public: SocketAddr::new(IpAddr::V4(external), port) })
    }

    /// Продлевает проброс ещё на `lease`.
    pub async fn renew(&self, lease: Duration) -> anyhow::Result<()> {
        let secs = lease.as_secs() as u32;
        self.gateway
            .add_port(PortMappingProtocol::UDP, self.public.port(), self.local, secs, DESCRIPTION)
            .await?;
        Ok(())
    }
}

/// Наш адрес в той сети, где роутер: система сама выберет нужный адаптер.
fn local_ip_towards(gateway: SocketAddr) -> anyhow::Result<IpAddr> {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0")?;
    probe.connect(gateway)?;
    Ok(probe.local_addr()?.ip())
}

/// Адрес, до которого можно достучаться из интернета: не локальный и не
/// «серый» адрес провайдера (100.64.0.0/10).
fn is_public(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || (a == 100 && (b & 0xC0) == 64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_global_addresses_are_public() {
        for ip in ["192.168.1.1", "10.0.0.5", "172.20.1.1", "100.72.3.4", "0.0.0.0", "169.254.1.1"]
        {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["92.55.5.211", "8.8.8.8", "100.128.0.1"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }
}

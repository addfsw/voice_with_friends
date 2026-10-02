//! Сервер внутри приложения: кнопка «Создать комнату».
//!
//! Это тот же `vwf-server`, что и отдельная программа, только запущенный
//! в процессе приложения. Хост сам подключается к нему как обычный клиент.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::bail;
use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use vwf_proto::{DEFAULT_PORT, DISCOVERY_PORT};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HostAddress {
    /// Сетевой интерфейс: «Ethernet», «Wi-Fi», «Tailscale»…
    pub interface: String,
    /// Адрес, который нужно сказать друзьям.
    pub address: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub name: String,
    pub port: u16,
    pub addresses: Vec<HostAddress>,
}

pub struct HostedServer {
    endpoint: quinn::Endpoint,
    task: JoinHandle<()>,
    /// Секрет хоста: с ним наше собственное подключение получает права
    /// заглушать и выгонять. Каждый запуск комнаты — новый.
    admin_token: String,
    pub info: HostInfo,
}

impl HostedServer {
    /// `data_dir` — где хранить сертификат сервера, чтобы он не менялся между запусками.
    pub async fn start(name: String, data_dir: PathBuf) -> anyhow::Result<Self> {
        let admin_token = random_token();
        let config = || vwf_server::Config {
            bind: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
            echo: false,
            data_dir: Some(data_dir.clone()),
            name: name.clone(),
            discovery: Some(SocketAddr::from(([0, 0, 0, 0], DISCOVERY_PORT))),
            admin_token: Some(admin_token.clone()),
        };

        // Если комнату только что закрыли, порт освобождается не мгновенно
        // (десятки миллисекунд), поэтому немного подождём.
        let started = Instant::now();
        let server = loop {
            match vwf_server::Server::bind(config()) {
                Ok(server) => break server,
                Err(e) if is_addr_in_use(&e) => {
                    if started.elapsed() > Duration::from_secs(1) {
                        bail!(
                            "порт {DEFAULT_PORT} занят: похоже, на этом компьютере уже запущен \
                             vwf-server или ещё одна копия программы с комнатой"
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e),
            }
        };

        let port = server.local_addr()?.port();
        let endpoint = server.endpoint().clone();
        let task = tauri::async_runtime::spawn(server.run());
        let addresses = vwf_server::reachable_addresses(port)
            .into_iter()
            .map(|(interface, addr)| HostAddress {
                interface,
                address: if port == DEFAULT_PORT {
                    addr.ip().to_string()
                } else {
                    addr.to_string()
                },
            })
            .collect();
        Ok(Self { endpoint, task, admin_token, info: HostInfo { name, port, addresses } })
    }

    /// Секрет хоста, если `server` — это адрес нашего собственного сервера.
    pub fn admin_token_for(&self, server: &str) -> Option<&str> {
        (server == self.local_address()).then_some(self.admin_token.as_str())
    }

    /// Адрес, по которому к серверу подключается сам хост.
    pub fn local_address(&self) -> String {
        format!("127.0.0.1:{}", self.info.port)
    }

    /// Останавливает сервер: все участники получат «сервер остановлен».
    pub async fn stop(self) {
        self.endpoint.close(0u32.into(), vwf_proto::SERVER_STOPPED_REASON.as_bytes());
        let _ = tokio::time::timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
        drop(self.endpoint);
        let _ = self.task.await;
    }
}

/// 128 случайных бит в шестнадцатеричном виде. Источник случайности — тот же,
/// что у `HashMap` в стандартной библиотеке (ключи из ОС при старте процесса).
fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = || {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        hasher.finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

fn is_addr_in_use(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::AddrInUse)
    })
}

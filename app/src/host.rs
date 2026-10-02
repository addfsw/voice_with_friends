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
    pub info: HostInfo,
}

impl HostedServer {
    /// `data_dir` — где хранить сертификат сервера, чтобы он не менялся между запусками.
    pub async fn start(name: String, data_dir: PathBuf) -> anyhow::Result<Self> {
        let config = || vwf_server::Config {
            bind: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
            echo: false,
            data_dir: Some(data_dir.clone()),
            name: name.clone(),
            discovery: Some(SocketAddr::from(([0, 0, 0, 0], DISCOVERY_PORT))),
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
        Ok(Self { endpoint, task, info: HostInfo { name, port, addresses } })
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

fn is_addr_in_use(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::AddrInUse)
    })
}

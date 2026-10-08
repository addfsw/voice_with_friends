//! Сервер внутри приложения: свои комнаты на этом компьютере.
//!
//! Это тот же `vwf-server`, что и отдельная программа, только запущенный
//! в процессе приложения. Хост сам подключается к нему как обычный клиент.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::bail;
use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use vwf_nat::invite::{Invite, load_or_create_room};
use vwf_nat::mailbox::Mailbox;
use vwf_nat::rendezvous::HostRendezvous;
use vwf_nat::socket::NatSocket;
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
    /// Какая из своих комнат открыта (см. `rooms.rs`).
    pub room_id: String,
    pub name: String,
    pub port: u16,
    pub addresses: Vec<HostAddress>,
    /// Короткий код сертификата, чтобы друзья могли его сверить.
    pub code: String,
}

/// Состояние доступа через интернет (пробивка NAT) для интерфейса.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct InternetStatus {
    pub enabled: bool,
    /// Код приглашения для друзей.
    pub invite: String,
    /// Внешний адрес, если уже известен.
    pub public: Option<String>,
    pub symmetric: bool,
    pub error: Option<String>,
    pub guests: u32,
}

pub struct HostedServer {
    endpoint: quinn::Endpoint,
    /// Общий сокет QUIC и пробивки NAT.
    socket: Arc<NatSocket>,
    invite: Invite,
    rendezvous: Mutex<Option<HostRendezvous>>,
    task: JoinHandle<()>,
    /// Секрет хоста: с ним наше собственное подключение получает права
    /// заглушать и выгонять. Каждый запуск комнаты — новый.
    admin_token: String,
    fingerprint: String,
    locked: Arc<AtomicBool>,
    lan: Arc<AtomicBool>,
    access: Arc<vwf_server::Access>,
    pub info: HostInfo,
}

impl HostedServer {
    /// Открывает комнату `room`. `data_dir` — где хранить сертификат сервера,
    /// чтобы он не менялся между запусками; `room_file` — номер комнаты для
    /// её кода приглашения.
    pub async fn start(
        room: &crate::rooms::RoomConfig,
        room_file: PathBuf,
        data_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        let name = room.name.clone();
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
        let bind = || {
            let socket = NatSocket::bind(SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)))?;
            let server = vwf_server::Server::bind_with_socket(config(), Some(socket.clone() as _))?;
            anyhow::Ok((server, socket))
        };
        let (server, socket) = loop {
            match bind() {
                Ok(bound) => break bound,
                Err(e) if is_addr_in_use(&e) => {
                    if started.elapsed() > Duration::from_secs(1) {
                        bail!(
                            "порт {DEFAULT_PORT} занят: похоже, на этом компьютере уже запущен \
                             poigrulkiny-server или ещё одна копия программы с комнатой"
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e),
            }
        };

        let port = server.local_addr()?.port();
        let fingerprint = server.fingerprint().to_string();
        let invite = Invite::new(load_or_create_room(&room_file)?, &fingerprint);
        let code = vwf_proto::short_fingerprint(&fingerprint);
        let endpoint = server.endpoint().clone();
        let locked = server.lock_switch();
        let lan = server.discovery_switch();
        lan.store(room.lan, Ordering::Relaxed);
        let access = server.access();
        access.set_password(&room.password);
        access.set_max_peers(room.max_peers);
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
        Ok(Self {
            endpoint,
            socket,
            invite,
            rendezvous: Mutex::new(None),
            task,
            admin_token,
            fingerprint,
            locked,
            lan,
            access,
            info: HostInfo { room_id: room.id.clone(), name, port, addresses, code },
        })
    }

    /// Если `server` — адрес нашего собственного сервера: секрет хоста
    /// и отпечаток сертификата, который он должен предъявить.
    pub fn own(&self, server: &str) -> Option<(String, String)> {
        (server == self.local_address())
            .then(|| (self.admin_token.clone(), self.fingerprint.clone()))
    }

    /// Включает или выключает доступ через интернет. Пока он включён, программа
    /// обращается к STUN-серверам и к почтовому ящику (ntfy.sh).
    pub fn set_internet(&self, enabled: bool) -> anyhow::Result<()> {
        let mut rendezvous = self.rendezvous.lock().unwrap();
        if enabled && rendezvous.is_none() {
            let mailbox = Mailbox::from_env()?;
            *rendezvous =
                Some(HostRendezvous::start(self.socket.clone(), mailbox, self.invite.topic()));
        } else if !enabled {
            *rendezvous = None;
        }
        Ok(())
    }

    pub fn internet_status(&self) -> InternetStatus {
        let rendezvous = self.rendezvous.lock().unwrap();
        let mut status =
            InternetStatus { invite: self.invite.encode(), ..InternetStatus::default() };
        if let Some(rendezvous) = rendezvous.as_ref() {
            let host = rendezvous.status();
            status.enabled = true;
            status.public = host.public.map(|a| a.to_string());
            status.symmetric = host.symmetric;
            status.error = host.error;
            status.guests = host.guests;
        }
        status
    }

    /// «Никого больше не пускать»: новые участники получают отказ.
    pub fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::Relaxed);
    }

    /// Видна ли комната в списке «В локальной сети».
    pub fn set_lan(&self, visible: bool) {
        self.lan.store(visible, Ordering::Relaxed);
    }

    /// Пароль и лимит участников — сразу, для следующих входящих.
    pub fn set_access(&self, password: &str, max_peers: u32) {
        self.access.set_password(password);
        self.access.set_max_peers(max_peers);
    }

    /// Адрес, по которому к серверу подключается сам хост.
    pub fn local_address(&self) -> String {
        format!("127.0.0.1:{}", self.info.port)
    }

    /// Останавливает сервер: все участники получат «сервер остановлен».
    pub async fn stop(self) {
        // Сначала перестаём отвечать гостям, потом закрываем сервер.
        self.rendezvous.lock().unwrap().take();
        drop(self.socket);
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

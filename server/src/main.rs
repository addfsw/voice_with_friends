use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use tracing::info;

/// Поигрулькины: сервер голосового чата.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Адрес и UDP-порт, на котором слушать.
    #[arg(long, default_value_t = SocketAddr::from(([0, 0, 0, 0], vwf_proto::DEFAULT_PORT)))]
    bind: SocketAddr,

    /// Возвращать каждому его собственный голос (проверка связи в одиночку).
    #[arg(long)]
    echo: bool,

    /// Папка для сертификата и ключа сервера.
    #[arg(long, default_value = ".")]
    data_dir: PathBuf,

    /// Название сервера для поиска в локальной сети. По умолчанию — имя компьютера.
    #[arg(long)]
    name: Option<String>,

    /// Не отвечать на поиск серверов в локальной сети.
    #[arg(long)]
    no_discovery: bool,

    /// Доступ через интернет без белого IP: пробивка NAT и код приглашения,
    /// который друзья вставляют в поле «Сервер».
    #[arg(long)]
    internet: bool,

    /// Секрет для прав хоста. Кто подключится с ним (`poigrulkiny-cli connect ...
    /// --admin-token ...`), сможет заглушать и выгонять участников.
    #[arg(long)]
    admin_token: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    // Для пробивки NAT сервер работает на сокете, который умеет и STUN.
    let nat_socket = match args.internet {
        true => Some(vwf_nat::socket::NatSocket::bind(args.bind)?),
        false => None,
    };
    let room_file = args.data_dir.join("room_id.txt");
    let server = vwf_server::Server::bind_with_socket(
        vwf_server::Config {
            bind: args.bind,
            echo: args.echo,
            data_dir: Some(args.data_dir),
            name: args.name.unwrap_or_else(vwf_server::default_server_name),
            discovery: (!args.no_discovery)
                .then(|| SocketAddr::from(([0, 0, 0, 0], vwf_proto::DISCOVERY_PORT))),
            admin_token: args.admin_token,
        },
        nat_socket.clone().map(|s| s as _),
    )?;
    let addr = server.local_addr()?;
    info!("сервер слушает UDP {addr}");
    info!(
        "код сервера: {}  (полный отпечаток сертификата: {})",
        vwf_proto::short_fingerprint(server.fingerprint()),
        server.fingerprint()
    );
    if let Some(addr) = server.discovery_addr() {
        info!("отвечаю на поиск в локальной сети: UDP {addr}");
    }
    if args.echo {
        info!("режим эха включён: каждый слышит сам себя");
    }
    if addr.ip().is_unspecified() {
        info!("друзья могут подключаться по одному из адресов:");
        for (iface, a) in vwf_server::reachable_addresses(addr.port()) {
            info!("    {a}    ({iface})");
        }
    }
    // Держим «встречу» живой, пока работает сервер.
    let _rendezvous = match nat_socket {
        Some(socket) => {
            let room = vwf_nat::invite::load_or_create_room(&room_file)?;
            let invite = vwf_nat::invite::Invite::new(room, server.fingerprint());
            info!("доступ через интернет включён. Код приглашения для друзей:");
            info!("    {}", invite.encode());
            let mailbox = vwf_nat::mailbox::Mailbox::from_env()?;
            Some(vwf_nat::rendezvous::HostRendezvous::start(socket, mailbox, invite.topic()))
        }
        None => None,
    };
    info!("остановить: Ctrl+C");

    let endpoint = server.endpoint().clone();
    tokio::select! {
        _ = server.run() => {}
        _ = tokio::signal::ctrl_c() => {
            info!("останавливаюсь...");
            endpoint.close(0u32.into(), vwf_proto::SERVER_STOPPED_REASON.as_bytes());
        }
    }
    endpoint.wait_idle().await;
    Ok(())
}

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use tracing::info;

/// Сервер голосового чата voice_with_friends.
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let server = vwf_server::Server::bind(vwf_server::Config {
        bind: args.bind,
        echo: args.echo,
        data_dir: Some(args.data_dir),
    })?;
    let addr = server.local_addr()?;
    info!("сервер слушает UDP {addr}");
    info!("отпечаток сертификата: {}", server.fingerprint());
    if args.echo {
        info!("режим эха включён: каждый слышит сам себя");
    }
    if addr.ip().is_unspecified() {
        info!("друзья могут подключаться по одному из адресов:");
        for (iface, a) in vwf_server::reachable_addresses(addr.port()) {
            info!("    {a}    ({iface})");
        }
    }
    info!("остановить: Ctrl+C");

    let endpoint = server.endpoint().clone();
    tokio::select! {
        _ = server.run() => {}
        _ = tokio::signal::ctrl_c() => {
            info!("останавливаюсь...");
            endpoint.close(0u32.into(), b"server shutdown");
        }
    }
    endpoint.wait_idle().await;
    Ok(())
}

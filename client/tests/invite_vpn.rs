//! Хост узнал у STUN «не тот» внешний адрес: так бывает, когда VPN ведёт через
//! туннель только часть трафика — STUN-серверы видят адрес VPN, а к гостю
//! пакеты идут напрямую. Гость всё равно подключается — по адресу, с которого
//! к нему на самом деле пришли «пробойники» хоста.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;
use vwf_nat::invite::Invite;
use vwf_nat::mailbox::Mailbox;
use vwf_nat::rendezvous::HostRendezvous;
use vwf_nat::socket::NatSocket;

/// STUN-сервер, который хосту (`liar_for`) называет чужой адрес — адрес «VPN»,
/// где никто не слушает, — а остальным отвечает честно.
async fn start_stun_server(liar_for: SocketAddr, vpn: SocketAddr) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        while let Ok((len, from)) = socket.recv_from(&mut buf).await {
            if let Some(id) = vwf_nat::stun::request_id(&buf[..len]) {
                let seen = if from == liar_for { vpn } else { from };
                let _ = socket.send_to(&vwf_nat::stun::binding_response(&id, seen), from).await;
            }
        }
    });
    addr
}

#[tokio::test]
async fn guest_connects_to_where_host_punches_from() {
    // Тест в отдельном файле = отдельный процесс, других потоков ещё нет.
    unsafe { std::env::set_var("VWF_UPNP", "0") };
    let socket = NatSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let vpn: SocketAddr = "127.0.0.9:9".parse().unwrap();
    let stun = start_stun_server(socket.local_addr().unwrap(), vpn).await;
    unsafe { std::env::set_var("VWF_STUN_SERVERS", stun.to_string()) };

    let mailbox = Mailbox::memory();
    let server = vwf_server::Server::bind_with_socket(
        vwf_server::Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            echo: true,
            data_dir: None,
            name: "Комната".into(),
            discovery: None,
            admin_token: None,
        },
        Some(socket.clone() as Arc<dyn quinn::AsyncUdpSocket>),
    )
    .unwrap();
    let fingerprint = server.fingerprint().to_string();
    tokio::spawn(server.run());

    let invite = Invite::new(vwf_nat::new_room_id(), &fingerprint);
    let host = HostRendezvous::start(socket, mailbox.clone(), invite.topic());
    timeout(Duration::from_secs(5), async {
        while host.status().public.is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("хост не узнал свой адрес");
    assert_eq!(host.status().public, Some(vpn));

    let session = timeout(
        Duration::from_secs(15),
        vwf_client::net::connect_invite(&invite.encode(), "Гость", None, &mailbox),
    )
    .await
    .expect("подключение по коду зависло")
    .unwrap();
    assert_eq!(session.fingerprint, fingerprint);
}

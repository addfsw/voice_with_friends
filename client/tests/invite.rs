//! Подключение по коду приглашения на localhost: свой STUN-сервер, почтовый
//! ящик в памяти. NAT здесь нет — настоящая пробивка проверяется на стенде
//! (`scripts/nat-lab.sh`), а тут — вся логика встречи и подключения.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;
use vwf_nat::invite::Invite;
use vwf_nat::mailbox::Mailbox;
use vwf_nat::rendezvous::HostRendezvous;
use vwf_nat::socket::NatSocket;

/// Простейший STUN-сервер: отвечает, с какого адреса пришёл запрос.
async fn start_stun_server() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        while let Ok((len, from)) = socket.recv_from(&mut buf).await {
            if let Some(id) = vwf_nat::stun::request_id(&buf[..len]) {
                let _ = socket.send_to(&vwf_nat::stun::binding_response(&id, from), from).await;
            }
        }
    });
    addr
}

#[tokio::test]
async fn guest_joins_by_invite_code() {
    let stun = start_stun_server().await;
    // Тест в отдельном файле = отдельный процесс, других потоков ещё нет.
    unsafe { std::env::set_var("VWF_STUN_SERVERS", stun.to_string()) };

    let mailbox = Mailbox::memory();
    let socket = NatSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
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
    // Хост сначала узнаёт свой внешний адрес.
    timeout(Duration::from_secs(5), async {
        while host.status().public.is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("хост не узнал свой адрес");

    let session = timeout(
        Duration::from_secs(15),
        vwf_client::net::connect_invite(&invite.encode(), "Гость", None, &mailbox),
    )
    .await
    .expect("подключение по коду зависло")
    .unwrap();
    assert_eq!(session.fingerprint, fingerprint);
    assert_eq!(host.status().guests, 1);

    // Голос ходит (у сервера включено эхо).
    let rx = session.voice_receiver();
    for seq in 0..5 {
        session.voice_sender().send(seq, b"hello");
    }
    let (_, _, opus) = timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
    assert_eq!(&opus[..], b"hello");

    // Код с чужим отпечатком: найти хоста получится, но подключиться — нет.
    let forged = Invite { fingerprint: "00:".repeat(31) + "00", ..invite };
    let error = vwf_client::net::connect_invite(&forged.encode(), "Гость", None, &mailbox)
        .await
        .err()
        .expect("подмена не замечена");
    assert!(error.downcast_ref::<vwf_client::net::CertificateChanged>().is_some(), "{error:#}");
}

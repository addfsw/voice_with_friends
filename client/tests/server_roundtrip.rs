//! Сквозной тест сети: настоящий сервер на localhost и два клиента.

use std::time::Duration;

use tokio::time::timeout;
use vwf_proto::ServerMsg;

async fn start_server(echo: bool) -> std::net::SocketAddr {
    let server = vwf_server::Server::bind(vwf_server::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        echo,
        data_dir: None,
        name: "Тестовый".into(),
        discovery: None,
    })
    .unwrap();
    let addr = server.local_addr().unwrap();
    tokio::spawn(server.run());
    addr
}

const T: Duration = Duration::from_secs(5);

#[tokio::test]
async fn voice_is_forwarded_to_others_only() {
    let addr = start_server(false).await.to_string();

    let mut alice = vwf_client::net::connect(&addr, "Алиса").await.unwrap();
    assert!(alice.peers.is_empty());

    let bob = vwf_client::net::connect(&addr, "Боб").await.unwrap();
    assert_eq!(bob.peers.len(), 1);
    assert_eq!(bob.peers[0].name, "Алиса");

    match timeout(T, alice.next_event()).await.unwrap().unwrap() {
        Some(ServerMsg::PeerJoined(p)) => assert_eq!((p.id, p.name.as_str()), (bob.my_id, "Боб")),
        other => panic!("ожидали PeerJoined, получили {other:?}"),
    }

    // Датаграммы ненадёжны даже на localhost, поэтому шлём несколько.
    let alice_rx = alice.voice_receiver();
    let bob_rx = bob.voice_receiver();
    for seq in 0..5 {
        alice.voice_sender().send(seq, b"opus-frame");
    }
    let (from, seq, opus) = timeout(T, bob_rx.recv()).await.unwrap().unwrap();
    assert_eq!(from, alice.my_id);
    assert!(seq < 5);
    assert_eq!(&opus[..], b"opus-frame");

    // Без режима эха отправитель сам себя не слышит.
    assert!(timeout(Duration::from_millis(300), alice_rx.recv()).await.is_err());

    let bob_id = bob.my_id;
    bob.close().await;
    match timeout(T, alice.next_event()).await.unwrap().unwrap() {
        Some(ServerMsg::PeerLeft { id }) => assert_eq!(id, bob_id),
        other => panic!("ожидали PeerLeft, получили {other:?}"),
    }
}

#[tokio::test]
async fn echo_mode_returns_own_voice() {
    let addr = start_server(true).await.to_string();
    let solo = vwf_client::net::connect(&addr, "Соло").await.unwrap();
    let rx = solo.voice_receiver();
    for seq in 0..5 {
        solo.voice_sender().send(seq, b"hello");
    }
    let (from, _, opus) = timeout(T, rx.recv()).await.unwrap().unwrap();
    assert_eq!(from, solo.my_id);
    assert_eq!(&opus[..], b"hello");
}

#[tokio::test]
async fn server_is_found_by_discovery() {
    let server = vwf_server::Server::bind(vwf_server::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        echo: false,
        data_dir: None,
        name: "Комната Пети".into(),
        discovery: Some("127.0.0.1:0".parse().unwrap()),
    })
    .unwrap();
    let addr = server.local_addr().unwrap();
    let discovery_port = server.discovery_addr().unwrap().port();
    tokio::spawn(server.run());

    // Один участник уже в комнате — это должно попасть в ответ.
    let _alice = vwf_client::net::connect(&addr.to_string(), "Алиса").await.unwrap();

    let found = vwf_client::discovery::discover_on(discovery_port, Duration::from_millis(500))
        .await
        .unwrap();
    assert_eq!(
        found,
        vec![vwf_client::discovery::FoundServer {
            name: "Комната Пети".into(),
            address: addr,
            users: 1,
            compatible: true,
        }]
    );
}

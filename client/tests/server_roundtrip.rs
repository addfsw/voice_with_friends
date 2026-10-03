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
        admin_token: None,
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
        Some(ServerMsg::PeerJoined(p)) => assert_eq!((p.id, p.name.as_str()), (bob.me.id, "Боб")),
        other => panic!("ожидали PeerJoined, получили {other:?}"),
    }

    // Датаграммы ненадёжны даже на localhost, поэтому шлём несколько.
    let alice_rx = alice.voice_receiver();
    let bob_rx = bob.voice_receiver();
    for seq in 0..5 {
        alice.voice_sender().send(seq, b"opus-frame");
    }
    let (from, seq, opus) = timeout(T, bob_rx.recv()).await.unwrap().unwrap();
    assert_eq!(from, alice.me.id);
    assert!(seq < 5);
    assert_eq!(&opus[..], b"opus-frame");

    // Без режима эха отправитель сам себя не слышит.
    assert!(timeout(Duration::from_millis(300), alice_rx.recv()).await.is_err());

    let bob_id = bob.me.id;
    bob.close().await;
    match timeout(T, alice.next_event()).await.unwrap().unwrap() {
        Some(ServerMsg::PeerLeft { id }) => assert_eq!(id, bob_id),
        other => panic!("ожидали PeerLeft, получили {other:?}"),
    }
}

#[tokio::test]
async fn echo_can_be_switched_while_running() {
    let server = vwf_server::Server::bind(vwf_server::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        echo: false,
        data_dir: None,
        name: "Тестовый".into(),
        discovery: None,
        admin_token: None,
    })
    .unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let echo = server.echo_switch();
    tokio::spawn(server.run());

    let solo = vwf_client::net::connect(&addr, "Соло").await.unwrap();
    let rx = solo.voice_receiver();
    solo.voice_sender().send(0, b"off");
    assert!(timeout(Duration::from_millis(300), rx.recv()).await.is_err());

    echo.store(true, std::sync::atomic::Ordering::Relaxed);
    for seq in 1..6 {
        solo.voice_sender().send(seq, b"on");
    }
    let (_, _, opus) = timeout(T, rx.recv()).await.unwrap().unwrap();
    assert_eq!(&opus[..], b"on");
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
    assert_eq!(from, solo.me.id);
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
        admin_token: None,
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

#[tokio::test]
async fn stopped_server_releases_its_ports() {
    let config = |bind: &str, discovery: &str| vwf_server::Config {
        bind: bind.parse().unwrap(),
        echo: false,
        data_dir: None,
        name: "Временный".into(),
        discovery: Some(discovery.parse().unwrap()),
        admin_token: None,
    };
    let server = vwf_server::Server::bind(config("127.0.0.1:0", "127.0.0.1:0")).unwrap();
    let addr = server.local_addr().unwrap();
    let discovery = server.discovery_addr().unwrap();
    let endpoint = server.endpoint().clone();
    let run = tokio::spawn(server.run());

    endpoint.close(0u32.into(), b"stop");
    timeout(T, run).await.unwrap().unwrap();
    endpoint.wait_idle().await;
    drop(endpoint);

    // Порты освобождаются: тот же сервер можно запустить снова (как после
    // «Остановить сервер» в приложении). Сокет QUIC закрывает фоновая задача
    // quinn, поэтому даём ей немного времени.
    let started = std::time::Instant::now();
    let again = loop {
        match vwf_server::Server::bind(config(&addr.to_string(), &discovery.to_string())) {
            Ok(server) => break server,
            Err(_) if started.elapsed() < Duration::from_secs(2) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("порт не освободился: {e:#}"),
        }
    };
    assert_eq!(again.discovery_addr(), Some(discovery));
}

/// Ждёт от сервера событие, подходящее под `matches`, пропуская остальные.
async fn expect_event(
    session: &mut vwf_client::net::Session,
    mut matches: impl FnMut(&ServerMsg) -> bool,
) -> ServerMsg {
    timeout(T, async {
        loop {
            let event = session.next_event().await.unwrap().expect("соединение закрыто");
            if matches(&event) {
                return event;
            }
        }
    })
    .await
    .expect("не дождались события")
}

#[tokio::test]
async fn host_can_mute_and_kick_but_guest_cannot() {
    use vwf_proto::ClientMsg;

    let server = vwf_server::Server::bind(vwf_server::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        echo: false,
        data_dir: None,
        name: "Комната".into(),
        discovery: None,
        admin_token: Some("секрет".into()),
    })
    .unwrap();
    let addr = server.local_addr().unwrap().to_string();
    tokio::spawn(server.run());

    let mut host =
        vwf_client::net::connect_with_token(&addr, "Хост", Some("секрет")).await.unwrap();
    assert!(host.me.admin);
    let mut guest =
        vwf_client::net::connect_with_token(&addr, "Гость", Some("неверный")).await.unwrap();
    assert!(!guest.me.admin);
    assert!(guest.peers[0].admin, "гость видит, кто хост");
    let guest_id = guest.me.id;
    expect_event(&mut host, |e| matches!(e, ServerMsg::PeerJoined(_))).await;

    // Статус гостя видят все.
    guest.control().send(ClientMsg::SetStatus { muted: true, deafened: false });
    match expect_event(&mut host, |e| matches!(e, ServerMsg::PeerUpdated(_))).await {
        ServerMsg::PeerUpdated(p) => assert!(p.id == guest_id && p.muted && !p.deafened),
        _ => unreachable!(),
    }

    // Гость не может выгнать хоста: команда игнорируется.
    guest.control().send(ClientMsg::Kick { id: host.me.id });

    // Хост заглушает гостя: тот узнаёт об этом, а его голос не доходит.
    host.control().send(ClientMsg::SetServerMute { id: guest_id, muted: true });
    match expect_event(&mut guest, |e| matches!(e, ServerMsg::PeerUpdated(p) if p.server_muted))
        .await
    {
        ServerMsg::PeerUpdated(p) => assert_eq!(p.id, guest_id),
        _ => unreachable!(),
    }
    let host_rx = host.voice_receiver();
    for seq in 0..5 {
        guest.voice_sender().send(seq, b"muted");
    }
    assert!(timeout(Duration::from_millis(300), host_rx.recv()).await.is_err());

    // Голос возвращён — пакеты снова доходят.
    host.control().send(ClientMsg::SetServerMute { id: guest_id, muted: false });
    expect_event(&mut guest, |e| matches!(e, ServerMsg::PeerUpdated(p) if !p.server_muted)).await;
    for seq in 5..10 {
        guest.voice_sender().send(seq, b"back");
    }
    let (from, _, opus) = timeout(T, host_rx.recv()).await.unwrap().unwrap();
    assert_eq!((from, &opus[..]), (guest_id, &b"back"[..]));

    // Хост выгоняет гостя.
    host.control().send(ClientMsg::Kick { id: guest_id });
    let closer = guest.closer();
    let ended = timeout(T, async { while let Ok(Some(_)) = guest.next_event().await {} }).await;
    assert!(ended.is_ok(), "гостя не отключили");
    assert_eq!(closer.close_reason().as_deref(), Some(vwf_proto::KICKED_REASON));
    match expect_event(&mut host, |e| matches!(e, ServerMsg::PeerLeft { .. })).await {
        ServerMsg::PeerLeft { id } => assert_eq!(id, guest_id),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn changed_server_certificate_is_detected() {
    use std::sync::Mutex;
    use vwf_client::known_servers::KnownServers;
    use vwf_client::net::{CertificateChanged, Trust, connect_with};

    let config = |bind: &str| vwf_server::Config {
        bind: bind.parse().unwrap(),
        echo: false,
        data_dir: None, // каждый запуск — новый сертификат
        name: "Комната".into(),
        discovery: None,
        admin_token: None,
    };
    let known = Mutex::new(KnownServers::in_memory());

    let first = vwf_server::Server::bind(config("127.0.0.1:0")).unwrap();
    let addr = first.local_addr().unwrap().to_string();
    let first_fp = first.fingerprint().to_string();
    let endpoint = first.endpoint().clone();
    let run = tokio::spawn(first.run());

    // Первое подключение запоминает сертификат, второе с ним сверяется.
    let session = connect_with(&addr, "Алиса", None, Trust::Known(&known)).await.unwrap();
    assert_eq!(session.fingerprint, first_fp);
    assert_eq!(known.lock().unwrap().get(&addr), Some(first_fp.as_str()));
    session.close().await;
    connect_with(&addr, "Алиса", None, Trust::Known(&known)).await.unwrap().close().await;

    // На том же адресе теперь другой сервер — с другим сертификатом.
    endpoint.close(0u32.into(), b"stop");
    timeout(T, run).await.unwrap().unwrap();
    endpoint.wait_idle().await;
    drop(endpoint);
    let started = std::time::Instant::now();
    let second = loop {
        match vwf_server::Server::bind(config(&addr)) {
            Ok(server) => break server,
            Err(_) if started.elapsed() < Duration::from_secs(2) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("порт не освободился: {e:#}"),
        }
    };
    let second_fp = second.fingerprint().to_string();
    tokio::spawn(second.run());

    let error = connect_with(&addr, "Алиса", None, Trust::Known(&known)).await.err().unwrap();
    let changed = error.downcast_ref::<CertificateChanged>().expect("ожидали CertificateChanged");
    assert_eq!((changed.known.as_str(), changed.presented.as_str()), (&*first_fp, &*second_fp));
    // Запомненный сертификат сам по себе не заменился.
    assert_eq!(known.lock().unwrap().get(&addr), Some(first_fp.as_str()));

    // Пользователь подтвердил новый — подключение проходит.
    known.lock().unwrap().remember(&addr, &second_fp).unwrap();
    connect_with(&addr, "Алиса", None, Trust::Known(&known)).await.unwrap().close().await;
    // Явно ожидаемый отпечаток тоже проверяется.
    assert!(connect_with(&addr, "Алиса", None, Trust::Exact(&first_fp)).await.is_err());
}

#[tokio::test]
async fn chat_reaches_everyone_and_newcomers_get_history() {
    use vwf_proto::ClientMsg;

    let addr = start_server(false).await.to_string();
    let mut alice = vwf_client::net::connect(&addr, "Алиса").await.unwrap();
    let mut bob = vwf_client::net::connect(&addr, "Боб").await.unwrap();
    assert!(bob.chat.is_empty());

    // Управляющие символы сервер вырезает, перенос строки оставляет.
    alice
        .control()
        .send(ClientMsg::Chat { text: "  привет\u{7}\nкак дела?  ".into() });
    let is_chat = |e: &ServerMsg| matches!(e, ServerMsg::Chat(_));
    let ServerMsg::Chat(to_bob) = expect_event(&mut bob, is_chat).await else { unreachable!() };
    assert_eq!((to_bob.from, to_bob.name.as_str()), (alice.me.id, "Алиса"));
    assert_eq!(to_bob.text, "привет\nкак дела?");
    // Отправитель тоже получает своё сообщение: так у всех одинаковый порядок.
    let ServerMsg::Chat(to_alice) = expect_event(&mut alice, is_chat).await else { unreachable!() };
    assert_eq!(to_alice, to_bob);

    // Пустые сообщения не рассылаются, а флуд режется.
    bob.control().send(ClientMsg::Chat { text: "   ".into() });
    for i in 0..15 {
        bob.control().send(ClientMsg::Chat { text: format!("спам {i}") });
    }
    let mut received = Vec::new();
    while let Ok(Ok(Some(event))) = timeout(Duration::from_millis(500), alice.next_event()).await {
        if let ServerMsg::Chat(m) = event {
            received.push(m.text);
        }
    }
    assert_eq!(received, (0..10).map(|i| format!("спам {i}")).collect::<Vec<_>>());

    // Кто пришёл позже, видит историю: первое сообщение и пропущенный спам.
    let carol = vwf_client::net::connect(&addr, "Кэрол").await.unwrap();
    assert_eq!(carol.chat.len(), 11);
    assert_eq!(carol.chat[0], to_bob);
    assert!(carol.chat.windows(2).all(|w| w[0].id < w[1].id));
}

/// Синтетический «экран» для теста: полоски, которые сдвигаются каждый кадр.
struct MovingStripes(u32);

impl vwf_client::screen::FrameSource for MovingStripes {
    fn frame(
        &mut self,
        deadline: std::time::Instant,
    ) -> anyhow::Result<Option<vwf_client::screen::Image>> {
        std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
        self.0 += 4;
        let (w, h) = (320u32, 240u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = (((x + self.0) / 16 + y / 16) % 2 * 200 + 30) as u8;
                rgba.extend_from_slice(&[v, v / 2, 255 - v, 255]);
            }
        }
        Ok(Some(vwf_client::screen::Image { width: w, height: h, rgba }))
    }
}

#[tokio::test]
async fn screen_share_reaches_only_watchers() {
    use vwf_client::screen::{ShareSettings, Sharer, Viewer};
    use vwf_proto::ClientMsg;

    let addr = start_server(false).await.to_string();
    let mut alice = vwf_client::net::connect(&addr, "Алиса").await.unwrap();
    let mut bob = vwf_client::net::connect(&addr, "Боб").await.unwrap();
    let carol = vwf_client::net::connect(&addr, "Кэрол").await.unwrap();
    let alice_id = alice.me.id;

    // Алиса показывает экран. Пока никто не смотрит, кадров нет.
    alice.control().send(ClientMsg::SetSharing { sharing: true });
    let video = alice.video_sender();
    let sharer = Sharer::start_with(
        || Ok(Box::new(MovingStripes(0))),
        ShareSettings { max_height: 0, fps: 30 },
        move |frame| video.send(frame),
        |e| panic!("демонстрация прервалась: {e}"),
    )
    .unwrap();
    expect_event(&mut bob, |e| matches!(e, ServerMsg::PeerUpdated(p) if p.sharing)).await;

    // Боб начинает смотреть: Алиса узнаёт о зрителе и что нужен опорный кадр.
    bob.control().send(ClientMsg::Watch { id: alice_id, watch: true });
    expect_event(&mut alice, |e| matches!(e, ServerMsg::Viewers { count: 1 })).await;
    sharer.set_viewers(1);
    sharer.request_keyframe();

    let (frames_tx, mut frames) = tokio::sync::mpsc::unbounded_channel();
    let viewer = Viewer::start(move |f| drop(frames_tx.send(f)), |_| {}).unwrap();
    let receiver = bob.video_receiver();
    let decoded = timeout(T, async {
        let mut decoded = Vec::new();
        while decoded.len() < 5 {
            tokio::select! {
                Some((from, header, data)) = receiver.recv() => {
                    assert_eq!(from, alice_id);
                    viewer.push(from, header, data);
                }
                Some(frame) = frames.recv() => decoded.push(frame),
            }
        }
        decoded
    })
    .await
    .expect("кадры не дошли");
    assert!(decoded.iter().all(|f| (f.from, f.width, f.height) == (alice_id, 320, 240)));

    // Кэрол не смотрит — ей кадры не идут.
    let carol_video = carol.video_receiver();
    assert!(timeout(Duration::from_millis(300), carol_video.recv()).await.is_err());

    // Боб перестал смотреть — зрителей ноль; Алиса закончила — все это видят.
    bob.control().send(ClientMsg::Watch { id: alice_id, watch: false });
    expect_event(&mut alice, |e| matches!(e, ServerMsg::Viewers { count: 0 })).await;
    drop(sharer);
    alice.control().send(ClientMsg::SetSharing { sharing: false });
    expect_event(
        &mut bob,
        |e| matches!(e, ServerMsg::PeerUpdated(p) if p.id == alice_id && !p.sharing),
    )
    .await;
}

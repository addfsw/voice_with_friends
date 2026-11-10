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
    // Версию программы собеседника видно (у консольного клиента — «cli …»).
    assert!(bob.peers[0].app.starts_with("cli "), "{}", bob.peers[0].app);

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
            password: false,
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

#[tokio::test]
async fn locked_room_lets_in_only_host_and_returning_members() {
    use std::sync::atomic::Ordering;
    use vwf_proto::ClientMsg;

    let server = vwf_server::Server::bind(vwf_server::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        echo: false,
        data_dir: None,
        name: "Комната".into(),
        discovery: Some("127.0.0.1:0".parse().unwrap()),
        admin_token: Some("секрет".into()),
    })
    .unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let discovery_port = server.discovery_addr().unwrap().port();
    let lock = server.lock_switch();
    let visible = server.discovery_switch();
    tokio::spawn(server.run());

    let host = vwf_client::net::connect_with_token(&addr, "Хост", Some("секрет")).await.unwrap();
    let mut alice = vwf_client::net::connect(&addr, "Алиса").await.unwrap();
    let mallory = vwf_client::net::connect(&addr, "Мэллори").await.unwrap();
    let _carol = vwf_client::net::connect(&addr, "Кэрол").await.unwrap();

    // Хост выгоняет Мэллори и закрывает вход.
    host.control().send(ClientMsg::Kick { id: mallory.me.id });
    expect_event(&mut alice, |e| matches!(e, ServerMsg::PeerLeft { .. })).await;
    lock.store(true, Ordering::Relaxed);

    // Новичка не пускают — с понятной причиной.
    let err = vwf_client::net::connect(&addr, "Боб").await.err().expect("Боба не должны пустить");
    assert!(format!("{err:#}").contains(vwf_proto::LOCKED_REASON), "{err:#}");
    // Выгнанный тоже не возвращается.
    assert!(vwf_client::net::connect(&addr, "Мэллори").await.is_err());

    // Алиса, у которой оборвалась связь, возвращается и в закрытую комнату.
    alice.close().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    vwf_client::net::connect(&addr, "Алиса").await.expect("Алиса возвращается");
    // У Кэрол связь пропала молча: сервер ещё считает её в комнате, а программа
    // уже переподключается. Её тоже пускают.
    vwf_client::net::connect(&addr, "Кэрол").await.expect("Кэрол переподключается");

    // Открыли вход — Боб заходит.
    lock.store(false, Ordering::Relaxed);
    vwf_client::net::connect(&addr, "Боб").await.expect("вход открыт");

    // Комнату спрятали из локальной сети — поиск её не находит.
    visible.store(false, Ordering::Relaxed);
    let found = vwf_client::discovery::discover_on(discovery_port, Duration::from_millis(400))
        .await
        .unwrap();
    assert!(found.is_empty(), "{found:?}");
}

#[tokio::test]
async fn avatars_reach_everyone_and_newcomers() {
    use vwf_proto::ClientMsg;

    let addr = start_server(false).await.to_string();
    let alice = vwf_client::net::connect(&addr, "Алиса").await.unwrap();
    let mut bob = vwf_client::net::connect(&addr, "Боб").await.unwrap();
    let jpeg = [&[0xFF, 0xD8, 0xFF, 0xE0][..], &[7; 100]].concat();

    // Алиса ставит аватарку — Боб получает её.
    alice.control().send(ClientMsg::SetAvatar { data: jpeg.clone() });
    let is_avatar = |e: &ServerMsg| matches!(e, ServerMsg::Avatar { .. });
    let got = expect_event(&mut bob, is_avatar).await;
    assert_eq!(got, ServerMsg::Avatar { id: alice.me.id, data: jpeg.clone() });

    // Слишком большую сервер не принимает и не рассылает.
    alice.control().send(ClientMsg::SetAvatar { data: vec![0; vwf_proto::MAX_AVATAR_BYTES + 1] });

    // Новичок получает аватарки тех, кто уже в комнате, сразу после Welcome.
    let mut carol = vwf_client::net::connect(&addr, "Кэрол").await.unwrap();
    let got = expect_event(&mut carol, is_avatar).await;
    assert_eq!(got, ServerMsg::Avatar { id: alice.me.id, data: jpeg.clone() });

    // Убрала аватарку — у остальных она тоже пропадает.
    alice.control().send(ClientMsg::SetAvatar { data: Vec::new() });
    let got = expect_event(&mut bob, is_avatar).await;
    assert_eq!(got, ServerMsg::Avatar { id: alice.me.id, data: Vec::new() });
}

#[tokio::test]
async fn password_and_capacity() {
    use vwf_client::net::{Credentials, Trust, connect_as};

    let server = vwf_server::Server::bind(vwf_server::Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        echo: false,
        data_dir: None,
        name: "Комната".into(),
        discovery: Some("127.0.0.1:0".parse().unwrap()),
        admin_token: Some("секрет".into()),
    })
    .unwrap();
    let addr = server.local_addr().unwrap().to_string();
    let discovery_port = server.discovery_addr().unwrap().port();
    let access = server.access();
    access.set_password("тайна");
    access.set_max_peers(2);
    tokio::spawn(server.run());

    let with = |password| Credentials { admin_token: None, password };
    // Без пароля и с неверным — отказ с понятной причиной.
    let err = connect_as(&addr, "Боб", with(None), Trust::Any).await.err().unwrap();
    assert!(format!("{err:#}").contains(vwf_proto::PASSWORD_REQUIRED_REASON), "{err:#}");
    let err = connect_as(&addr, "Боб", with(Some("не то")), Trust::Any).await.err().unwrap();
    assert!(format!("{err:#}").contains(vwf_proto::WRONG_PASSWORD_REASON), "{err:#}");

    // Хост входит без пароля, Алиса — с паролем.
    let _host = vwf_client::net::connect_with_token(&addr, "Хост", Some("секрет")).await.unwrap();
    let _alice = connect_as(&addr, "Алиса", with(Some("тайна")), Trust::Any).await.unwrap();

    // Двое уже в комнате — третьему места нет.
    let err = connect_as(&addr, "Боб", with(Some("тайна")), Trust::Any).await.err().unwrap();
    assert!(format!("{err:#}").contains(vwf_proto::ROOM_FULL_REASON), "{err:#}");

    // Сняли лимит и пароль — Боб входит без пароля; в поиске комната без замка.
    access.set_max_peers(0);
    access.set_password("");
    connect_as(&addr, "Боб", with(None), Trust::Any).await.expect("пароль снят");
    let found = vwf_client::discovery::discover_on(discovery_port, Duration::from_millis(400))
        .await
        .unwrap();
    assert!(found.iter().all(|s| !s.password), "{found:?}");
    access.set_password("снова");
    let found = vwf_client::discovery::discover_on(discovery_port, Duration::from_millis(400))
        .await
        .unwrap();
    assert!(found.iter().any(|s| s.password), "{found:?}");
}

#[tokio::test]
async fn probe_tells_running_server_from_stopped() {
    use vwf_client::net::{Availability, probe_address};
    let addr = start_server(false).await.to_string();
    assert_eq!(probe_address(&addr, T).await, Availability::Online);

    // Проверка не входит в комнату: следующий вошедший никого не видит.
    let alice = vwf_client::net::connect(&addr, "Алиса").await.unwrap();
    assert!(alice.peers.is_empty());

    // Порт, на котором никто не слушает.
    let free = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let wait = Duration::from_millis(500);
    assert_eq!(probe_address(&free.to_string(), wait).await, Availability::Offline);
}

/// Камера, которая отдаёт кадры 1280×720 так часто, как их берут (~30 к/с).
struct TestCamera(u32);

impl vwf_client::camera::CameraSource for TestCamera {
    fn frame(
        &mut self,
        _deadline: std::time::Instant,
    ) -> anyhow::Result<Option<vwf_client::camera::I420>> {
        std::thread::sleep(Duration::from_millis(33));
        self.0 += 1;
        let mut frame = vwf_client::camera::I420::new(1280, 720);
        let shift = self.0 as usize * 8;
        for (i, v) in frame.data[..1280 * 720].iter_mut().enumerate() {
            *v = (((i % 1280 + shift) / 32 % 2) * 180 + 40) as u8;
        }
        Ok(Some(frame))
    }
}

/// Слои камеры как в групповых звонках Telegram: каждый зритель получает
/// только тот слой, который попросил; при смене слоя переключение — на
/// опорном кадре нового; отправитель знает, сколько зрителей у каждого слоя.
#[tokio::test]
async fn camera_layers_reach_each_viewer() {
    use vwf_client::camera::CameraSharer;
    use vwf_proto::{ClientMsg, VideoKind};

    let addr = start_server(false).await.to_string();
    let mut alice = vwf_client::net::connect(&addr, "Алиса").await.unwrap();
    let mut bob = vwf_client::net::connect(&addr, "Боб").await.unwrap();
    let carol = vwf_client::net::connect(&addr, "Кэрол").await.unwrap();
    let alice_id = alice.me.id;

    alice.control().send(ClientMsg::SetCamera { layers: 3 });
    let video = alice.camera_sender();
    let camera = std::sync::Arc::new(
        CameraSharer::start_with(
            || Ok(Box::new(TestCamera(0))),
            3,
            move |frame| video.send(frame),
            |_| {},
            |e| panic!("камера остановилась: {e}"),
        )
        .unwrap(),
    );
    expect_event(&mut bob, |e| matches!(e, ServerMsg::PeerUpdated(p) if p.camera)).await;

    // Что сервер говорит Алисе — передаём камере, как это делает `Call`.
    let alice_control = alice.control();
    let (viewers_tx, mut viewers) = tokio::sync::mpsc::unbounded_channel();
    {
        let camera = camera.clone();
        tokio::spawn(async move {
            while let Ok(Some(event)) = alice.next_event().await {
                match event {
                    ServerMsg::CameraViewers { layers } => {
                        camera.set_viewers(layers);
                        let _ = viewers_tx.send(layers);
                    }
                    ServerMsg::CameraKeyframeNeeded { layer } => camera.request_keyframe(layer),
                    _ => {}
                }
            }
        });
    }
    let mut wait_viewers = async |want: [u32; 3]| {
        timeout(T, async {
            while viewers.recv().await != Some(want) {}
        })
        .await
        .unwrap_or_else(|_| panic!("не дождались зрителей по слоям {want:?}"));
    };

    // Боб смотрит маленькую плитку, Кэрол — на весь экран.
    bob.control().send(ClientMsg::WatchCamera { id: alice_id, layer: Some(0) });
    carol.control().send(ClientMsg::WatchCamera { id: alice_id, layer: Some(2) });
    wait_viewers([1, 0, 1]).await;

    let first_frames = async |session: &vwf_client::net::Session, n: usize| {
        let receiver = session.video_receiver();
        timeout(T, async {
            let mut headers = Vec::new();
            while headers.len() < n {
                let (from, header, _) = receiver.recv().await.unwrap();
                assert_eq!(from, alice_id);
                headers.push(header);
            }
            headers
        })
        .await
        .expect("кадры камеры не дошли")
    };
    let bob_frames = first_frames(&bob, 5).await;
    assert!(bob_frames.iter().all(|h| h.kind == VideoKind::Camera && h.layer == 0));
    assert!(bob_frames[0].keyframe, "показ начинается с опорного кадра");
    assert_eq!((bob_frames[0].width, bob_frames[0].height), (320, 180));
    let carol_frames = first_frames(&carol, 5).await;
    assert!(carol_frames.iter().all(|h| h.layer == 2));
    assert!(carol_frames[0].keyframe);
    assert_eq!((carol_frames[0].width, carol_frames[0].height), (1280, 720));

    // Боб развернул плитку: слой 1. Пока не пришёл опорный кадр слоя 1,
    // идёт слой 0; после — только слой 1.
    bob.control().send(ClientMsg::WatchCamera { id: alice_id, layer: Some(1) });
    let switched = first_frames(&bob, 20).await;
    let first = switched.iter().position(|h| h.layer == 1).expect("слой не переключился");
    assert!(switched[first].keyframe, "переключение на опорном кадре");
    assert!(switched[first..].iter().all(|h| h.layer == 1), "назад не переключается");
    wait_viewers([0, 1, 1]).await;

    // Кэрол перестала смотреть — слой 2 никому не нужен.
    carol.control().send(ClientMsg::WatchCamera { id: alice_id, layer: None });
    wait_viewers([0, 1, 0]).await;

    // Алиса выключила камеру — это видят все.
    drop(camera);
    alice_control.send(ClientMsg::SetCamera { layers: 0 });
    expect_event(
        &mut bob,
        |e| matches!(e, ServerMsg::PeerUpdated(p) if p.id == alice_id && !p.camera),
    )
    .await;
}

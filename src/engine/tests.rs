use super::*;

#[test]
fn stop_acknowledges_cancellation_and_missing_patterns_fail() {
    let engine = Engine::new().unwrap();
    let mut profile = Profile::default();
    profile.settings.client_identity = "alice".into();
    profile.settings.client_password = "secret".into();
    profile.settings.patterns.client_pattern_path = "/missing/bluesky-pattern.bin".into();
    engine.send(Command::Start(1, Box::new(profile)));
    loop {
        let event = engine.events.recv_timeout(Duration::from_secs(3)).unwrap();
        if event.phase == Phase::Failed {
            assert_eq!(event.generation, 1);
            break;
        }
    }
    engine.send(Command::Stop(2));
    let event = engine.events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(event.generation, 2);
    assert_eq!(event.phase, Phase::Idle);
}

#[test]
fn cancelling_handshake_releases_tcp_and_udp_listeners_before_acknowledging_stop() {
    let dir = tempfile::tempdir().unwrap();
    crate::profiles::prepare(dir.path()).unwrap();
    let engine = Engine::new().unwrap();
    // Keep the server bound without answering, so authentication is cancellable.
    let remote = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut profile = Profile::default();
    profile.settings.server_address = remote.local_addr().unwrap().to_string();
    profile.settings.client_identity = "alice".into();
    profile.settings.client_password = "secret".into();
    crate::profiles::resolve_patterns(&mut profile, dir.path());
    let scenarios = [None, Some(Default::default())]
        .into_iter()
        .flat_map(|multipath| {
            [Mode::Http, Mode::Tcp, Mode::Udp]
                .into_iter()
                .map(move |mode| (multipath.clone(), mode))
        });
    for (index, (multipath, mode)) in scenarios.enumerate() {
        let vacant = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = vacant.local_addr().unwrap();
        drop(vacant);
        profile.mode = mode;
        profile.settings.multipath = multipath;
        profile.settings.local_proxy_bind = address.to_string();
        let generation = index as u64 * 2 + 1;
        engine.send(Command::Start(generation, Box::new(profile.clone())));
        loop {
            let event = engine.events.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_ne!(event.phase, Phase::Failed, "{}", event.message);
            if event.message.starts_with("Authenticating") {
                break;
            }
        }
        if mode == Mode::Udp {
            assert!(std::net::UdpSocket::bind(address).is_err());
        } else {
            assert!(std::net::TcpListener::bind(address).is_err());
        }
        engine.send(Command::Stop(generation + 1));
        loop {
            let event = engine.events.recv_timeout(Duration::from_secs(5)).unwrap();
            if event.generation == generation + 1 {
                assert_eq!(event.phase, Phase::Idle);
                break;
            }
        }
        if mode == Mode::Udp {
            assert!(std::net::UdpSocket::bind(address).is_ok());
        } else {
            assert!(std::net::TcpListener::bind(address).is_ok());
        }
    }
}

#[tokio::test]
async fn transport_failure_reports_retry_and_rotation_aborts_the_old_driver() {
    let mut failed = tokio::spawn(async { Err(io::Error::other("test transport failure")) });
    let reason = wait_generation(&mut failed, std::future::pending(), None)
        .await
        .unwrap()
        .unwrap();
    assert!(reason.contains("test transport failure"));
    let mut driver = tokio::spawn(std::future::pending());
    let reason = wait_generation(&mut driver, std::future::pending(), Some(Duration::ZERO))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reason, "Scheduled session rotation");
    assert!(driver.is_finished());
}

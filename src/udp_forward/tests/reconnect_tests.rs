use super::*;
use bluesky_obfs_tunnel::{client::reconnect, config::ReconnectConfig};

#[tokio::test]
async fn listener_and_local_sender_survive_rotation_with_fresh_udp_associations() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let target = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let destination = target.local_addr().unwrap().to_string();
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let peer = local_peer(socket.local_addr().unwrap()).await;
        let (generations, mut connected) = mpsc::unbounded_channel();
        let supervisor = tokio::spawn(async move {
            let mut servers = JoinSet::new();
            let config = ReconnectConfig {
                session_min_secs: 1,
                session_max_secs: 1,
                ..ReconnectConfig::default()
            };
            reconnect::supervise(
                &config,
                Duration::from_secs(2),
                || {
                    let (client, server) = tokio::io::duplex(128 * 1024);
                    servers.spawn(
                        ClientSession::new(
                            Framed::new(server, LengthDelimitedCodec::new()),
                            Arc::new(ForwardTo::Multiplexed(ConnectPolicy::default())),
                            StreamLimits::default(),
                        )
                        .run(),
                    );
                    let session = ForwardClient::from_framed(
                        Framed::new(client, LengthDelimitedCodec::new()),
                        StreamLimits::default(),
                    );
                    generations.send(()).unwrap();
                    async { Ok(session) }
                },
                |client| {
                    serve(
                        &socket,
                        client,
                        &destination,
                        Duration::from_secs(2),
                        256,
                        64,
                    )
                },
            )
            .await
        });
        let mut buffer = [0; 32];
        for _ in 0..2 {
            connected.recv().await.unwrap();
            peer.send(b"request").await.unwrap();
            let (size, source) = target.recv_from(&mut buffer).await.unwrap();
            assert_eq!(&buffer[..size], b"request");
            target.send_to(b"reply", source).await.unwrap();
            let size = peer.recv(&mut buffer).await.unwrap();
            assert_eq!(&buffer[..size], b"reply");
        }
        supervisor.abort();
        assert!(supervisor.await.unwrap_err().is_cancelled());
    })
    .await
    .unwrap();
}

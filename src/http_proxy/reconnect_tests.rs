use super::*;
use bluesky_obfs_tunnel::{
    StreamLimits,
    client::reconnect,
    config::ReconnectConfig,
    server::{client_session::ClientSession, connect_policy::ConnectPolicy, fwd_server::ForwardTo},
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

async fn connect_proxy(proxy: std::net::SocketAddr, target: std::net::SocketAddr) -> TcpStream {
    let mut socket = TcpStream::connect(proxy).await.unwrap();
    socket
        .write_all(format!("CONNECT {target} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(socket.read_u8().await.unwrap());
    }
    assert_eq!(header, b"HTTP/1.1 200 Connection Established\r\n\r\n");
    socket.write_all(b"echo").await.unwrap();
    let mut reply = [0; 4];
    socket.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"echo");
    socket
}

#[tokio::test]
async fn proxy_listener_survives_rotation_and_new_requests_use_a_fresh_tunnel() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = origin.local_addr().unwrap();
        let origin = tokio::spawn(async move {
            let mut relays = JoinSet::new();
            for _ in 0..2 {
                let (mut socket, _) = origin.accept().await.unwrap();
                relays.spawn(async move {
                    let mut buffer = [0; 32];
                    loop {
                        let size = socket.read(&mut buffer).await.unwrap();
                        if size == 0 {
                            break;
                        }
                        socket.write_all(&buffer[..size]).await.unwrap();
                    }
                });
            }
            while let Some(result) = relays.join_next().await {
                result.unwrap();
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
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
                    let (client, server) = tokio::io::duplex(64 * 1024);
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
                |client| serve(&listener, client, Duration::from_secs(2), 256),
            )
            .await
        });
        connected.recv().await.unwrap();
        let mut first = connect_proxy(proxy_address, target).await;
        connected.recv().await.unwrap();
        assert_eq!(first.read(&mut [0; 1]).await.unwrap(), 0);
        let mut second = connect_proxy(proxy_address, target).await;
        supervisor.abort();
        assert!(supervisor.await.unwrap_err().is_cancelled());
        assert_eq!(second.read(&mut [0; 1]).await.unwrap(), 0);
        origin.await.unwrap();
    })
    .await
    .unwrap();
}

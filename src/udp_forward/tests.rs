use super::*;
use bluesky_obfs_tunnel::{
    StreamLimits,
    server::{client_session::ClientSession, connect_policy::ConnectPolicy, fwd_server::ForwardTo},
};
use std::future::Future;
use tokio::task::JoinHandle;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

async fn deadline<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("UDP forwarding test timed out")
}

fn session() -> (
    ForwardClient,
    JoinHandle<io::Result<()>>,
    JoinHandle<io::Result<()>>,
) {
    let (client, server) = tokio::io::duplex(128 * 1024);
    let server = tokio::spawn(
        ClientSession::new(
            Framed::new(server, LengthDelimitedCodec::new()),
            Arc::new(ForwardTo::Multiplexed(ConnectPolicy::default())),
            StreamLimits::default(),
        )
        .run(),
    );
    let (client, driver) = ForwardClient::from_framed(
        Framed::new(client, LengthDelimitedCodec::new()),
        StreamLimits::default(),
    );
    (client, driver, server)
}

struct Forwarder {
    address: SocketAddr,
    tasks: Vec<JoinHandle<io::Result<()>>>,
}

impl Forwarder {
    async fn new(destination: SocketAddr) -> Self {
        let (client, driver, server) = session();
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let address = socket.local_addr().unwrap();
        let listener = tokio::spawn(async move {
            serve(
                &socket,
                client,
                &destination.to_string(),
                Duration::from_secs(1),
                256,
                64,
            )
            .await
        });
        Self {
            address,
            tasks: vec![listener, driver, server],
        }
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn local_peer(address: SocketAddr) -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect(address).await.unwrap();
    socket
}

#[tokio::test]
async fn continuous_datagrams_preserve_boundaries_and_route_replies_to_each_sender() {
    deadline(async {
        let target = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let forwarder = Forwarder::new(target.local_addr().unwrap()).await;
        let peers = [
            local_peer(forwarder.address).await,
            local_peer(forwarder.address).await,
        ];
        let mut sources = [None, None];
        let mut buffer = vec![0; MAX_TUNNEL_CHUNK + 1];
        for packets in [
            [b"first".to_vec(), b"second".to_vec()],
            [vec![], vec![255; MAX_TUNNEL_CHUNK]],
            [b"last one".to_vec(), b"last two".to_vec()],
        ] {
            for (peer, packet) in peers.iter().zip(&packets) {
                peer.send(packet).await.unwrap();
            }
            let mut replies = Vec::new();
            for _ in 0..2 {
                let (size, source) = target.recv_from(&mut buffer).await.unwrap();
                let index = packets.iter().position(|p| p == &buffer[..size]).unwrap();
                if let Some(previous) = sources[index] {
                    assert_eq!(source, previous, "sender must reuse its UDP association");
                }
                sources[index] = Some(source);
                replies.push((buffer[..size].to_vec(), source));
            }
            assert_ne!(sources[0], sources[1]);
            // Reverse reply order to catch cross-delivery between local senders.
            for (packet, source) in replies.into_iter().rev() {
                target.send_to(&packet, source).await.unwrap();
            }
            for (peer, packet) in peers.iter().zip(&packets) {
                let size = peer.recv(&mut buffer).await.unwrap();
                assert_eq!(&buffer[..size], packet);
            }
        }
        // One request can receive further replies without sending another packet.
        target.send_to(b"push", sources[0].unwrap()).await.unwrap();
        let size = peers[0].recv(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..size], b"push");
    })
    .await;
}

#[tokio::test]
async fn oversized_local_datagrams_are_dropped_without_truncation_or_stopping_listener() {
    deadline(async {
        let target = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let forwarder = Forwarder::new(target.local_addr().unwrap()).await;
        let peer = local_peer(forwarder.address).await;
        peer.send(&vec![42; MAX_TUNNEL_CHUNK + 1]).await.unwrap();
        peer.send(b"valid").await.unwrap();
        let mut buffer = vec![0; MAX_TUNNEL_CHUNK + 1];
        let (size, source) = target.recv_from(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..size], b"valid");
        target.send_to(b"reply", source).await.unwrap();
        let size = peer.recv(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..size], b"reply");
    })
    .await;
}

#[tokio::test]
async fn idle_mapping_expires_and_releases_its_queue() {
    tokio::time::timeout(IDLE_TIMEOUT * 2, async {
        let target = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (client, driver, server) = session();
        let connection = client
            .open_udp(target.local_addr().unwrap().to_string())
            .await
            .unwrap();
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let peer = local_peer(socket.local_addr().unwrap()).await;
        let (sender, incoming) = mpsc::channel(1);
        let task = tokio::spawn(relay(
            socket,
            peer.local_addr().unwrap(),
            connection,
            incoming,
        ));
        tokio::time::pause();
        tokio::task::yield_now().await;
        tokio::time::advance(IDLE_TIMEOUT + Duration::from_secs(1)).await;
        task.await.unwrap().unwrap();
        assert!(sender.is_closed());
        tokio::time::resume();
        client.shutdown().await.unwrap();
        driver.await.unwrap().unwrap();
        server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

mod reconnect_tests;

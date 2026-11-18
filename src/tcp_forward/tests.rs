use super::*;
use bluesky_obfs_tunnel::{
    StreamLimits,
    server::{client_session::ClientSession, connect_policy::ConnectPolicy, fwd_server::ForwardTo},
};
use std::{future::Future, net::SocketAddr, sync::Arc};
use tokio::task::JoinHandle;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

async fn deadline<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("TCP forwarding test timed out")
}

struct Forwarder {
    address: SocketAddr,
    tasks: Vec<JoinHandle<io::Result<()>>>,
}

impl Forwarder {
    async fn new(destination: SocketAddr) -> Self {
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let listener = tokio::spawn(async move {
            serve(
                &listener,
                client,
                &destination.to_string(),
                Duration::from_secs(1),
                256,
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

#[tokio::test]
async fn concurrent_binary_streams_half_close_without_blocking_on_idle_client() {
    deadline(async {
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let forwarder = Forwarder::new(target.local_addr().unwrap()).await;
        let _idle = TcpStream::connect(forwarder.address).await.unwrap();
        let origin = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            for _ in 0..3 {
                let (mut socket, _) = target.accept().await.unwrap();
                tasks.spawn(async move {
                    let mut data = Vec::new();
                    socket.read_to_end(&mut data).await.unwrap();
                    socket.write_all(&data).await.unwrap();
                    socket.write_all(b"after EOF").await.unwrap();
                });
            }
            while let Some(result) = tasks.join_next().await {
                result.unwrap();
            }
        });
        let address = forwarder.address;
        let exchange = |byte| async move {
            let mut socket = TcpStream::connect(address).await.unwrap();
            let payload = vec![byte; MAX_TUNNEL_CHUNK * 5 + 19];
            socket.write_all(&payload).await.unwrap();
            socket.shutdown().await.unwrap();
            let mut response = Vec::new();
            socket.read_to_end(&mut response).await.unwrap();
            assert_eq!(&response[..payload.len()], &payload);
            assert_eq!(&response[payload.len()..], b"after EOF");
        };
        tokio::join!(exchange(0), exchange(255));
        origin.abort(); // The third connection is deliberately still idle.
    })
    .await;
}

#[tokio::test]
async fn remote_half_close_preserves_local_writes() {
    deadline(async {
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let forwarder = Forwarder::new(target.local_addr().unwrap()).await;
        let origin = tokio::spawn(async move {
            let (mut socket, _) = target.accept().await.unwrap();
            socket.write_all(b"greeting").await.unwrap();
            socket.shutdown().await.unwrap();
            let mut received = Vec::new();
            socket.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, b"after remote EOF");
        });
        let mut socket = TcpStream::connect(forwarder.address).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"greeting");
        socket.write_all(b"after remote EOF").await.unwrap();
        socket.shutdown().await.unwrap();
        origin.await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn failed_destination_does_not_stop_listener_and_cancellation_closes_streams() {
    deadline(async {
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination = target.local_addr().unwrap();
        drop(target);
        let mut forwarder = Forwarder::new(destination).await;
        let mut failed = TcpStream::connect(forwarder.address).await.unwrap();
        assert_eq!(failed.read(&mut [0]).await.unwrap(), 0);
        let target = TcpListener::bind(destination).await.unwrap();
        let mut socket = TcpStream::connect(forwarder.address).await.unwrap();
        let (mut origin, _) = target.accept().await.unwrap();
        origin.write_all(b"ready").await.unwrap();
        let mut ready = [0; 5];
        socket.read_exact(&mut ready).await.unwrap();
        assert_eq!(&ready, b"ready");
        forwarder.tasks[0].abort();
        let _ = (&mut forwarder.tasks[0]).await;
        assert_eq!(socket.read(&mut [0]).await.unwrap(), 0);
        assert_eq!(origin.read(&mut [0]).await.unwrap(), 0);
    })
    .await;
}

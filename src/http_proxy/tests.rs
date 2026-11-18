use super::request::MAX_HEADERS;
use super::*;
use bluesky_obfs_tunnel::server::{
    StreamLimits, client_session::ClientSession, connect_policy::ConnectPolicy,
    fwd_server::ForwardTo,
};
use std::{future::Future, net::SocketAddr, sync::Arc};
use tokio::task::JoinHandle;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

async fn deadline<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("proxy test timed out")
}

struct Proxy {
    address: SocketAddr,
    client: ForwardClient,
    driver: JoinHandle<io::Result<()>>,
    server: JoinHandle<io::Result<()>>,
    listener: JoinHandle<io::Result<()>>,
}

impl Proxy {
    async fn new(setup_timeout: Duration) -> Self {
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
        let proxy_client = client.clone();
        let listener =
            tokio::spawn(async move { serve(&listener, proxy_client, setup_timeout, 256).await });
        Self {
            address,
            client,
            driver,
            server,
            listener,
        }
    }

    async fn stop(mut self) {
        self.listener.abort();
        let _ = (&mut self.listener).await;
        self.client.shutdown().await.unwrap();
        (&mut self.driver).await.unwrap().unwrap();
        (&mut self.server).await.unwrap().unwrap();
    }

    async fn exchange(&self, request: &[u8]) -> Vec<u8> {
        let mut socket = TcpStream::connect(self.address).await.unwrap();
        socket.write_all(request).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        response
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.listener.abort();
        self.driver.abort();
        self.server.abort();
    }
}

async fn read_headers(socket: &mut TcpStream) -> Vec<u8> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(socket.read_u8().await.unwrap());
        assert!(header.len() <= MAX_HEADERS);
    }
    header
}

#[tokio::test]
async fn http_upload_and_connect_share_tunnel_while_another_client_is_idle() {
    deadline(async {
        let proxy = Proxy::new(Duration::from_secs(2)).await;
        let _idle = TcpStream::connect(proxy.address).await.unwrap();
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_address = http.local_addr().unwrap();
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_address = echo.local_addr().unwrap();
        let upload = vec![42; MAX_TUNNEL_CHUNK * 5 + 17];
        let expected = upload.clone();
        let http_origin = tokio::spawn(async move {
            let (mut socket, _) = http.accept().await.unwrap();
            let header = String::from_utf8(read_headers(&mut socket).await).unwrap();
            assert!(header.starts_with("POST /upload?q=1 HTTP/1.1\r\n"));
            assert!(header.contains(&format!("Host: {http_address}\r\n")));
            assert!(header.contains("Connection: close\r\n"));
            assert!(!header.contains("Proxy-Authorization"));
            socket.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await.unwrap();
            let mut body = vec![0; expected.len()];
            socket.read_exact(&mut body).await.unwrap();
            assert_eq!(body, expected);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK").await.unwrap();
        });
        let echo_origin = tokio::spawn(async move {
            let (mut socket, _) = echo.accept().await.unwrap();
            let mut received = Vec::new();
            socket.read_to_end(&mut received).await.unwrap();
            socket.write_all(&received).await.unwrap();
        });
        let http_client = async {
            let mut socket = TcpStream::connect(proxy.address).await.unwrap();
            let header = format!("POST http://{http_address}/upload?q=1 HTTP/1.1\r\nHost: ignored\r\nContent-Length: {}\r\nExpect: 100-continue\r\nProxy-Authorization: secret\r\n\r\n", upload.len());
            // Fragmented headers must not delay other proxy connections.
            socket.write_all(&header.as_bytes()[..12]).await.unwrap();
            tokio::task::yield_now().await;
            socket.write_all(&header.as_bytes()[12..]).await.unwrap();
            assert_eq!(read_headers(&mut socket).await, b"HTTP/1.1 100 Continue\r\n\r\n");
            socket.write_all(&upload).await.unwrap();
            // Leave the browser write side open: the origin's close must finish HTTP.
            let mut reply = Vec::new();
            socket.read_to_end(&mut reply).await.unwrap();
            assert!(reply.ends_with(b"\r\n\r\nOK"));
        };
        let connect_client = async {
            let mut socket = TcpStream::connect(proxy.address).await.unwrap();
            let mut request = format!("CONNECT {echo_address} HTTP/1.1\r\nHost: {echo_address}\r\n\r\n").into_bytes();
            request.extend_from_slice(b"early TLS bytes");
            socket.write_all(&request).await.unwrap();
            assert_eq!(read_headers(&mut socket).await, b"HTTP/1.1 200 Connection Established\r\n\r\n");
            let payload = vec![7; MAX_TUNNEL_CHUNK * 4 + 9];
            socket.write_all(&payload).await.unwrap();
            socket.shutdown().await.unwrap();
            let mut reply = Vec::new();
            socket.read_to_end(&mut reply).await.unwrap();
            assert_eq!(&reply[..15], b"early TLS bytes");
            assert_eq!(&reply[15..], payload);
        };
        tokio::join!(http_client, connect_client);
        http_origin.await.unwrap();
        echo_origin.await.unwrap();
        proxy.stop().await;
    }).await;
}

#[tokio::test]
async fn streams_chunked_body_with_final_zero_chunk_and_trailers() {
    deadline(async {
        let proxy = Proxy::new(Duration::from_secs(2)).await;
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = origin.local_addr().unwrap();
        let body = b"4;ext=yes\r\ntest\r\n0\r\nX-Trailer: done\r\n\r\n";
        let origin = tokio::spawn(async move {
            let (mut socket, _) = origin.accept().await.unwrap();
            let header = read_headers(&mut socket).await;
            assert!(header.starts_with(b"POST / HTTP/1.1\r\n"));
            let mut received = vec![0; body.len()];
            socket.read_exact(&mut received).await.unwrap();
            assert_eq!(received, body);
            socket.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await.unwrap();
        });
        let mut request = format!("POST http://{address}/ HTTP/1.1\r\nTransfer-Encoding: chunked\r\nTrailer: X-Trailer\r\n\r\n").into_bytes();
        request.extend_from_slice(body);
        assert!(proxy.exchange(&request).await.starts_with(b"HTTP/1.1 204"));
        origin.await.unwrap();
        proxy.stop().await;
    }).await;
}

#[tokio::test]
async fn proxy_reports_failed_connect_invalid_headers_and_slow_headers() {
    deadline(async {
        let proxy = Proxy::new(Duration::from_millis(100)).await;
        let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = unused.local_addr().unwrap();
        drop(unused);
        let response = proxy
            .exchange(format!("CONNECT {address} HTTP/1.1\r\n\r\n").as_bytes())
            .await;
        assert!(response.starts_with(b"HTTP/1.1 502 Bad Gateway\r\n"));
        assert!(!response.windows(3).any(|window| window == b"200"));
        assert!(
            proxy
                .exchange(b"GET / HTTP/1.1\r\n\r\n")
                .await
                .starts_with(b"HTTP/1.1 400")
        );
        let oversized = format!("GET / HTTP/1.1\r\nX-Long: {}", "x".repeat(MAX_HEADERS));
        assert!(
            proxy
                .exchange(oversized.as_bytes())
                .await
                .starts_with(b"HTTP/1.1 431")
        );
        assert!(proxy.exchange(b"GET /").await.starts_with(b"HTTP/1.1 408"));
        proxy.stop().await;
    })
    .await;
}

//! Local HTTP/HTTPS proxy listener backed by multiplexed tunnel connections.
mod request;

use bluesky_obfs_tunnel::{
    client::{ForwardClient, ForwardConnection},
    protocol::MAX_TUNNEL_CHUNK,
};
use request::read_request;
use std::{io, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
use tokio_util::bytes::Bytes;

pub(super) async fn serve(
    listener: &TcpListener,
    client: ForwardClient,
    setup_timeout: Duration,
    max_connections: usize,
) -> io::Result<()> {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept(), if tasks.len() < max_connections.max(1) => {
                let (socket, peer) = accepted?;
                let client = client.clone();
                tasks.spawn(async move {
                    if let Err(error) = handle(socket, client, setup_timeout).await {
                        eprintln!("HTTP proxy {peer}: {error}");
                    }
                });
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result {
                    eprintln!("HTTP proxy task: {error}");
                }
            }
        }
    }
    // Dropping the JoinSet cancels connections when the listener is stopped.
}

async fn reject(socket: &mut TcpStream, status: &str, message: &str) -> io::Result<()> {
    eprintln!("HTTP proxy: {status}: {message}");
    let body = format!("{message}\n");
    let response = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
    );
    socket.write_all(response.as_bytes()).await?;
    socket.shutdown().await
}

async fn handle(
    mut socket: TcpStream,
    client: ForwardClient,
    setup_timeout: Duration,
) -> io::Result<()> {
    socket.set_nodelay(true)?;
    let request = match tokio::time::timeout(setup_timeout, read_request(&mut socket)).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => return reject(&mut socket, error.status, &error.message).await,
        Err(_) => {
            return reject(
                &mut socket,
                "408 Request Timeout",
                "proxy headers timed out",
            )
            .await;
        }
    };
    let connection =
        match tokio::time::timeout(setup_timeout, client.open_tcp(request.destination.clone()))
            .await
        {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                return reject(
                    &mut socket,
                    "502 Bad Gateway",
                    &format!("cannot open {}: {error}", request.destination),
                )
                .await;
            }
            Err(_) => {
                return reject(
                    &mut socket,
                    "504 Gateway Timeout",
                    &format!(
                        "opening {} timed out; the example server must run with --multiplexed",
                        request.destination
                    ),
                )
                .await;
            }
        };
    if request.connect {
        socket
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    }
    relay(socket, connection, request.initial, request.connect).await
}

async fn relay(
    socket: TcpStream,
    connection: ForwardConnection,
    initial: Bytes,
    connect: bool,
) -> io::Result<()> {
    let (mut read, mut write) = socket.into_split();
    let (sender, mut receiver) = connection.split();
    let send = async {
        sender.send(initial).await?;
        let mut buffer = [0; MAX_TUNNEL_CHUNK];
        loop {
            let size = read.read(&mut buffer).await?;
            if size == 0 {
                return sender.shutdown_write().await;
            }
            sender.send(Bytes::copy_from_slice(&buffer[..size])).await?;
        }
    };
    let receive = async {
        while let Some(data) = receiver.recv().await? {
            write.write_all(&data).await?;
        }
        write.shutdown().await
    };
    tokio::pin!(send, receive);
    if connect {
        tokio::try_join!(send, receive)?;
        Ok(())
    } else {
        tokio::select! {
            result = &mut receive => result,
            result = &mut send => { result?; receive.await }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reconnect_tests;

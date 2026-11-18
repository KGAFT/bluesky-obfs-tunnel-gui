//! A local TCP listener forwarding opaque bytes over a multiplexed tunnel.
use bluesky_obfs_tunnel::{
    client::{ForwardClient, ForwardConnection},
    protocol::MAX_TUNNEL_CHUNK,
};
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
    destination: &str,
    setup_timeout: Duration,
    max_connections: usize,
) -> io::Result<()> {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept(), if tasks.len() < max_connections.max(1) => {
                let (socket, peer) = accepted?;
                let client = client.clone();
                let destination = destination.to_owned();
                tasks.spawn(async move {
                    let result = async {
                        socket.set_nodelay(true)?;
                        let connection = tokio::time::timeout(
                            setup_timeout, client.open_tcp(destination.clone()),
                        ).await??;
                        relay(socket, connection).await
                    }.await;
                    if let Err(error) = result {
                        eprintln!("TCP forward {peer} to {destination}: {error}");
                    }
                });
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result {
                    eprintln!("TCP forwarding task: {error}");
                }
            }
        }
    }
    // Dropping this future aborts all streams during shutdown or rotation.
}

async fn relay(socket: TcpStream, connection: ForwardConnection) -> io::Result<()> {
    let (mut read, mut write) = socket.into_split();
    let (sender, mut receiver) = connection.split();
    let send = async {
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
    tokio::try_join!(send, receive)?;
    Ok(())
}

#[cfg(test)]
mod tests;

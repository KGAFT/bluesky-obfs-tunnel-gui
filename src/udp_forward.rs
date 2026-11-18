//! A local UDP listener with an independent tunnel connection per sender.
use bluesky_obfs_tunnel::{
    client::{ForwardClient, ForwardConnection},
    protocol::MAX_TUNNEL_CHUNK,
};
use std::{collections::HashMap, io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::UdpSocket,
    sync::mpsc,
    task::{Id, JoinSet},
};
use tokio_util::bytes::Bytes;

const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) async fn serve(
    socket: &Arc<UdpSocket>,
    client: ForwardClient,
    destination: &str,
    setup_timeout: Duration,
    max_peers: usize,
    queue_depth: usize,
) -> io::Result<()> {
    let mut peers: HashMap<SocketAddr, (Id, mpsc::Sender<Bytes>)> = HashMap::new();
    let mut tasks = JoinSet::new();
    // Read the entire datagram so an oversized packet is dropped, never truncated.
    let mut buffer = vec![0; u16::MAX as usize + 1];
    loop {
        tokio::select! {
            biased;
            Some(result) = tasks.join_next_with_id(), if !tasks.is_empty() => {
                let id = match result {
                    Ok((id, (peer, result))) => {
                        if let Err(error) = result {
                            eprintln!("UDP forward {peer} to {destination}: {error}");
                        }
                        id
                    }
                    Err(error) => {
                        eprintln!("UDP forwarding task: {error}");
                        error.id()
                    }
                };
                // A new mapping may already have replaced this completed task.
                peers.retain(|_, (task, _)| *task != id);
            }
            received = socket.recv_from(&mut buffer) => {
                let (size, peer) = received?;
                if size > MAX_TUNNEL_CHUNK {
                    eprintln!("UDP forward {peer}: dropping datagram larger than 8192 bytes");
                    continue;
                }
                let mut data = Bytes::copy_from_slice(&buffer[..size]);
                if let Some((_, sender)) = peers.get(&peer) {
                    match sender.try_send(data) {
                        Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => continue,
                        Err(mpsc::error::TrySendError::Closed(packet)) => data = packet,
                    }
                }
                peers.remove(&peer);
                if peers.len() >= max_peers.max(1) {
                    continue;
                }
                let (sender, incoming) = mpsc::channel(queue_depth.max(1));
                sender.try_send(data).expect("new UDP queue has capacity");
                let socket = Arc::clone(socket);
                let client = client.clone();
                let destination = destination.to_owned();
                let task = tasks.spawn(async move {
                    let result = async {
                        let connection = tokio::time::timeout(
                            setup_timeout, client.open_udp(destination),
                        ).await??;
                        relay(socket, peer, connection, incoming).await
                    }.await;
                    (peer, result)
                });
                peers.insert(peer, (task.id(), sender));
            }
        }
    }
    // Dropping the generation aborts every mapping, including pending opens.
}

async fn relay(
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    connection: ForwardConnection,
    mut incoming: mpsc::Receiver<Bytes>,
) -> io::Result<()> {
    let (sender, mut receiver) = connection.split();
    loop {
        let active = tokio::time::timeout(IDLE_TIMEOUT, async {
            tokio::select! {
                data = incoming.recv() => {
                    let Some(data) = data else { return Ok(false); };
                    sender.send(data).await?;
                }
                data = receiver.recv() => {
                    let Some(data) = data? else { return Ok(false); };
                    if socket.send_to(&data, peer).await? != data.len() {
                        return Err(io::Error::from(io::ErrorKind::WriteZero));
                    }
                }
            }
            Ok(true)
        })
        .await
        .unwrap_or(Ok(false))?;
        if !active {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests;

use crate::{
    http_proxy,
    profiles::{Mode, Profile},
    tcp_forward, udp_forward,
};
use bluesky_obfs_tunnel::client::ForwardClient;
use std::{
    future::Future,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::mpsc as async_mpsc,
    task::JoinHandle,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Connecting,
    Connected,
    Retrying,
    Failed,
}

#[derive(Clone)]
pub struct Event {
    pub generation: u64,
    pub phase: Phase,
    pub message: String,
}
pub enum Command {
    Start(u64, Box<Profile>),
    Stop(u64),
}

#[derive(Clone)]
pub struct Engine {
    commands: async_mpsc::UnboundedSender<Command>,
    pub events: EventReceiver,
    snapshot: Arc<Mutex<Event>>,
    generation: Arc<AtomicU64>,
    _runtime: Arc<tokio::runtime::Runtime>,
}

#[derive(Clone)]
pub struct EventReceiver(Arc<Mutex<mpsc::Receiver<Event>>>);
impl EventReceiver {
    pub fn try_recv(&self) -> Result<Event, mpsc::TryRecvError> {
        self.0.lock().unwrap().try_recv()
    }
    #[cfg(test)]
    fn recv_timeout(&self, timeout: Duration) -> Result<Event, mpsc::RecvTimeoutError> {
        self.0.lock().unwrap().recv_timeout(timeout)
    }
}

#[derive(Clone)]
struct EventSink {
    sender: mpsc::Sender<Event>,
    snapshot: Arc<Mutex<Event>>,
}

struct AbortOnDrop(JoinHandle<io::Result<()>>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Engine {
    pub fn new() -> io::Result<Self> {
        // The runtime belongs to the process on Android, so Activity teardown does
        // not drop a live tunnel while its foreground service is still running.
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
            let mut engine = ENGINE.lock().unwrap();
            if engine.is_none() {
                *engine = Some(Self::create()?);
            }
            Ok(engine.as_ref().unwrap().clone())
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Self::create()
    }

    fn create() -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (commands, mut incoming) = async_mpsc::unbounded_channel();
        let (sender, receiver) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(Event {
            generation: 0,
            phase: Phase::Idle,
            message: "Choose your server, then connect to start a local tunnel.".into(),
        }));
        let events = EventSink {
            sender,
            snapshot: snapshot.clone(),
        };
        runtime.spawn(async move {
            let mut task: Option<JoinHandle<()>> = None;
            while let Some(command) = incoming.recv().await {
                // Await cancellation before reusing listeners and the handshake setup port.
                if let Some(previous) = task.take() {
                    previous.abort();
                    let _ = previous.await;
                }
                match command {
                    Command::Stop(generation) => emit(
                        &events,
                        generation,
                        Phase::Idle,
                        "Tunnel stopped. Local listener released.",
                    ),
                    Command::Start(generation, profile) => {
                        let events = events.clone();
                        task = Some(tokio::spawn(async move {
                            if let Err(error) = serve(*profile, generation, &events).await {
                                emit(&events, generation, Phase::Failed, &error.to_string());
                            }
                        }));
                    }
                }
            }
            if let Some(task) = task {
                task.abort();
                let _ = task.await;
            }
        });
        Ok(Self {
            commands,
            events: EventReceiver(Arc::new(Mutex::new(receiver))),
            snapshot,
            generation: Arc::new(AtomicU64::new(0)),
            _runtime: Arc::new(runtime),
        })
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn snapshot(&self) -> Event {
        self.snapshot.lock().unwrap().clone()
    }
}

fn emit(events: &EventSink, generation: u64, phase: Phase, message: &str) {
    let event = Event {
        generation,
        phase,
        message: message.into(),
    };
    *events.snapshot.lock().unwrap() = event.clone();
    // Cleanup must happen on the runtime even when the mobile UI is suspended.
    if matches!(phase, Phase::Idle | Phase::Failed) {
        crate::platform::stop_connection();
    }
    let _ = events.sender.send(event);
}

enum Listener {
    Tcp(TcpListener),
    Udp(Arc<UdpSocket>),
}

async fn serve(profile: Profile, generation: u64, events: &EventSink) -> io::Result<()> {
    profile
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let settings = &profile.settings;
    emit(
        events,
        generation,
        Phase::Connecting,
        "Loading patterns and opening the local listener…",
    );
    let patterns = settings.patterns.load().await?;
    let cfg = settings.codec_cfg(patterns.client, settings.credentials());
    let listener = if profile.mode == Mode::Udp {
        Listener::Udp(Arc::new(UdpSocket::bind(&settings.local_proxy_bind).await?))
    } else {
        Listener::Tcp(TcpListener::bind(&settings.local_proxy_bind).await?)
    };
    loop {
        emit(
            events,
            generation,
            Phase::Connecting,
            &format!("Authenticating with {}…", settings.server_address),
        );
        let connected = tokio::time::timeout(settings.proxy_setup_timeout(), async {
            let cfg = cfg.clone();
            let limits = settings.limits();
            if settings.multipath.is_some() {
                ForwardClient::connect_multipath(cfg, settings.server_address.clone(), limits).await
            } else {
                ForwardClient::connect(cfg, &settings.server_address, limits).await
            }
        })
        .await;
        let (client, driver) = match connected {
            Ok(Ok(session)) => session,
            failed => {
                let error = match failed {
                    Ok(Err(error)) => error.to_string(),
                    Err(_) => "Connection setup timed out".into(),
                    _ => unreachable!(),
                };
                emit(
                    events,
                    generation,
                    Phase::Retrying,
                    &format!(
                        "{error}. Retrying in {}s.",
                        settings.reconnect.retry_delay_secs
                    ),
                );
                tokio::time::sleep(settings.reconnect.retry_delay()).await;
                continue;
            }
        };
        let mut driver = AbortOnDrop(driver);
        emit(
            events,
            generation,
            Phase::Connected,
            &format!("Authenticated. Listening on {}.", settings.local_proxy_bind),
        );
        let lifetime = settings
            .reconnect
            .next_lifetime()
            .map_err(|errors| io::Error::other(errors.join("; ")))?;
        let relay = async {
            let timeout = settings.proxy_setup_timeout();
            let capacity = settings.proxy_setup_queue_depth as usize;
            match (&listener, profile.mode) {
                (Listener::Tcp(listener), Mode::Http) => {
                    http_proxy::serve(listener, client, timeout, capacity).await
                }
                (Listener::Tcp(listener), Mode::Tcp) => {
                    tcp_forward::serve(listener, client, &profile.destination, timeout, capacity)
                        .await
                }
                (Listener::Udp(socket), Mode::Udp) => {
                    udp_forward::serve(
                        socket,
                        client,
                        &profile.destination,
                        timeout,
                        capacity,
                        settings.limits().max_pending_frames,
                    )
                    .await
                }
                _ => unreachable!(),
            }
        };
        let Some(reason) = wait_generation(&mut driver.0, relay, lifetime).await? else {
            return Ok(());
        };
        emit(
            events,
            generation,
            Phase::Retrying,
            &format!("{reason}. Reconnecting…"),
        );
        tokio::time::sleep(settings.reconnect.retry_delay()).await;
    }
}

async fn wait_generation(
    driver: &mut JoinHandle<io::Result<()>>,
    relay: impl Future<Output = io::Result<()>>,
    lifetime: Option<Duration>,
) -> io::Result<Option<String>> {
    let rotation = async {
        match lifetime {
            Some(duration) => tokio::time::sleep(duration).await,
            None => std::future::pending::<()>().await,
        }
    };
    let reason = tokio::select! {
        result = relay => { result?; return Ok(None); },
        _ = rotation => "Scheduled session rotation".to_string(),
        result = &mut *driver => {
            // A completed JoinHandle must never be polled a second time.
            return Ok(Some(match result {
                Ok(Ok(())) => "Tunnel closed".into(),
                Ok(Err(error)) => format!("Tunnel lost: {error}"),
                Err(error) => format!("Tunnel driver stopped: {error}"),
            }));
        },
    };
    driver.abort();
    let _ = driver.await;
    Ok(Some(reason))
}

#[cfg(test)]
mod tests;

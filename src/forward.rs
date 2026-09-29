use crate::{config::Config, web::Machine, wol};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;
use tokio::io::{copy_bidirectional, AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

fn turn_off_url(remote_ip: &str, turn_off_port: u16) -> String {
    format!("http://{}:{}/machines/turn-off", remote_ip, turn_off_port)
}

/// Tracks how much data has actually flowed through a single proxied
/// connection and reports it to the limiter only once it has carried real
/// traffic.
///
/// Accepting a connection is not by itself a sign that anyone is using the
/// machine. Uptime monitors, dashboards and port checks connect on a fixed
/// interval and transfer little or nothing; counting those as requests pins
/// the idle timer at zero and the machine never suspends.
struct ConnectionActivity {
    limiter: TurnOffLimiter,
    ip: Ipv4Addr,
    min_bytes: u64,
    window: Mutex<ActivityWindow>,
}

/// How long a connection has to move `min_bytes` for it to count as activity.
const ACTIVITY_WINDOW: Duration = Duration::from_secs(60);

/// Bytes a connection has moved in the current [`ACTIVITY_WINDOW`].
struct ActivityWindow {
    start: Instant,
    bytes: u64,
}

impl ConnectionActivity {
    /// Begin tracking a freshly accepted connection.
    ///
    /// A `min_bytes` of 0 restores the old behaviour: accepting the connection
    /// refreshes the idle timer immediately, as does any later traffic.
    fn start(limiter: TurnOffLimiter, ip: Ipv4Addr, min_bytes: u64) -> Arc<Self> {
        if min_bytes == 0 {
            limiter.update_last_request(ip);
        }
        Arc::new(Self {
            limiter,
            ip,
            min_bytes,
            window: Mutex::new(ActivityWindow {
                start: Instant::now(),
                bytes: 0,
            }),
        })
    }

    /// Record `n` bytes moved in either direction. Moving `min_bytes` within
    /// one [`ACTIVITY_WINDOW`] refreshes the idle timer.
    ///
    /// Merely being open is not activity: a long-lived connection such as a
    /// web app's websocket trickles keep-alive traffic (~150 bytes/min for Open
    /// WebUI) for as long as a tab stays open. Counting only bytes within a
    /// window stops that trickle from ever adding up to activity.
    fn record(&self, n: usize) {
        self.record_at(n, Instant::now());
    }

    fn record_at(&self, n: usize, now: Instant) {
        if n == 0 {
            return;
        }
        let total = {
            let mut window = self.window.lock().unwrap();
            if now.duration_since(window.start) >= ACTIVITY_WINDOW {
                window.start = now;
                window.bytes = 0;
            }
            window.bytes += n as u64;
            if window.bytes < self.min_bytes {
                return;
            }
            std::mem::take(&mut window.bytes)
        };
        debug!(
            "Connection to {} carried {} bytes within {:?}, counting it as activity",
            self.ip, total, ACTIVITY_WINDOW
        );
        self.limiter.update_last_request(self.ip);
    }
}

/// Wraps one half of a proxied connection so that every byte read from or
/// written to it is reported to [`ConnectionActivity`].
///
/// Wrapping the client side alone covers both directions: reads are bytes the
/// client sent, writes are bytes the machine sent back.
struct CountingStream<S> {
    inner: S,
    activity: Arc<ConnectionActivity>,
}

impl<S> CountingStream<S> {
    fn new(inner: S, activity: Arc<ConnectionActivity>) -> Self {
        Self { inner, activity }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for CountingStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            this.activity.record(buf.filled().len() - before);
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CountingStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &result {
            this.activity.record(*n);
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

struct MachineConfig {
    window: Duration,
    turn_off_port: u16,
    mac: String,
    last_turn_off_attempt: Mutex<Option<Instant>>,
    last_request: Instant,
    can_be_turned_off: bool,
}

#[derive(Clone)]
pub struct TurnOffLimiter {
    machines: Arc<Mutex<HashMap<Ipv4Addr, MachineConfig>>>,
    offline_since: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl Default for TurnOffLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnOffLimiter {
    pub fn new() -> Self {
        Self {
            machines: Arc::new(Mutex::new(HashMap::new())),
            offline_since: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn initialize_machine(&self, machine: &Machine, turn_off_port: u16) {
        let window_minutes = machine.inactivity_period.max(1);
        let window_secs = window_minutes.saturating_mul(60);
        let config = MachineConfig {
            window: Duration::from_secs(window_secs as u64),
            turn_off_port,
            mac: machine.mac.clone(),
            last_turn_off_attempt: Mutex::new(None),
            last_request: Instant::now(),
            can_be_turned_off: machine.can_be_turned_off,
        };
        let mut machines = self.machines.lock().unwrap();
        machines.insert(machine.ip, config);
    }

    #[allow(dead_code)]
    pub fn update_machine(&self, machine: &Machine, turn_off_port: u16) {
        let window_minutes = machine.inactivity_period.max(1);
        let window_secs = window_minutes.saturating_mul(60);
        let mut machines = self.machines.lock().unwrap();
        if let Some(config) = machines.get_mut(&machine.ip) {
            // Update existing configuration
            config.window = Duration::from_secs(window_secs as u64);
            config.turn_off_port = turn_off_port;
            config.mac = machine.mac.clone();
            // Reset attempt timer so it can trigger again if needed
            *config.last_turn_off_attempt.lock().unwrap() = None;
            debug!(
                "Updated inactivity monitoring configuration for machine {} (IP: {}): {}min",
                machine.mac, machine.ip, machine.inactivity_period
            );
        } else {
            // Machine not found, initialize it
            drop(machines);
            self.initialize_machine(machine, turn_off_port);
        }
    }

    pub fn idle_minutes(&self, ip: Ipv4Addr) -> Option<u64> {
        let machines = self.machines.lock().unwrap();
        machines.get(&ip).map(|config| {
            let elapsed = Instant::now().duration_since(config.last_request);
            elapsed.as_secs() / 60
        })
    }

    pub fn mark_offline(&self, mac: &str) {
        let mut map = self.offline_since.lock().unwrap();
        map.entry(mac.to_string()).or_insert_with(std::time::Instant::now);
    }

    pub fn mark_online(&self, mac: &str) {
        let mut map = self.offline_since.lock().unwrap();
        map.remove(mac);
    }

    pub fn offline_minutes_for_mac(&self, mac: &str) -> Option<u64> {
        let map = self.offline_since.lock().unwrap();
        map.get(mac).map(|since| since.elapsed().as_secs() / 60)
    }

    pub fn remove_machine(&self, ip: Ipv4Addr) {
        {
            let mut machines = self.machines.lock().unwrap();
            if machines.remove(&ip).is_some() {
                debug!(
                    "Removed machine with IP {} from inactivity monitor",
                    ip
                );
            }
        }
    }

    pub fn update_last_request(&self, ip: Ipv4Addr) {
        let mut machines = self.machines.lock().unwrap();
        if let Some(config) = machines.get_mut(&ip) {
            config.last_request = Instant::now();
            *config.last_turn_off_attempt.lock().unwrap() = None;
            debug!(
                "Updated last_request for machine {} (IP: {})",
                config.mac, ip
            );
        }
    }

    #[cfg(test)]
    fn secs_since_last_request(&self, ip: Ipv4Addr) -> Option<u64> {
        let machines = self.machines.lock().unwrap();
        machines
            .get(&ip)
            .map(|config| Instant::now().duration_since(config.last_request).as_secs())
    }

    /// Backdate a machine's last request so tests can assert whether a code
    /// path refreshed it, without waiting for real time to pass.
    #[cfg(test)]
    fn backdate_last_request(&self, ip: Ipv4Addr, age: Duration) {
        let mut machines = self.machines.lock().unwrap();
        if let Some(config) = machines.get_mut(&ip) {
            config.last_request = Instant::now() - age;
        }
    }

    pub fn start_inactivity_monitor(&self) -> tokio::task::AbortHandle {
        let limiter = self.clone();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                let now = Instant::now();
                let machines_to_check: Vec<(Ipv4Addr, u16, String)> = {
                    let machines = limiter.machines.lock().unwrap();
                    machines
                        .iter()
                        .filter_map(|(ip, config)| {
                            let time_since_last_request = now.duration_since(config.last_request);
                            debug!(
                                "Checking inactivity for machine {} (IP: {}): last request was {:?} ago, window is {:?}",
                                config.mac, ip, time_since_last_request, config.window
                            );
                            if time_since_last_request > config.window {
                                if config.can_be_turned_off {
                                    let should_trigger = {
                                        let mut last_attempt = config.last_turn_off_attempt.lock().unwrap();
                                        let can_attempt = last_attempt
                                            .map(|t| now.duration_since(t) > config.window)
                                            .unwrap_or(true);
                                        if can_attempt {
                                            *last_attempt = Some(now);
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    if should_trigger {
                                        debug!(
                                            "Machine {} (IP: {}) has been inactive for {:?}, exceeding window of {:?}",
                                            config.mac, ip, time_since_last_request, config.window
                                        );
                                        Some((*ip, config.turn_off_port, config.mac.clone()))
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                        .collect()
                };

                for (ip, turn_off_port, mac) in machines_to_check {
                    let remote_ip = ip.to_string();
                    let limiter = limiter.clone();
                    debug!(
                        "Sending turn-off signal for inactive machine {} (IP: {})",
                        mac, remote_ip
                    );
                    tokio::spawn(async move {
                        match turn_off_remote_machine(&remote_ip, turn_off_port).await {
                            Ok(_) => limiter.mark_offline(&mac),
                            Err(e) => error!(
                                "Failed to send turn-off signal for inactive machine {} on {}:{}: {}",
                                mac, remote_ip, turn_off_port, e
                            ),
                        }
                    });
                }
            }
        }).abort_handle();
        handle
    }

    pub async fn proxy_internal(
        &self,
        local_port: u16,
        remote_addr: SocketAddr,
        machine: Machine,
        mut rx: watch::Receiver<bool>,
        config: Arc<Config>,
    ) -> Result<()> {
        let listen_addr = format!("0.0.0.0:{}", local_port);
        // Retry on EADDRINUSE to cover the window where a previous listener
        // for the same port has been signalled to stop but its TcpListener
        // hasn't been dropped yet (e.g. after update_machine_api re-spawns).
        const MAX_BIND_ATTEMPTS: u32 = 50;
        let listener = {
            let mut attempts: u32 = 0;
            loop {
                if !*rx.borrow() {
                    info!(
                        "Proxy for {} on port {} cancelled before binding.",
                        remote_addr, local_port
                    );
                    return Ok(());
                }
                match TcpListener::bind(&listen_addr).await {
                    Ok(l) => break l,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::AddrInUse
                            && attempts < MAX_BIND_ATTEMPTS =>
                    {
                        attempts += 1;
                        debug!(
                            "Port {} in use, retrying bind in 100ms (attempt {}/{})",
                            local_port, attempts, MAX_BIND_ATTEMPTS
                        );
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err(e) => {
                        return Err(anyhow::Error::from(e).context(format!(
                            "Failed to bind TCP listener on {}",
                            listen_addr
                        )));
                    }
                }
            }
        };
        info!(
            "TCP Forwarder listening on {}, proxying to {}, inactivity period: {}min",
            listen_addr, remote_addr, machine.inactivity_period
        );

        let machine_ip = machine.ip;

        // Note: Monitor is started globally, not per proxy

        loop {
            tokio::select! {
                result = rx.changed() => {
                    if result.is_err() || !*rx.borrow() {
                        info!("Proxy for {} on port {} cancelled.", remote_addr, local_port);
                        return Ok(());
                    }
                }
                result = listener.accept() => {
                    let (inbound, client_addr) = result
                        .context("Failed to accept incoming connection")?;
                    info!(
                        "Accepted connection from {} to forward to {}",
                        client_addr, remote_addr
                    );

                    let remote_addr_clone = remote_addr;
                    let mac_str_clone = machine.mac.clone();
                    let rate_limiter = self.clone();
                    let machine_ip_clone = machine_ip;
                    let config_clone = Arc::clone(&config);

                    // Accepting a connection is not activity on its own. The
                    // connection refreshes the idle timer each time it carries
                    // another `activity_min_bytes`, which the CountingStream
                    // below reports as bytes are proxied.
                    let activity = ConnectionActivity::start(
                        rate_limiter.clone(),
                        machine_ip_clone,
                        config_clone.health.activity_min_bytes,
                    );

                    tokio::spawn(async move {
                        'conn: {
                            let connect_timeout = Duration::from_millis(1000);
                            if !wol::tcp_check(remote_addr_clone, connect_timeout).await {
                                info!(
                                    "Host {} seems to be down. Sending WOL packet to MAC {}.",
                                    remote_addr_clone, mac_str_clone
                                );

                                let mac = match wol::parse_mac(&mac_str_clone) {
                                    Ok(m) => m,
                                    Err(e) => {
                                        error!("Invalid MAC for WOL on proxy: {}: {}", mac_str_clone, e);
                                        break 'conn;
                                    }
                                };

                                let wol_port = config_clone.wol.default_port;
                                let wol_count = config_clone.wol.default_packet_count;
                                if let Err(e) = crate::wol::send_packets(
                                    &mac,
                                    wol_port,
                                    wol_count,
                                    &config_clone,
                                )
                                .await
                                {
                                    error!("Failed to send WOL packet for {}: {}", mac_str_clone, e);
                                    break 'conn;
                                }

                                info!(
                                    "WOL packet sent. Waiting up to 60s for {} to become reachable...",
                                    remote_addr_clone
                                );

                                let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
                                let mut host_up = false;
                                while tokio::time::Instant::now() < deadline {
                                    if wol::tcp_check(remote_addr_clone, connect_timeout).await {
                                        info!("Host {} is now up.", remote_addr_clone);
                                        host_up = true;
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_secs(2)).await;
                                }

                                if !host_up {
                                    warn!(
                                        "Timeout waiting for host {} to come up. Dropping connection from {}.",
                                        remote_addr_clone, client_addr
                                    );
                                    break 'conn;
                                }
                            }

                            rate_limiter.mark_online(&mac_str_clone);

                            let mut outbound = match tokio::time::timeout(
                                Duration::from_secs(30),
                                tokio::net::TcpStream::connect(remote_addr_clone),
                            )
                            .await
                            {
                                Ok(Ok(stream)) => {
                                    debug!("Successfully connected to {}", remote_addr_clone);
                                    stream
                                }
                                Ok(Err(e)) => {
                                    error!(
                                        "Failed to connect to remote {}: {}",
                                        remote_addr_clone, e
                                    );
                                    break 'conn;
                                }
                                Err(_) => {
                                    error!("Timeout connecting to remote {}", remote_addr_clone);
                                    break 'conn;
                                }
                            };

                            // Counting the client side covers both directions:
                            // reads are client -> machine, writes are machine -> client.
                            let mut inbound = CountingStream::new(inbound, Arc::clone(&activity));

                            match copy_bidirectional(&mut inbound, &mut outbound).await {
                                Ok(_) => {
                                    drop(outbound);
                                    debug!(
                                        "Completed data transfer for {} (connection closed)",
                                        remote_addr_clone
                                    );
                                }
                                Err(e) => {
                                    drop(outbound);
                                    warn!(
                                        "Error forwarding data between {} and {}: {}",
                                        client_addr, remote_addr_clone, e
                                    );
                                }
                            }
                        }
                    });
                }
            }
        }
    }

    pub async fn proxy(
        local_port: u16,
        remote_addr: SocketAddr,
        machine: Machine,
        rx: watch::Receiver<bool>,
        limiter: Arc<TurnOffLimiter>,
        config: Arc<Config>,
    ) -> Result<()> {
        limiter
            .proxy_internal(local_port, remote_addr, machine, rx, config)
            .await
    }
}

pub async fn turn_off_remote_machine(
    remote_ip: &str,
    turn_off_port: u16,
) -> Result<(), reqwest::Error> {
    let url = turn_off_url(remote_ip, turn_off_port);
    info!("Sending turn-off signal to {}", url);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;

    let response = client.post(&url).send().await?;
    if response.status().is_success() {
        info!(
            "Successfully sent turn-off signal to {}:{}",
            remote_ip, turn_off_port
        );
    } else {
        error!(
            "Failed to send turn-off signal to {}:{}, status: {}",
            remote_ip,
            turn_off_port,
            response.status()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    const TEST_IP: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);

    fn test_machine() -> Machine {
        Machine {
            mac: "AA:BB:CC:DD:EE:FF".to_string(),
            ip: TEST_IP,
            name: "test".to_string(),
            description: None,
            turn_off_port: Some(3001),
            can_be_turned_off: true,
            inactivity_period: 30,
            port_forwards: Vec::new(),
        }
    }

    fn tracked_limiter() -> TurnOffLimiter {
        let limiter = TurnOffLimiter::new();
        limiter.initialize_machine(&test_machine(), 3001);
        limiter
    }

    #[test]
    fn connection_below_threshold_is_not_activity() {
        let limiter = tracked_limiter();
        let activity = ConnectionActivity::start(limiter.clone(), TEST_IP, 4096);
        limiter.backdate_last_request(TEST_IP, Duration::from_secs(600));

        activity.record(100);
        activity.record(200);
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() >= 600);

        // Closing it must not reset the timer either, otherwise a poller
        // still keeps the machine awake.
        drop(activity);
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() >= 600);
    }

    #[test]
    fn open_connection_with_keepalive_pings_is_not_activity() {
        let limiter = tracked_limiter();
        let activity = ConnectionActivity::start(limiter.clone(), TEST_IP, 4096);
        limiter.backdate_last_request(TEST_IP, Duration::from_secs(600));

        // A websocket held open by an idle browser tab: ~15-byte pings.
        for _ in 0..100 {
            activity.record(15);
        }
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() >= 600);
        assert_eq!(limiter.idle_minutes(TEST_IP), Some(10));
    }

    #[test]
    fn slow_keepalive_trickle_never_adds_up_to_activity() {
        let limiter = tracked_limiter();
        let activity = ConnectionActivity::start(limiter.clone(), TEST_IP, 4096);
        limiter.backdate_last_request(TEST_IP, Duration::from_secs(600));

        // Open WebUI's idle websocket: ~150 bytes/min, here for two hours.
        let start = Instant::now();
        for minute in 0..120 {
            activity.record_at(150, start + Duration::from_secs(60 * minute));
        }
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() >= 600);
    }

    #[test]
    fn bytes_from_an_expired_window_do_not_count() {
        let limiter = tracked_limiter();
        let activity = ConnectionActivity::start(limiter.clone(), TEST_IP, 4096);
        limiter.backdate_last_request(TEST_IP, Duration::from_secs(600));

        let start = Instant::now();
        activity.record_at(4000, start);
        activity.record_at(200, start + ACTIVITY_WINDOW);
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() >= 600);

        // Within the new window it still counts once the threshold is reached.
        activity.record_at(3896, start + ACTIVITY_WINDOW + Duration::from_secs(1));
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() < 5);
    }

    #[test]
    fn every_threshold_of_bytes_refreshes_idle_timer() {
        let limiter = tracked_limiter();
        let activity = ConnectionActivity::start(limiter.clone(), TEST_IP, 4096);
        limiter.backdate_last_request(TEST_IP, Duration::from_secs(600));

        activity.record(4000);
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() >= 600);
        activity.record(96);
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() < 5);

        // The counter restarts, so the same connection has to carry another
        // full threshold before it refreshes the timer again.
        limiter.backdate_last_request(TEST_IP, Duration::from_secs(600));
        activity.record(4000);
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() >= 600);
        activity.record(96);
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() < 5);
    }

    #[test]
    fn zero_threshold_counts_every_connection() {
        let limiter = tracked_limiter();
        limiter.backdate_last_request(TEST_IP, Duration::from_secs(600));

        let activity = ConnectionActivity::start(limiter.clone(), TEST_IP, 0);
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() < 5);

        limiter.backdate_last_request(TEST_IP, Duration::from_secs(600));
        activity.record(1);
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() < 5);
    }

    #[tokio::test]
    async fn counting_stream_counts_both_directions() {
        let limiter = tracked_limiter();
        limiter.backdate_last_request(TEST_IP, Duration::from_secs(600));
        let activity = ConnectionActivity::start(limiter.clone(), TEST_IP, 8);

        let (client, mut peer) = tokio::io::duplex(64);
        let mut counted = CountingStream::new(client, Arc::clone(&activity));

        // 4 bytes machine -> client: still below the threshold.
        peer.write_all(b"abcd").await.expect("peer write failed");
        let mut buf = [0u8; 4];
        counted.read_exact(&mut buf).await.expect("read failed");
        assert_eq!(&buf, b"abcd");
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() >= 600);

        // 4 more bytes client -> machine crosses it.
        counted.write_all(b"efgh").await.expect("write failed");
        assert!(limiter.secs_since_last_request(TEST_IP).unwrap() < 5);
    }

    #[test]
    fn turn_off_url_formats_expected_path() {
        let url = super::turn_off_url("192.168.1.10", 8080);
        assert_eq!(url, "http://192.168.1.10:8080/machines/turn-off");
    }

    #[tokio::test]
    async fn turn_off_remote_machine_sends_expected_request() {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::PermissionDenied | ErrorKind::AddrNotAvailable
                ) =>
            {
                eprintln!(
                    "skipping test because binding TCP sockets is not permitted: {}",
                    err
                );
                return;
            }
            Err(err) => panic!("failed to bind http test listener: {err}"),
        };
        let addr = listener.local_addr().expect("failed to read listener addr");

        let received = Arc::new(Mutex::new(None));
        let received_clone = received.clone();

        let server_task = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = vec![0u8; 1024];
                if let Ok(n) = socket.read(&mut buf).await {
                    if n > 0 {
                        let request = String::from_utf8_lossy(&buf[..n]).to_string();
                        *received_clone.lock().await = Some(request);
                    }
                }
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
            }
        });

        turn_off_remote_machine(&addr.ip().to_string(), addr.port())
            .await
            .expect("turn_off_remote_machine should succeed");

        server_task.await.expect("server task panicked");

        let request = received.lock().await.clone().expect("no request captured");
        assert!(request.starts_with("POST /machines/turn-off"));

        let host_line = request
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("host:"))
            .unwrap_or_else(|| panic!("Host header missing in request: {request}"));

        let host_value = host_line.split_once(':').map(|(_, value)| value.trim());
        let expected_ip = addr.ip().to_string();
        let expected_with_port = format!("{}:{}", expected_ip, addr.port());
        assert!(
            matches!(host_value, Some(value) if value.eq_ignore_ascii_case(&expected_ip) || value.eq_ignore_ascii_case(&expected_with_port)),
            "unexpected host header: {host_line}"
        );
    }
}

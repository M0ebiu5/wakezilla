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
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};
use wakezilla_common::{RunScriptRequest, RunScriptResponse};

fn turn_off_url(remote_ip: &str, turn_off_port: u16) -> String {
    format!("http://{}:{}/machines/turn-off", remote_ip, turn_off_port)
}

fn run_script_url(remote_ip: &str, client_port: u16) -> String {
    format!("http://{}:{}/scripts/run", remote_ip, client_port)
}

/// A port-forward script and the forward it belongs to.
#[derive(Clone, Debug)]
struct ForwardScript {
    local_port: u16,
    target_port: u16,
    script: String,
}

impl ForwardScript {
    fn connect(pf: &crate::web::PortForward) -> Option<Self> {
        Self::new(pf, pf.on_connect_script.as_ref())
    }

    fn idle(pf: &crate::web::PortForward) -> Option<Self> {
        Self::new(pf, pf.on_idle_script.as_ref())
    }

    fn new(pf: &crate::web::PortForward, script: Option<&String>) -> Option<Self> {
        let script = script.filter(|s| !s.trim().is_empty())?;
        Some(Self {
            local_port: pf.local_port,
            target_port: pf.target_port,
            script: script.clone(),
        })
    }

    /// Has the machine's client server run this script and logs the outcome.
    /// Failures are logged, never fatal: a broken script must not stop the
    /// forward from working or the machine from being turned off.
    async fn run_on(
        &self,
        event: &str,
        remote_ip: &str,
        client_port: u16,
        config: &Config,
        retry_for: Duration,
    ) {
        let request = RunScriptRequest {
            event: event.to_string(),
            script: self.script.clone(),
            local_port: self.local_port,
            target_port: self.target_port,
            timeout_secs: config.server.script_timeout_secs,
        };
        match run_remote_script(remote_ip, client_port, &request, retry_for).await {
            Ok(response) if response.timed_out => warn!(
                "{} script for port {} on {} timed out after {}s",
                event, self.local_port, remote_ip, request.timeout_secs
            ),
            Ok(response) if response.exit_code == Some(0) => info!(
                "{} script for port {} on {} succeeded",
                event, self.local_port, remote_ip
            ),
            Ok(response) => warn!(
                "{} script for port {} on {} exited with {:?}: {}",
                event,
                self.local_port,
                remote_ip,
                response.exit_code,
                response.stderr.trim()
            ),
            Err(e) => error!(
                "Failed to run {} script for port {} on {}:{}: {:#}",
                event, self.local_port, remote_ip, client_port, e
            ),
        }
    }
}

/// Asks the client server on `remote_ip` to run a script and waits for it to
/// finish. Retries connection failures for `retry_for`, since a machine that
/// just woke up may accept connections on the forwarded port before its
/// client server is up.
async fn run_remote_script(
    remote_ip: &str,
    client_port: u16,
    request: &RunScriptRequest,
    retry_for: Duration,
) -> Result<RunScriptResponse> {
    let url = run_script_url(remote_ip, client_port);
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(request.timeout_secs + 10))
        .build()?;
    let body = serde_json::to_vec(request)?;
    let deadline = Instant::now() + retry_for;
    let response = loop {
        let post = client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());
        match post.send().await {
            Ok(response) => break response,
            Err(e) if e.is_connect() && Instant::now() < deadline => {
                debug!("Client server at {} not reachable yet: {}", url, e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e.into()),
        }
    };
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("client server answered {}: {}", status, body.trim());
    }
    Ok(serde_json::from_slice(&response.bytes().await?)?)
}

/// How long a connect script waits for the client server of a machine that
/// was just woken up.
const CONNECT_SCRIPT_CLIENT_WAIT: Duration = Duration::from_secs(15);

/// Runs a forward's connect script for new connections. Connections that
/// arrive while a run is in progress share the next run instead of each
/// starting their own, so a burst of connections (a browser loading a page)
/// runs the script at most twice.
struct ConnectScriptGate {
    script: ForwardScript,
    last_started: tokio::sync::Mutex<Option<Instant>>,
}

impl ConnectScriptGate {
    fn new(script: ForwardScript) -> Self {
        Self {
            script,
            last_started: tokio::sync::Mutex::new(None),
        }
    }

    /// Runs the script for a connection accepted at `accepted_at`, unless a
    /// run that started after that already covered it.
    async fn run_for(&self, accepted_at: Instant, remote_ip: &str, client_port: u16, config: &Config) {
        let mut last_started = self.last_started.lock().await;
        if last_started.is_some_and(|started| started >= accepted_at) {
            return;
        }
        *last_started = Some(Instant::now());
        self.script
            .run_on("connect", remote_ip, client_port, config, CONNECT_SCRIPT_CLIENT_WAIT)
            .await;
    }
}

/// Returns the path of an HTTP request line (`GET /path?query HTTP/1.1`),
/// without query string or fragment. `None` if `line` isn't an HTTP request line.
fn http_request_path(line: &[u8]) -> Option<&str> {
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split(' ');
    parts.next().filter(|method| !method.is_empty())?;
    let target = parts.next().filter(|t| t.starts_with('/'))?;
    parts.next().filter(|v| v.starts_with("HTTP/"))?;
    target.split(['?', '#']).next()
}

fn path_matches(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| match pattern.strip_suffix('*') {
        Some(prefix) => path.starts_with(prefix),
        None => path == pattern,
    })
}

/// Peeks (without consuming) the first HTTP request line on `stream` and
/// returns its path if it matches `no_wake_paths`. Non-HTTP traffic never
/// matches, so it still wakes the machine.
async fn no_wake_request_path(stream: &TcpStream, no_wake_paths: &[String]) -> Option<String> {
    if no_wake_paths.is_empty() {
        return None;
    }
    let mut buf = [0u8; 2048];
    let peek_request_line = async {
        loop {
            let n = stream.peek(&mut buf).await.ok()?;
            if n == 0 {
                return None;
            }
            if let Some(end) = buf[..n].windows(2).position(|w| w == b"\r\n") {
                return http_request_path(&buf[..end])
                    .filter(|path| path_matches(path, no_wake_paths))
                    .map(str::to_string);
            }
            if n == buf.len() {
                return None;
            }
            // Partial request line; peek returns immediately while data is
            // buffered, so back off briefly before looking again.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(1), peek_request_line)
        .await
        .ok()
        .flatten()
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
    idle_scripts: Vec<ForwardScript>,
}

/// What the inactivity monitor does for a machine whose idle timer fired.
struct IdleAction {
    ip: Ipv4Addr,
    turn_off_port: u16,
    mac: String,
    can_be_turned_off: bool,
    idle_scripts: Vec<ForwardScript>,
    /// The machine's last activity when the timer fired; any later activity
    /// cancels the turn-off.
    last_request: Instant,
}

fn idle_scripts(machine: &Machine) -> Vec<ForwardScript> {
    machine
        .port_forwards
        .iter()
        .filter_map(ForwardScript::idle)
        .collect()
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
            idle_scripts: idle_scripts(machine),
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
            config.can_be_turned_off = machine.can_be_turned_off;
            config.idle_scripts = idle_scripts(machine);
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

    fn last_request(&self, ip: Ipv4Addr) -> Option<Instant> {
        let machines = self.machines.lock().unwrap();
        machines.get(&ip).map(|config| config.last_request)
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

    pub fn start_inactivity_monitor(&self, config: Arc<Config>) -> tokio::task::AbortHandle {
        let limiter = self.clone();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                let now = Instant::now();
                let idle_actions: Vec<IdleAction> = {
                    let machines = limiter.machines.lock().unwrap();
                    machines
                        .iter()
                        .filter_map(|(ip, config)| {
                            let time_since_last_request = now.duration_since(config.last_request);
                            debug!(
                                "Checking inactivity for machine {} (IP: {}): last request was {:?} ago, window is {:?}",
                                config.mac, ip, time_since_last_request, config.window
                            );
                            if time_since_last_request <= config.window {
                                return None;
                            }
                            if !config.can_be_turned_off && config.idle_scripts.is_empty() {
                                return None;
                            }
                            let should_trigger = {
                                let mut last_attempt = config.last_turn_off_attempt.lock().unwrap();
                                // A turn-off is retried every window while the
                                // machine stays idle; idle scripts alone run
                                // once per idle period.
                                let can_attempt = last_attempt
                                    .map(|t| config.can_be_turned_off && now.duration_since(t) > config.window)
                                    .unwrap_or(true);
                                if can_attempt {
                                    *last_attempt = Some(now);
                                }
                                can_attempt
                            };
                            if !should_trigger {
                                return None;
                            }
                            debug!(
                                "Machine {} (IP: {}) has been inactive for {:?}, exceeding window of {:?}",
                                config.mac, ip, time_since_last_request, config.window
                            );
                            Some(IdleAction {
                                ip: *ip,
                                turn_off_port: config.turn_off_port,
                                mac: config.mac.clone(),
                                can_be_turned_off: config.can_be_turned_off,
                                idle_scripts: config.idle_scripts.clone(),
                                last_request: config.last_request,
                            })
                        })
                        .collect()
                };

                for action in idle_actions {
                    tokio::spawn(limiter.clone().run_idle_action(action, Arc::clone(&config)));
                }
            }
        }).abort_handle();
        handle
    }

    /// Runs a machine's idle scripts and waits for them to exit, then turns
    /// the machine off unless it became active in the meantime. The scripts'
    /// run time is the grace period: a script can wait for work to finish
    /// before the machine goes down.
    async fn run_idle_action(self, action: IdleAction, config: Arc<Config>) {
        let remote_ip = action.ip.to_string();
        if !action.idle_scripts.is_empty() {
            info!(
                "Machine {} (IP: {}) is idle, running {} idle script(s)",
                action.mac,
                remote_ip,
                action.idle_scripts.len()
            );
            for script in &action.idle_scripts {
                script
                    .run_on("idle", &remote_ip, action.turn_off_port, &config, Duration::ZERO)
                    .await;
            }
            if !action.can_be_turned_off {
                return;
            }
            if self.last_request(action.ip) != Some(action.last_request) {
                info!(
                    "Machine {} (IP: {}) became active while its idle scripts ran, not turning it off",
                    action.mac, remote_ip
                );
                return;
            }
        }

        debug!(
            "Sending turn-off signal for inactive machine {} (IP: {})",
            action.mac, remote_ip
        );
        match turn_off_remote_machine(&remote_ip, action.turn_off_port).await {
            Ok(_) => self.mark_offline(&action.mac),
            Err(e) => error!(
                "Failed to send turn-off signal for inactive machine {} on {}:{}: {}",
                action.mac, remote_ip, action.turn_off_port, e
            ),
        }
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
        let no_wake_paths: Arc<[String]> = machine
            .port_forwards
            .iter()
            .find(|pf| pf.local_port == local_port)
            .map(|pf| pf.no_wake_paths.clone())
            .unwrap_or_default()
            .into();
        let connect_script: Option<Arc<ConnectScriptGate>> = machine
            .port_forwards
            .iter()
            .find(|pf| pf.local_port == local_port)
            .and_then(ForwardScript::connect)
            .map(|script| Arc::new(ConnectScriptGate::new(script)));
        let client_port = machine.turn_off_port.unwrap_or(config.server.client_port);

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
                    let accepted_at = Instant::now();
                    info!(
                        "Accepted connection from {} to forward to {}",
                        client_addr, remote_addr
                    );

                    let remote_addr_clone = remote_addr;
                    let mac_str_clone = machine.mac.clone();
                    let rate_limiter = self.clone();
                    let machine_ip_clone = machine_ip;
                    let config_clone = Arc::clone(&config);
                    let no_wake_paths = Arc::clone(&no_wake_paths);
                    let connect_script = connect_script.clone();

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
                                // Background requests (e.g. a web app polling
                                // for updates) must not wake a sleeping machine.
                                if let Some(path) =
                                    no_wake_request_path(&inbound, &no_wake_paths).await
                                {
                                    debug!(
                                        "Host {} is down; not waking it for {} from {}",
                                        remote_addr_clone, path, client_addr
                                    );
                                    break 'conn;
                                }
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

                            if let Some(gate) = &connect_script {
                                gate.run_for(
                                    accepted_at,
                                    &machine_ip_clone.to_string(),
                                    client_port,
                                    &config_clone,
                                )
                                .await;
                            }

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
    fn http_request_path_strips_query_and_rejects_non_http() {
        assert_eq!(
            http_request_path(b"GET /_app/version.json?t=1 HTTP/1.1"),
            Some("/_app/version.json")
        );
        assert_eq!(http_request_path(b"POST /api/chat HTTP/1.1"), Some("/api/chat"));
        assert_eq!(http_request_path(b"SSH-2.0-OpenSSH_9.6"), None);
        assert_eq!(http_request_path(b"GET /path"), None);
    }

    #[test]
    fn path_matches_supports_exact_and_prefix_patterns() {
        let patterns = vec!["/_app/version.json".to_string(), "/ws/*".to_string()];
        assert!(path_matches("/_app/version.json", &patterns));
        assert!(path_matches("/ws/socket.io/", &patterns));
        assert!(!path_matches("/_app/version.json.bak", &patterns));
        assert!(!path_matches("/api/chats", &patterns));
        assert!(!path_matches("/anything", &[]));
    }

    async fn no_wake_path_after_sending(request: &[u8]) -> Option<(Option<String>, Vec<u8>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
        let addr = listener.local_addr().ok()?;
        let mut client = TcpStream::connect(addr).await.ok()?;
        let (mut server, _) = listener.accept().await.ok()?;
        client.write_all(request).await.ok()?;

        let patterns = vec!["/_app/version.json".to_string()];
        let matched = no_wake_request_path(&server, &patterns).await;

        // Peeking must leave the request intact for the upstream server.
        let mut received = vec![0u8; request.len()];
        server.read_exact(&mut received).await.ok()?;
        Some((matched, received))
    }

    #[tokio::test]
    async fn no_wake_request_path_peeks_without_consuming() {
        let poll = b"GET /_app/version.json HTTP/1.1\r\nHost: big:3002\r\n\r\n";
        let Some((matched, received)) = no_wake_path_after_sending(poll).await else {
            eprintln!("skipping test because binding TCP sockets is not permitted");
            return;
        };
        assert_eq!(matched.as_deref(), Some("/_app/version.json"));
        assert_eq!(received, poll);

        let real = b"GET /api/v1/chats HTTP/1.1\r\nHost: big:3002\r\n\r\n";
        let (matched, received) = no_wake_path_after_sending(real).await.unwrap();
        assert_eq!(matched, None);
        assert_eq!(received, real);
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

    /// A stand-in client server that records the paths it is asked for and
    /// answers every script with exit code 0, after 500ms for `slow-*` scripts.
    async fn fake_client_server() -> Option<(u16, Arc<std::sync::Mutex<Vec<String>>>)> {
        use axum::{routing::post, Json, Router};
        let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let script_hits = Arc::clone(&hits);
        let turn_off_hits = Arc::clone(&hits);
        let app = Router::new()
            .route(
                "/scripts/run",
                post(move |Json(request): Json<RunScriptRequest>| async move {
                    if request.script.starts_with("slow-") {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    script_hits
                        .lock()
                        .unwrap()
                        .push(format!("{}:{}", request.event, request.script));
                    Json(RunScriptResponse {
                        exit_code: Some(0),
                        timed_out: false,
                        stdout: String::new(),
                        stderr: String::new(),
                    })
                }),
            )
            .route(
                "/machines/turn-off",
                post(move || async move {
                    turn_off_hits.lock().unwrap().push("turn-off".to_string());
                    "ok"
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
        let port = listener.local_addr().ok()?.port();
        tokio::spawn(async move { axum::serve(listener, app).await });
        Some((port, hits))
    }

    fn script(text: &str) -> ForwardScript {
        ForwardScript {
            local_port: 8080,
            target_port: 80,
            script: text.to_string(),
        }
    }

    #[tokio::test]
    async fn connect_script_gate_runs_once_for_connections_it_covers() {
        let Some((port, hits)) = fake_client_server().await else {
            eprintln!("skipping test because binding TCP sockets is not permitted");
            return;
        };
        let config = Config::default();
        let gate = ConnectScriptGate::new(script("start-service"));

        let burst = Instant::now();
        gate.run_for(burst, "127.0.0.1", port, &config).await;
        // Accepted before the first run started, so that run covered it.
        gate.run_for(burst, "127.0.0.1", port, &config).await;
        assert_eq!(hits.lock().unwrap().len(), 1);

        gate.run_for(Instant::now(), "127.0.0.1", port, &config).await;
        assert_eq!(*hits.lock().unwrap(), vec!["connect:start-service"; 2]);
    }

    #[tokio::test]
    async fn run_remote_script_reports_client_refusal_as_error() {
        use axum::{http::StatusCode, routing::post, Router};
        let Ok(listener) = TcpListener::bind("127.0.0.1:0").await else {
            eprintln!("skipping test because binding TCP sockets is not permitted");
            return;
        };
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().route(
            "/scripts/run",
            post(|| async { (StatusCode::FORBIDDEN, "Scripts are disabled") }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await });

        let request = RunScriptRequest {
            event: "idle".into(),
            script: "true".into(),
            local_port: 8080,
            target_port: 80,
            timeout_secs: 5,
        };
        let err = run_remote_script("127.0.0.1", port, &request, Duration::ZERO)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("403"), "{err}");
    }

    fn idle_action(limiter: &TurnOffLimiter, port: u16, idle_script: &str) -> IdleAction {
        let ip = Ipv4Addr::LOCALHOST;
        let mut machine = test_machine();
        machine.ip = ip;
        limiter.initialize_machine(&machine, port);
        IdleAction {
            ip,
            turn_off_port: port,
            mac: machine.mac,
            can_be_turned_off: true,
            idle_scripts: vec![script(idle_script)],
            last_request: limiter.last_request(ip).unwrap(),
        }
    }

    #[tokio::test]
    async fn idle_action_turns_off_once_idle_scripts_exit() {
        let Some((port, hits)) = fake_client_server().await else {
            eprintln!("skipping test because binding TCP sockets is not permitted");
            return;
        };
        let limiter = TurnOffLimiter::new();
        let action = idle_action(&limiter, port, "slow-stop");
        limiter.clone().run_idle_action(action, Arc::default()).await;
        assert_eq!(*hits.lock().unwrap(), vec!["idle:slow-stop", "turn-off"]);
    }

    #[tokio::test]
    async fn activity_while_idle_scripts_run_cancels_turn_off() {
        let Some((port, hits)) = fake_client_server().await else {
            eprintln!("skipping test because binding TCP sockets is not permitted");
            return;
        };
        let limiter = TurnOffLimiter::new();
        let action = idle_action(&limiter, port, "slow-stop");
        let task = tokio::spawn(limiter.clone().run_idle_action(action, Arc::default()));
        tokio::time::sleep(Duration::from_millis(200)).await;
        limiter.update_last_request(Ipv4Addr::LOCALHOST);
        task.await.unwrap();
        assert_eq!(*hits.lock().unwrap(), vec!["idle:slow-stop"]);
    }
}

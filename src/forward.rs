use crate::{config::Config, web::Machine, wol};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

fn turn_off_url(remote_ip: &str, turn_off_port: u16) -> String {
    format!("http://{}:{}/machines/turn-off", remote_ip, turn_off_port)
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
    active_connections: Arc<Mutex<HashMap<Ipv4Addr, u32>>>,
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
            active_connections: Arc::new(Mutex::new(HashMap::new())),
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
        {
            let active = self.active_connections.lock().unwrap();
            if active.get(&ip).copied().unwrap_or(0) > 0 {
                return Some(0);
            }
        }
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
        self.active_connections.lock().unwrap().remove(&ip);
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

    pub fn connection_started(&self, ip: Ipv4Addr) {
        let mut map = self.active_connections.lock().unwrap();
        *map.entry(ip).or_insert(0) += 1;
    }

    pub fn connection_ended(&self, ip: Ipv4Addr) {
        {
            let mut map = self.active_connections.lock().unwrap();
            if let Some(count) = map.get_mut(&ip) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    map.remove(&ip);
                }
            }
        }
        self.update_last_request(ip);
    }

    pub fn start_inactivity_monitor(&self) -> tokio::task::AbortHandle {
        let limiter = self.clone();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                let now = Instant::now();
                let active_snapshot: HashMap<Ipv4Addr, u32> =
                    limiter.active_connections.lock().unwrap().clone();
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
                            if active_snapshot.get(ip).copied().unwrap_or(0) > 0 {
                                return None;
                            }
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
                    debug!(
                        "Sending turn-off signal for inactive machine {} (IP: {})",
                        mac, remote_ip
                    );
                    tokio::spawn(async move {
                        if let Err(e) = turn_off_remote_machine(&remote_ip, turn_off_port).await {
                            error!(
                                "Failed to send turn-off signal for inactive machine {} on {}:{}: {}",
                                mac, remote_ip, turn_off_port, e
                            );
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
        let listener = TcpListener::bind(&listen_addr)
            .await
            .with_context(|| format!("Failed to bind TCP listener on {}", listen_addr))?;
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
                    let (mut inbound, client_addr) = result
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

                    tokio::spawn(async move {
                        rate_limiter.update_last_request(machine_ip_clone);
                        rate_limiter.connection_started(machine_ip_clone);

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

                        rate_limiter.connection_ended(machine_ip_clone);
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

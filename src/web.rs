use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{watch, RwLock};
use tracing::{debug, error, info};
use validator::ValidationError;

use serde::{Deserializer, Serializer};
use std::str::FromStr;

fn serialize_ipv4addr<S>(ip: &Ipv4Addr, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&ip.to_string())
}

fn deserialize_ipv4addr<'de, D>(deserializer: D) -> Result<Ipv4Addr, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ipv4Addr::from_str(&s).map_err(serde::de::Error::custom)
}

use crate::config::Config;
use crate::forward;

const DEFAULT_DB_PATH: &str = "machines.json";

fn machines_db_path() -> PathBuf {
    // First check for environment variable override
    if let Ok(path) = std::env::var("WAKEZILLA__STORAGE__MACHINES_DB_PATH") {
        return PathBuf::from(path);
    }

    // Use ~/.local/share/wakezilla/machines.json so the file is stable
    // regardless of which directory the binary is launched from.
    if let Some(home) = env::var_os("HOME") {
        let dir = PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("wakezilla");
        if let Err(e) = fs::create_dir_all(&dir) {
            tracing::warn!("Could not create data directory {}: {}", dir.display(), e);
        } else {
            return dir.join(DEFAULT_DB_PATH);
        }
    }

    // Fallback: current working directory (old behaviour)
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(DEFAULT_DB_PATH)
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Machine {
    pub mac: String,
    #[serde(
        serialize_with = "serialize_ipv4addr",
        deserialize_with = "deserialize_ipv4addr"
    )]
    pub ip: Ipv4Addr,
    pub name: String,
    pub description: Option<String>,
    pub turn_off_port: Option<u16>,
    pub can_be_turned_off: bool,
    #[serde(default = "get_default_inactivity_period")]
    pub inactivity_period: u32,

    pub port_forwards: Vec<PortForward>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct PortForward {
    pub name: String,
    pub local_port: u16,
    pub target_port: u16,
    #[serde(default)]
    pub no_wake_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_connect_script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_script: Option<String>,
}

/// Blank scripts are stored as "no script".
fn non_blank_script(script: &Option<String>) -> Option<String> {
    script.clone().filter(|s| !s.trim().is_empty())
}

pub fn validate_ip(ip: &str) -> Result<(), ValidationError> {
    if ip.parse::<IpAddr>().is_ok() {
        Ok(())
    } else {
        Err(ValidationError::new("Invalid IP address"))
    }
}

static MAC_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^([0-9A-Fa-f]{2}[:-]){5}([0-9A-Fa-f]{2})$").unwrap());

pub fn validate_mac(mac: &str) -> Result<(), ValidationError> {
    if MAC_REGEX.is_match(mac) {
        Ok(())
    } else {
        Err(ValidationError::new("Invalid MAC address"))
    }
}
pub fn get_default_inactivity_period() -> u32 {
    30
}

#[derive(Clone)]
pub struct AppState {
    pub machines: Arc<RwLock<Vec<Machine>>>,
    pub proxies: Arc<RwLock<HashMap<String, watch::Sender<bool>>>>,
    pub config: Arc<Config>,
    pub turn_off_limiter: Arc<forward::TurnOffLimiter>,
    pub monitor_handle: Arc<std::sync::Mutex<Option<tokio::task::AbortHandle>>>,
}

pub fn api_port_forward_to_internal(pf: &wakezilla_common::PortForward) -> PortForward {
    PortForward {
        name: pf.name.clone().unwrap_or_default(),
        local_port: pf.local_port,
        target_port: pf.target_port,
        no_wake_paths: pf.no_wake_paths.clone(),
        on_connect_script: non_blank_script(&pf.on_connect_script),
        on_idle_script: non_blank_script(&pf.on_idle_script),
    }
}

pub fn internal_port_forward_to_api(pf: &PortForward) -> wakezilla_common::PortForward {
    wakezilla_common::PortForward {
        name: if pf.name.trim().is_empty() {
            None
        } else {
            Some(pf.name.clone())
        },
        local_port: pf.local_port,
        target_port: pf.target_port,
        no_wake_paths: pf.no_wake_paths.clone(),
        on_connect_script: pf.on_connect_script.clone(),
        on_idle_script: pf.on_idle_script.clone(),
    }
}

pub fn machine_to_api_machine(
    machine: &Machine,
    limiter: Option<&forward::TurnOffLimiter>,
) -> wakezilla_common::Machine {
    let idle_minutes = limiter.and_then(|l| l.idle_minutes(machine.ip));
    let offline_minutes = limiter.and_then(|l| l.offline_minutes_for_mac(&machine.mac));
    wakezilla_common::Machine {
        name: machine.name.clone(),
        mac: machine.mac.clone(),
        ip: machine.ip.to_string(),
        description: machine.description.clone(),
        turn_off_port: machine.turn_off_port,
        can_be_turned_off: machine.can_be_turned_off,
        inactivity_period: machine.inactivity_period,
        port_forwards: machine
            .port_forwards
            .iter()
            .map(internal_port_forward_to_api)
            .collect(),
        idle_minutes,
        offline_minutes,
    }
}

pub fn api_machine_to_internal(api: &wakezilla_common::Machine) -> Result<Machine> {
    let ip = api
        .ip
        .parse::<Ipv4Addr>()
        .with_context(|| format!("Invalid IPv4 address: {}", api.ip))?;

    Ok(Machine {
        mac: api.mac.clone(),
        ip,
        name: api.name.clone(),
        description: api.description.clone(),
        turn_off_port: api.turn_off_port,
        can_be_turned_off: api.can_be_turned_off,
        inactivity_period: api.inactivity_period,
        port_forwards: api
            .port_forwards
            .iter()
            .map(api_port_forward_to_internal)
            .collect(),
    })
}

/// Load machines using the configured database path
pub fn load_machines() -> Result<Vec<Machine>> {
    load_machines_from_path(machines_db_path())
}

/// Load machines from a specific path
pub fn load_machines_from_path<P: AsRef<Path>>(path: P) -> Result<Vec<Machine>> {
    let path_ref = path.as_ref();
    let data = fs::read_to_string(path_ref).with_context(|| {
        format!(
            "Failed to read machines database from {}",
            path_ref.display()
        )
    })?;

    let machines: Vec<Machine> =
        serde_json::from_str(&data).with_context(|| "Failed to parse machines database")?;

    info!(
        "Successfully loaded {} machines from database at {:?}",
        machines.len(),
        path_ref
    );
    Ok(machines)
}

pub fn save_machines(machines: &[Machine]) -> Result<()> {
    tracing::debug!("Saving machines {:?}", machines);
    let data =
        serde_json::to_string_pretty(machines).context("Failed to serialize machines data")?;
    let path = machines_db_path();
    info!("Saving machines database to {}", path.display());
    fs::write(&path, data)
        .with_context(|| format!("Failed to write machines database to {}", path.display()))
}

pub fn start_proxy_if_configured(machine: &Machine, state: &AppState) {
    // Initialize idle tracking for any machine with port forwards so that
    // idle_minutes is always available in the API. Turn-off is only triggered
    // by the inactivity monitor when can_be_turned_off is true.
    if !machine.port_forwards.is_empty() || machine.can_be_turned_off {
        let turn_off_port = machine.turn_off_port.unwrap_or(3001);
        state.turn_off_limiter.initialize_machine(machine, turn_off_port);
        info!(
            "Initialized idle tracking for machine {} ({}): {}min inactivity period, can_turn_off={}",
            machine.mac, machine.ip, machine.inactivity_period, machine.can_be_turned_off
        );
    } else {
        info!(
            "Machine {} has no port forwards and cannot be turned off, skipping idle tracking",
            machine.mac
        );
    }

    for pf in &machine.port_forwards {
        let remote_addr = SocketAddr::new(machine.ip.into(), pf.target_port);
        let local_port = pf.local_port;
        let machine_clone = machine.clone();
        let config_clone = state.config.clone();

        let (tx, rx) = watch::channel(true);
        // The key for the proxy should probably include the port to be unique
        let proxy_key = format!("{}-{}-{}", machine.mac, local_port, pf.target_port);

        let proxies_clone = state.proxies.clone();
        let limiter_clone = state.turn_off_limiter.clone();
        tokio::spawn(async move {
            let mut proxies = proxies_clone.write().await;
            proxies.insert(proxy_key.clone(), tx);

            // We can't hold the lock across the await, so we need to drop it here
            drop(proxies);

            if let Err(e) = forward::TurnOffLimiter::proxy(
                local_port,
                remote_addr,
                machine_clone,
                rx,
                limiter_clone,
                config_clone,
            )
            .await
            {
                error!(
                    "Forwarder for {} -> {} failed: {}",
                    local_port, remote_addr, e
                );
            }
        });
    }
}

pub fn start_global_monitor(state: &AppState) {
    let mut handle_guard = state.monitor_handle.lock().unwrap();
    if handle_guard.is_none() {
        let handle = state
            .turn_off_limiter
            .start_inactivity_monitor(Arc::clone(&state.config));
        *handle_guard = Some(handle);
        info!("Started global inactivity monitor");
    }
}

pub fn restart_global_monitor(state: &AppState) {
    let mut handle_guard = state.monitor_handle.lock().unwrap();
    if let Some(handle) = handle_guard.take() {
        handle.abort();
        info!("Stopped old inactivity monitor");
    }
    let handle = state
            .turn_off_limiter
            .start_inactivity_monitor(Arc::clone(&state.config));
    *handle_guard = Some(handle);
    info!("Restarted global inactivity monitor");
}

#[cfg(test)]
mod tests {
    use super::*;
    use once_cell::sync::Lazy;
    use std::net::Ipv4Addr;
    use tempfile::{tempdir, NamedTempFile};

    static ENV_LOCK: Lazy<std::sync::Mutex<()>> = Lazy::new(|| std::sync::Mutex::new(()));

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set_path(key: &'static str, value: &std::path::Path) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value.as_os_str());
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(ref original) = self.original {
                std::env::set_var(self.key, original);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn validate_ip_accepts_valid_addresses() {
        assert!(validate_ip("192.168.0.1").is_ok());
        assert!(validate_ip("::1").is_ok());
    }

    #[test]
    fn validate_ip_rejects_invalid_addresses() {
        assert!(validate_ip("not-an-ip").is_err());
        assert!(validate_ip("999.999.999.999").is_err());
    }

    #[test]
    fn validate_mac_accepts_common_format() {
        assert!(validate_mac("AA:BB:CC:DD:EE:FF").is_ok());
    }

    #[test]
    fn validate_mac_rejects_bad_input() {
        assert!(validate_mac("zz:zz:zz:zz:zz:zz").is_err());
    }

    #[test]
    fn load_machines_from_path_reads_file() {
        let mut file = NamedTempFile::new().expect("failed to create temp file");
        let json = r#"
            [
                {
                    "mac": "AA:BB:CC:DD:EE:FF",
                    "ip": "192.168.1.10",
                    "name": "Test",
                    "description": null,
                    "turn_off_port": 8080,
                    "can_be_turned_off": true,
                    "inactivity_period": 10,
                    "port_forwards": []
                }
            ]
        "#;
        use std::io::Write;
        file.write_all(json.as_bytes())
            .expect("failed to write json");
        let machines = load_machines_from_path(file.path()).expect("load should succeed");
        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0].mac, "AA:BB:CC:DD:EE:FF");
        assert_eq!(machines[0].ip, Ipv4Addr::new(192, 168, 1, 10));
    }

    #[test]
    fn save_machines_writes_using_configured_path() {
        let _lock = ENV_LOCK.lock().unwrap();
        let tmp_dir = tempdir().expect("failed to create temp dir");
        let file_path = tmp_dir.path().join("machines.json");
        let _guard = EnvGuard::set_path("WAKEZILLA__STORAGE__MACHINES_DB_PATH", &file_path);

        let machines = vec![Machine {
            mac: "AA:BB:CC:DD:EE:FF".to_string(),
            ip: Ipv4Addr::new(10, 0, 0, 1),
            name: "Test".to_string(),
            description: Some("Example".to_string()),
            turn_off_port: Some(9000),
            can_be_turned_off: true,
            inactivity_period: get_default_inactivity_period(),
            port_forwards: vec![],
        }];

        save_machines(&machines).expect("save should succeed");

        let resolved_path = super::machines_db_path();
        assert_eq!(resolved_path, file_path);
        assert!(resolved_path.exists(), "machines db path should exist");

        let contents = std::fs::read_to_string(&resolved_path).expect("failed to read file");
        let data: serde_json::Value = serde_json::from_str(&contents).expect("valid json");
        assert_eq!(data[0]["mac"], "AA:BB:CC:DD:EE:FF");
        assert_eq!(data[0]["ip"], "10.0.0.1");
    }
}

//! Configuration management for Wakezilla
//!
//! This module provides a comprehensive configuration system that:
//! - Centralizes all configurable values
//! - Supports environment variables with `serde`
//! - Provides sensible defaults
//! - Validates configuration at runtime

use serde::{Deserialize, Serialize};

/// Default configuration file path for machines database
pub const DEFAULT_MACHINES_DB_PATH: &str = "machines.json";

/// Main configuration structure for the entire application
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Server configuration
    #[serde(default)]
    pub server: ServerConfig,

    /// Wake-on-LAN configuration
    #[serde(default)]
    pub wol: WolConfig,

    /// Network scanning configuration
    #[serde(default)]
    pub network: NetworkConfig,

    /// File system configuration
    #[serde(default)]
    pub storage: StorageConfig,

    /// Health check configuration
    #[serde(default)]
    pub health: HealthConfig,
}

impl Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self, config::ConfigError> {
        config::Config::builder()
            .add_source(
                config::Environment::with_prefix("WAKEZILLA")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?
            .try_deserialize()
    }
}

/// Server-related configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Port for the proxy server (default: 3000)
    #[serde(default = "default_proxy_port")]
    pub proxy_port: u16,

    /// Port for the client server (default: 3001)
    #[serde(default = "default_client_port")]
    pub client_port: u16,

    /// HTTP health check timeout in seconds (default: 5)
    #[serde(default = "default_health_timeout_secs")]
    pub health_timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            proxy_port: default_proxy_port(),
            client_port: default_client_port(),
            health_timeout_secs: default_health_timeout_secs(),
        }
    }
}

/// Wake-on-LAN specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WolConfig {
    /// Default WOL port (default: 9)
    #[serde(default = "default_wol_port")]
    pub default_port: u16,

    /// Default broadcast IP (default: 255.255.255.255)
    #[serde(default = "default_broadcast_ip")]
    pub default_broadcast_ip: String,

    /// Default number of WOL packets to send (default: 3)
    #[serde(default = "default_wol_packet_count")]
    pub default_packet_count: u32,

    /// Sleep interval between WOL packets in milliseconds (default: 50)
    #[serde(default = "default_wol_packet_sleeptime_ms")]
    pub packet_sleeptime_ms: u64,

    /// Default wait time for WOL in seconds (default: 90)
    #[serde(default = "default_wol_wait_secs")]
    pub default_wait_secs: u64,

    /// Default poll interval between checks in milliseconds (default: 1000)
    #[serde(default = "default_wol_poll_interval_ms")]
    pub default_poll_interval_ms: u64,

    /// Default TCP connect timeout in milliseconds (default: 700)
    #[serde(default = "default_wol_connect_timeout_ms")]
    pub default_connect_timeout_ms: u64,
}

impl Default for WolConfig {
    fn default() -> Self {
        Self {
            default_port: default_wol_port(),
            default_broadcast_ip: default_broadcast_ip(),
            default_packet_count: default_wol_packet_count(),
            packet_sleeptime_ms: default_wol_packet_sleeptime_ms(),
            default_wait_secs: default_wol_wait_secs(),
            default_poll_interval_ms: default_wol_poll_interval_ms(),
            default_connect_timeout_ms: default_wol_connect_timeout_ms(),
        }
    }
}

/// Network scanning configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Network scanning duration in seconds (default: 5)
    #[serde(default = "default_network_scan_duration_secs")]
    pub scan_duration_secs: u64,

    /// Network read timeout in seconds (default: 2)
    #[serde(default = "default_network_read_timeout_secs")]
    pub read_timeout_secs: u64,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            scan_duration_secs: default_network_scan_duration_secs(),
            read_timeout_secs: default_network_read_timeout_secs(),
        }
    }
}

/// File system and storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Path to the machines database file (default: "machines.json")
    #[serde(default = "default_machines_db_path")]
    pub machines_db_path: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            machines_db_path: default_machines_db_path(),
        }
    }
}

/// Health check configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// Health check interval in milliseconds (default: 30000)
    #[serde(default = "default_health_check_interval_ms")]
    pub check_interval_ms: u64,

    /// Proxy connect timeout in milliseconds (default: 1000)
    #[serde(default = "default_proxy_connect_timeout_ms")]
    pub proxy_connect_timeout_ms: u64,

    /// Proxy WOL wait time in seconds (default: 60)
    #[serde(default = "default_proxy_wol_wait_secs")]
    pub proxy_wol_wait_secs: u64,

    /// System shutdown sleep time in seconds (default: 5)
    #[serde(default = "default_system_shutdown_sleep_secs")]
    pub system_shutdown_sleep_secs: u64,

    /// Rate limiting sampling interval in seconds (default: 1)
    #[serde(default = "default_rate_limit_sample_interval_secs")]
    pub rate_limit_sample_interval_secs: u64,

    /// Bytes a proxied connection must transfer before it counts as activity
    /// for the inactivity monitor (default: 4096).
    ///
    /// Merely accepting a connection is not activity: uptime monitors and
    /// health checks open a socket on a fixed interval, and counting those
    /// keeps a machine awake forever. A connection only refreshes the idle
    /// timer once this many bytes have moved in either direction. Set to 0 to
    /// restore the old behaviour where every accepted connection counts.
    #[serde(default = "default_activity_min_bytes")]
    pub activity_min_bytes: u64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            check_interval_ms: default_health_check_interval_ms(),
            proxy_connect_timeout_ms: default_proxy_connect_timeout_ms(),
            proxy_wol_wait_secs: default_proxy_wol_wait_secs(),
            system_shutdown_sleep_secs: default_system_shutdown_sleep_secs(),
            rate_limit_sample_interval_secs: default_rate_limit_sample_interval_secs(),
            activity_min_bytes: default_activity_min_bytes(),
        }
    }
}

// Default value functions for serde

fn default_proxy_port() -> u16 {
    3000
}
fn default_client_port() -> u16 {
    3001
}
fn default_health_timeout_secs() -> u64 {
    5
}
fn default_wol_port() -> u16 {
    9
}
fn default_broadcast_ip() -> String {
    "255.255.255.255".into()
}
fn default_wol_packet_count() -> u32 {
    3
}
fn default_wol_packet_sleeptime_ms() -> u64 {
    50
}
fn default_wol_wait_secs() -> u64 {
    90
}
fn default_wol_poll_interval_ms() -> u64 {
    1000
}
fn default_wol_connect_timeout_ms() -> u64 {
    700
}
fn default_network_scan_duration_secs() -> u64 {
    5
}
fn default_network_read_timeout_secs() -> u64 {
    2
}
fn default_machines_db_path() -> String {
    DEFAULT_MACHINES_DB_PATH.into()
}
fn default_health_check_interval_ms() -> u64 {
    30000
}
fn default_proxy_connect_timeout_ms() -> u64 {
    1000
}
fn default_proxy_wol_wait_secs() -> u64 {
    60
}
fn default_system_shutdown_sleep_secs() -> u64 {
    5
}
fn default_rate_limit_sample_interval_secs() -> u64 {
    1
}
fn default_activity_min_bytes() -> u64 {
    4096
}

/// Convenience functions to get commonly used values
#[allow(dead_code)]
impl Config {
    /// Get the default WOL broadcast address as Ipv4Addr
    pub fn get_default_broadcast_addr(&self) -> std::net::Ipv4Addr {
        self.wol
            .default_broadcast_ip
            .parse()
            .unwrap_or_else(|_| std::net::Ipv4Addr::new(255, 255, 255, 255))
    }

    /// Get proxy connect timeout as Duration
    pub fn proxy_connect_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.health.proxy_connect_timeout_ms)
    }

    /// Get WOL packet sleep duration as Duration
    pub fn wol_packet_sleeptime(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.wol.packet_sleeptime_ms)
    }

    /// Get network scan duration as Duration
    pub fn network_scan_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.network.scan_duration_secs)
    }

    /// Get network read timeout as Duration
    pub fn network_read_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.network.read_timeout_secs)
    }

    /// Get health check interval as Duration
    pub fn health_check_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.health.check_interval_ms)
    }

    /// Get system shutdown sleep duration as Duration
    pub fn system_shutdown_sleep_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.health.system_shutdown_sleep_secs)
    }
}

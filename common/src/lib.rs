use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PortForward {
    pub name: Option<String>,
    pub local_port: u16,
    pub target_port: u16,
    /// HTTP paths that never wake a sleeping machine, e.g. a web app's
    /// background polling. A trailing `*` matches a path prefix.
    #[serde(default)]
    pub no_wake_paths: Vec<String>,
    /// Script run on the machine (by its client server) for each new
    /// connection, once the machine is up and before the connection is
    /// forwarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_connect_script: Option<String>,
    /// Script run on the machine (by its client server) when the idle timer
    /// fires, before the machine is turned off. The turn-off waits a grace
    /// period after the script and is cancelled by activity during it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_idle_script: Option<String>,
    /// How the web UI links to the forwarded service.
    #[serde(default, skip_serializing_if = "LinkScheme::is_default")]
    pub link: LinkScheme,
    /// Path the link opens, e.g. `/lab`. Defaults to `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_path: Option<String>,
}

/// Whether and how the web UI links to a forwarded service. Non-web
/// forwards (SSH, databases) should be `Off`: a browser can't open them.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LinkScheme {
    #[default]
    Http,
    Https,
    Off,
}

impl LinkScheme {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl PortForward {
    /// URL of the forwarded service as reached through the wakezilla server
    /// at `host`, the host the browser uses for wakezilla itself. Going
    /// through the forward (rather than to the machine directly) is what
    /// wakes the machine and runs its connect script. `None` for forwards
    /// without a link.
    pub fn link_url(&self, host: &str) -> Option<String> {
        let scheme = match self.link {
            LinkScheme::Http => "http",
            LinkScheme::Https => "https",
            LinkScheme::Off => return None,
        };
        // IPv6 literals need brackets in URLs.
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let path = self
            .link_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or("/");
        let slash = if path.starts_with('/') { "" } else { "/" };
        Some(format!("{scheme}://{host}:{}{slash}{path}", self.local_port))
    }

    /// Link text: "Machine · Service", or "Machine · port N" for an unnamed forward.
    pub fn link_label(&self, machine_name: &str) -> String {
        match self.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) => format!("{machine_name} · {name}"),
            None => format!("{machine_name} · port {}", self.local_port),
        }
    }
}

/// Request from the proxy asking a client server to run a port-forward script.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RunScriptRequest {
    /// Which hook fired: `connect` or `idle`.
    pub event: String,
    pub script: String,
    pub local_port: u16,
    pub target_port: u16,
    /// The client server kills the script after this many seconds.
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RunScriptResponse {
    /// `None` if the script was killed (timeout or signal).
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Machine {
    pub name: String,
    pub mac: String,
    pub ip: String,
    pub description: Option<String>,
    pub turn_off_port: Option<u16>,
    pub can_be_turned_off: bool,
    pub inactivity_period: u32,
    pub port_forwards: Vec<PortForward>,
    /// Idle time in minutes since last proxy activity. Only meaningful
    /// when the machine is online and tracked by the inactivity monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_minutes: Option<u64>,
    /// Minutes since the machine was first detected offline. None when online.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_minutes: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AddMachinePayload {
    pub mac: String,
    pub ip: String,
    pub name: String,
    pub description: Option<String>,
    pub turn_off_port: Option<u16>,
    pub can_be_turned_off: bool,
    pub inactivity_period: Option<u32>,
    pub port_forwards: Option<Vec<PortForward>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpdateMachinePayload {
    pub mac: String,
    pub ip: String,
    pub name: String,
    pub description: Option<String>,
    pub turn_off_port: Option<u16>,
    pub can_be_turned_off: bool,
    pub inactivity_period: Option<u32>,
    pub port_forwards: Option<Vec<PortForward>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeleteMachinePayload {
    pub mac: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct NetworkInterface {
    pub name: String,
    pub ip: String,
    pub mac: String,
    pub is_up: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub ip: String,
    pub mac: String,
    pub hostname: Option<String>,
}

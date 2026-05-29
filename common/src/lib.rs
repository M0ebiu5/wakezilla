use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PortForward {
    pub name: Option<String>,
    pub local_port: u16,
    pub target_port: u16,
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

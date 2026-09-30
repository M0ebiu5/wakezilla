use wakezilla_common::{AddMachinePayload, Machine, PortForward, UpdateMachinePayload};

#[test]
fn machine_deserializes_with_null_port_forward_name() {
    let json = r#"{
        "mac":"AA:BB:CC:DD:EE:FF",
        "ip":"192.168.1.10",
        "name":"desktop",
        "description":null,
        "turn_off_port":8080,
        "can_be_turned_off":true,
        "inactivity_period":30,
        "port_forwards":[{"name":null,"local_port":2222,"target_port":22}]
    }"#;

    let parsed: Machine = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.port_forwards[0].name, None);
}

#[test]
fn update_payload_supports_optional_fields() {
    let payload = UpdateMachinePayload {
        mac: "AA:BB:CC:DD:EE:FF".into(),
        ip: "192.168.1.10".into(),
        name: "desktop".into(),
        description: None,
        turn_off_port: None,
        can_be_turned_off: false,
        inactivity_period: None,
        port_forwards: Some(vec![PortForward {
            name: Some("ssh".into()),
            local_port: 2222,
            target_port: 22,
            no_wake_paths: vec![],
            on_connect_script: None,
            on_idle_script: None,
        }]),
    };

    let _json = serde_json::to_string(&payload).unwrap();
}

#[test]
fn add_payload_supports_optional_fields() {
    let payload = AddMachinePayload {
        mac: "AA:BB:CC:DD:EE:FF".into(),
        ip: "192.168.1.10".into(),
        name: "desktop".into(),
        description: None,
        turn_off_port: None,
        can_be_turned_off: false,
        inactivity_period: None,
        port_forwards: None,
    };

    let _json = serde_json::to_string(&payload).unwrap();
}

use wakezilla::web::{api_port_forward_to_internal, machine_to_api_machine, Machine};

#[test]
fn api_port_forward_allows_missing_name() {
    let pf = wakezilla::PortForward {
        name: None,
        local_port: 2222,
        target_port: 22,
        no_wake_paths: vec![],
    };

    let internal = api_port_forward_to_internal(&pf);
    assert_eq!(internal.name, "");
}

#[test]
fn machine_maps_to_api_shape() {
    let machine = Machine {
        mac: "AA:BB:CC:DD:EE:FF".into(),
        ip: "192.168.1.10".parse().unwrap(),
        name: "Desktop".into(),
        description: None,
        turn_off_port: Some(3001),
        can_be_turned_off: true,
        inactivity_period: 30,
        port_forwards: vec![],
    };

    let api = machine_to_api_machine(&machine, None);
    assert_eq!(api.ip, "192.168.1.10");
    assert_eq!(api.turn_off_port, Some(3001));
}

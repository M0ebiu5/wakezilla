use wakezilla::web::{self, Machine, PortForward};

struct EnvGuard {
    key: &'static str,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        std::env::set_var(key, value);
        Self { key }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.key);
    }
}

#[test]
fn save_and_load_machines_round_trip() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp directory");
    let db_path = temp_dir.path().join("machines.json");
    let _guard = EnvGuard::set(
        "WAKEZILLA__STORAGE__MACHINES_DB_PATH",
        db_path.to_str().expect("temp path should be valid utf-8"),
    );

    let machines = vec![Machine {
        mac: "AA:BB:CC:DD:EE:FF".into(),
        ip: "192.168.1.10".parse().unwrap(),
        name: "Desktop".into(),
        description: Some("Main desktop".into()),
        turn_off_port: Some(4000),
        can_be_turned_off: true,
        inactivity_period: 15,
        port_forwards: vec![PortForward {
            name: "SSH".into(),
            local_port: 2222,
            target_port: 22,
            no_wake_paths: vec![],
        }],
    }];

    web::save_machines(&machines).expect("failed to save machines");

    let loaded = web::load_machines().expect("failed to load machines");

    assert_eq!(loaded.len(), 1);
    let loaded_machine = &loaded[0];
    let original = &machines[0];

    assert_eq!(loaded_machine.mac, original.mac);
    assert_eq!(loaded_machine.ip, original.ip);
    assert_eq!(loaded_machine.name, original.name);
    assert_eq!(loaded_machine.description, original.description);
    assert_eq!(loaded_machine.turn_off_port, original.turn_off_port);
    assert_eq!(loaded_machine.can_be_turned_off, original.can_be_turned_off);
    assert_eq!(loaded_machine.inactivity_period, original.inactivity_period);
    assert_eq!(loaded_machine.port_forwards.len(), 1);
    let loaded_pf = &loaded_machine.port_forwards[0];
    let original_pf = &original.port_forwards[0];
    assert_eq!(loaded_pf.name, original_pf.name);
    assert_eq!(loaded_pf.local_port, original_pf.local_port);
    assert_eq!(loaded_pf.target_port, original_pf.target_port);
}

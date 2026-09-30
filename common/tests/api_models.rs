use wakezilla_common::{AddMachinePayload, LinkScheme, Machine, PortForward, UpdateMachinePayload};

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
            link: Default::default(),
            link_path: None,
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

fn web_forward(name: Option<&str>, link: LinkScheme, link_path: Option<&str>) -> PortForward {
    PortForward {
        name: name.map(Into::into),
        local_port: 3002,
        target_port: 3000,
        no_wake_paths: vec![],
        on_connect_script: None,
        on_idle_script: None,
        link,
        link_path: link_path.map(Into::into),
    }
}

#[test]
fn link_url_uses_the_wakezilla_host_and_local_port() {
    let pf = web_forward(Some("webui"), LinkScheme::Http, None);
    assert_eq!(pf.link_url("192.168.4.221").as_deref(), Some("http://192.168.4.221:3002/"));
    assert_eq!(pf.link_url("fd00::1").as_deref(), Some("http://[fd00::1]:3002/"));

    let pf = web_forward(Some("jupyter"), LinkScheme::Https, Some("lab"));
    assert_eq!(pf.link_url("server.lan").as_deref(), Some("https://server.lan:3002/lab"));

    let pf = web_forward(Some("ssh"), LinkScheme::Off, None);
    assert_eq!(pf.link_url("server.lan"), None);
}

#[test]
fn link_label_names_machine_and_service() {
    let named = web_forward(Some("comfyui"), LinkScheme::Http, None);
    assert_eq!(named.link_label("White"), "White · comfyui");
    let unnamed = web_forward(Some("  "), LinkScheme::Http, None);
    assert_eq!(unnamed.link_label("White"), "White · port 3002");
}

#[test]
fn link_settings_are_optional_in_json() {
    let pf: PortForward =
        serde_json::from_str(r#"{"name":"webui","local_port":3002,"target_port":3000}"#).unwrap();
    assert_eq!(pf.link, LinkScheme::Http);
    assert_eq!(pf.link_path, None);
    // Defaults are left out, so existing configs don't change on save.
    let json = serde_json::to_string(&pf).unwrap();
    assert!(!json.contains("link"), "{json}");

    let off: PortForward = serde_json::from_str(
        r#"{"name":"ssh","local_port":2222,"target_port":22,"link":"off"}"#,
    )
    .unwrap();
    assert_eq!(off.link, LinkScheme::Off);
}

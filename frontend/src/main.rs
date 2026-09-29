use leptos::prelude::*;
use std::collections::HashMap;

use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;
use web_sys::window;

pub mod api;
pub mod models;
use leptos::leptos_dom::logging::console_log;
use leptos_meta::*;
use leptos_router::{
    components::{Route, Router, Routes},
    hooks::use_params_map,
    path,
};

use web_sys::{SubmitEvent, console};

use crate::api::{
    create_machine, delete_machine, fetch_interfaces, fetch_machines, fetch_scan_network,
    get_details_machine, is_machine_online, turn_off_machine, wake_machine,
};
use crate::models::{
    DiscoveredDevice, Machine, NetworkInterface, PortForward, UpdateMachinePayload,
    validate_machine_form,
};

/// How often the dashboard re-checks whether each machine is reachable.
///
/// Status is polled rather than pushed, so without this the indicators would
/// keep showing whatever was true at page load: a machine that booted after a
/// wake (or was shut down by the idle monitor) would still read as it did then.
const STATUS_REFRESH_SECS: u64 = 10;

#[component]
pub fn ErrorDisplay(
    erros: ReadSignal<HashMap<String, Vec<String>>>,
    key: &'static str,
) -> impl IntoView {
    view! {
        {move || {
            if erros.get().contains_key(key) {
                let error_messages = erros.get().get(key).cloned().unwrap_or_default();
                Some(
                    view! {
                        <div class="error-message">
                            <For
                                each=move || error_messages.clone().into_iter()
                                key=|msg| msg.clone()
                                children=move |msg| {
                                    view! { <p>{msg}</p> }
                                }
                            />
                        </div>
                    },
                )
            } else {
                None
            }
        }}
    }
}
// Components
#[component]
fn MachineDetailPage() -> impl IntoView {
    let params = use_params_map();
    let mac = move || params.read().get("mac").unwrap_or_default();
    let (loading, set_loading) = signal(false);
    let (machine_details, set_machine_details) = signal::<Machine>(Machine {
        name: "".to_string(),
        mac: "".to_string(),
        ip: "".to_string(),
        description: None,
        turn_off_port: None,
        can_be_turned_off: false,
        inactivity_period: 60,
        port_forwards: vec![],
        idle_minutes: None,
        offline_minutes: None,
    });

    // Load initial machine details
    Effect::new(move || {
        leptos::task::spawn_local(async move {
            if let Ok(cats) = get_details_machine(&mac()).await {
                set_machine_details.set(cats);
            }
        });
    });

    // Form state
    let (name, set_name) = signal(String::new());
    let (ip, set_ip) = signal(String::new());
    let (description, set_description) = signal(String::new());
    let (turn_off_port, set_turn_off_port) = signal::<Option<u16>>(None);
    let (can_be_turned_off, set_can_be_turned_off) = signal(false);
    let (port_forwards, set_port_forwards) = signal::<Vec<PortForward>>(vec![]);
    let (inactivity_period, set_inactivity_period) = signal(60u32);
    let (turn_off_loading, set_turn_off_loading) = signal(false);
    let (turn_off_feedback, set_turn_off_feedback) = signal::<Option<(bool, String)>>(None);
    let (wake_loading, set_wake_loading) = signal(false);
    let (wake_feedback, set_wake_feedback) = signal::<Option<(bool, String)>>(None);
    let (turn_off_port_error, set_turn_off_port_error) = signal::<Option<String>>(None);

    let can_turn_off_machine = Memo::new(move |_| {
        let machine = machine_details.get();
        machine.can_be_turned_off
    });

    // Update form fields when machine details load
    Effect::new(move || {
        let machine = machine_details.get();
        set_name.set(machine.name.clone());
        set_ip.set(machine.ip.clone());
        set_description.set(machine.description.clone().unwrap_or_default());
        set_turn_off_port.set(machine.turn_off_port); // This should now match the type
        set_can_be_turned_off.set(machine.can_be_turned_off);
        set_port_forwards.set(machine.port_forwards.clone());
        set_inactivity_period.set(machine.inactivity_period);
    });

    let update_machine = move |ev: SubmitEvent| {
        ev.prevent_default();

        if can_be_turned_off.get() && turn_off_port.get().is_none() {
            set_turn_off_port_error.set(Some(
                "A port number is required when remote turn off is enabled. \
                 This is the port Wakezilla will connect to on the target machine \
                 to send the shutdown command. The machine must be running the \
                 Wakezilla client and have it listening on this port."
                    .to_string(),
            ));
            return;
        }
        set_turn_off_port_error.set(None);
        set_loading.set(true);

        let updated_mac = mac();
        let updated_name = name.get();
        let updated_ip = ip.get();
        let updated_description = if description.get().trim().is_empty() {
            None
        } else {
            Some(description.get())
        };
        let updated_turn_off_port = if can_be_turned_off.get() {
            turn_off_port.get()
        } else {
            None
        };
        let updated_can_be_turned_off = can_be_turned_off.get();
        let updated_port_forwards = port_forwards.get();

        // Create updated machine object for local state refresh
        let updated_machine = Machine {
            name: updated_name,
            mac: updated_mac.clone(),
            ip: updated_ip,
            description: updated_description,
            turn_off_port: updated_turn_off_port,
            can_be_turned_off: updated_can_be_turned_off,
            inactivity_period: inactivity_period.get(),
            port_forwards: updated_port_forwards.clone(),
            idle_minutes: None,
            offline_minutes: None,
        };

        let payload = UpdateMachinePayload {
            mac: updated_machine.mac.clone(),
            ip: updated_machine.ip.clone(),
            name: updated_machine.name.clone(),
            description: updated_machine.description.clone(),
            turn_off_port: updated_machine.turn_off_port,
            can_be_turned_off: updated_machine.can_be_turned_off,
            inactivity_period: Some(updated_machine.inactivity_period),
            port_forwards: Some(updated_machine.port_forwards),
        };

        leptos::task::spawn_local(async move {
            match crate::api::update_machine(&updated_mac, &payload).await {
                Ok(_) => {
                    web_sys::console::log_1(&"Machine updated successfully".into());
                    // Reload the machine details to reflect changes
                    if let Ok(updated_details) = get_details_machine(&updated_mac).await {
                        set_machine_details.set(updated_details);
                    }
                    window()
                        .unwrap()
                        .alert_with_message("Machine updated successfully!")
                        .unwrap();
                }
                Err(e) => {
                    web_sys::console::log_1(&format!("Error updating machine: {}", e).into());
                    window()
                        .unwrap()
                        .alert_with_message(&format!("Error updating machine: {}", e))
                        .unwrap();
                }
            }
            set_loading.set(false);
        });
    };

    let trigger_turn_off = move |_| {
        if !can_turn_off_machine.get() || turn_off_loading.get() {
            return;
        }

        let mac_address = mac();
        set_turn_off_loading.set(true);
        set_turn_off_feedback.set(None);

        let set_turn_off_loading = set_turn_off_loading;
        let set_turn_off_feedback = set_turn_off_feedback;

        leptos::task::spawn_local(async move {
            match turn_off_machine(&mac_address).await {
                Ok(message) => {
                    set_turn_off_feedback.set(Some((true, message.clone())));
                    if let Some(window) = window() {
                        let _ = window.alert_with_message(&message);
                    }
                }
                Err(message) => {
                    set_turn_off_feedback.set(Some((false, message.clone())));
                    if let Some(window) = window() {
                        let _ = window.alert_with_message(&format!(
                            "Failed to turn off machine: {}",
                            message
                        ));
                    }
                }
            }
            set_turn_off_loading.set(false);
        });
    };

    let trigger_wake = move |_| {
        if wake_loading.get() {
            return;
        }

        let mac_address = mac();
        set_wake_loading.set(true);
        set_wake_feedback.set(None);

        let set_wake_loading = set_wake_loading;
        let set_wake_feedback = set_wake_feedback;

        leptos::task::spawn_local(async move {
            match wake_machine(&mac_address).await {
                Ok(message) => {
                    set_wake_feedback.set(Some((true, message.clone())));
                    if let Some(window) = window() {
                        let _ = window.alert_with_message(&message);
                    }
                }
                Err(message) => {
                    set_wake_feedback.set(Some((false, message.clone())));
                    if let Some(window) = window() {
                        let _ = window
                            .alert_with_message(&format!("Failed to wake machine: {}", message));
                    }
                }
            }
            set_wake_loading.set(false);
        });
    };

    view! {
        <div class="page-stack">
            <a class="back-link" href="/">
                <span aria-hidden="true">"←"</span>
                <span>"Back to dashboard"</span>
            </a>

            <div class="card">
                <header class="card-header">
                    <h2 class="card-title">
                        {move || {
                            let current_name = name.get();
                            if current_name.trim().is_empty() {
                                "Machine Overview".to_string()
                            } else {
                                current_name
                            }
                        }}
                    </h2>
                    <p class="card-subtitle">
                        {move || format!("MAC {}", machine_details.get().mac)}
                    </p>
                </header>

                <form on:submit=update_machine class="form-grid">
                    <div class="form-grid two-column">
                        <div class="field">
                            <label for="name">"Name"</label>
                            <input
                                type="text"
                                id="name"
                                name="name"
                                class="input"
                                required
                                value=move || name.get()
                                on:input=move |ev| {
                                    let target = ev.target().unwrap();
                                    let input: HtmlInputElement = target.dyn_into().unwrap();
                                    set_name.set(input.value());
                                }
                            />
                        </div>
                        <div class="field">
                            <label for="ip">"IP address"</label>
                            <input
                                type="text"
                                id="ip"
                                name="ip"
                                class="input"
                                required
                                value=move || ip.get()
                                on:input=move |ev| {
                                    let target = ev.target().unwrap();
                                    let input: HtmlInputElement = target.dyn_into().unwrap();
                                    set_ip.set(input.value());
                                }
                            />
                        </div>
                    </div>

                    <div class="field">
                        <label for="description">"Description"</label>
                        <input
                            type="text"
                            id="description"
                            name="description"
                            class="input"
                            value=move || description.get()
                            on:input=move |ev| {
                                let target = ev.target().unwrap();
                                let input: HtmlInputElement = target.dyn_into().unwrap();
                                set_description.set(input.value());
                            }
                        />
                        <p class="field-help">
                            "Optional label to help the team recognise this machine."
                        </p>
                    </div>

                    <div class="field field-toggle">
                        <input
                            type="checkbox"
                            id="can_be_turned_off"
                            name="can_be_turned_off"
                            class="checkbox"
                            checked=move || can_be_turned_off.get()
                            on:change=move |ev| {
                                let target = ev.target().unwrap();
                                let input: HtmlInputElement = target.dyn_into().unwrap();
                                set_can_be_turned_off.set(input.checked());
                            }
                        />
                        <div class="field-toggle__content">
                            <label for="can_be_turned_off">"Enable remote turn off"</label>
                            <p class="field-help">
                                "Requires an accessible shutdown endpoint on the machine."
                            </p>
                        </div>
                    </div>

                    <Show when=move || can_be_turned_off.get() fallback=|| view! { <></> }>
                        <div class="field">
                            <label for="turn_off_port">"Turn off port"</label>
                            <input
                                type="number"
                                id="turn_off_port"
                                name="turn_off_port"
                                class="input"
                                min="1"
                                max="65535"
                                placeholder="3001"
                                value=move || {
                                    turn_off_port.get().map(|p| p.to_string()).unwrap_or_default()
                                }
                                on:input=move |ev| {
                                    let target = ev.target().unwrap();
                                    let input: HtmlInputElement = target.dyn_into().unwrap();
                                    let value = input.value();
                                    set_turn_off_port.set(value.parse().ok());
                                    set_turn_off_port_error.set(None);
                                }
                            />
                            {move || turn_off_port_error.get().map(|msg| view! {
                                <p class="error-message">{msg}</p>
                            })}
                            <p class="field-help">
                                "Port exposed by the machine to receive shutdown requests."
                            </p>
                        </div>
                    </Show>

                    <div class="field">
                        <div class="field-header">
                            <label>"Port forwards"</label>
                            <button
                                type="button"
                                class="btn btn-soft btn-sm"
                                on:click=move |_| {
                                    set_port_forwards
                                        .update(|pfs| {
                                            pfs.push(PortForward {
                                                name: None,
                                                local_port: 0,
                                                target_port: 0,
                                            });
                                        });
                                }
                            >
                                "+ Add port"
                            </button>
                        </div>
                        <p class="field-help">
                            "Start lightweight TCP tunnels when this machine is online."
                        </p>
                        <Show
                            when=move || !port_forwards.get().is_empty()
                            fallback=|| {
                                view! {
                                    <p class="field-empty">"No port forwards configured yet."</p>
                                }
                            }
                        >
                            <div class="port-forward-list">
                                <For
                                    each=move || {
                                        port_forwards
                                            .get()
                                            .into_iter()
                                            .enumerate()
                                            .collect::<Vec<(usize, PortForward)>>()
                                    }
                                    key=|(idx, _)| *idx
                                    children=move |(idx, _port_forward)| {
                                        let row_number = idx + 1;
                                        let name_id = format!("pf-name-{}", row_number);
                                        let local_id = format!("pf-local-{}", row_number);
                                        let target_id = format!("pf-target-{}", row_number);
                                        let name_label = format!("Service name {}", row_number);
                                        let local_label = format!("Local port {}", row_number);
                                        let target_label = format!(
                                            "Forward to port {}",
                                            row_number,
                                        );
                                        let forward_label = format!("Forward {}", row_number);

                                        view! {
                                            <div class="port-forward-item">
                                                <div class="port-forward-item__header">
                                                    <span class="port-forward-item__title">
                                                        {forward_label}
                                                    </span>
                                                    <button
                                                        type="button"
                                                        class="btn btn-ghost btn-sm port-forward-item__remove"
                                                        on:click=move |_| {
                                                            set_port_forwards
                                                                .update(|pfs| {
                                                                    if idx < pfs.len() {
                                                                        pfs.remove(idx);
                                                                    }
                                                                });
                                                        }
                                                    >
                                                        "Remove"
                                                    </button>
                                                </div>
                                                <div class="port-forward-item__grid">
                                                    <div class="field">
                                                        <label for=name_id.clone()>{name_label.clone()}</label>
                                                        <input
                                                            class="input"
                                                            id=name_id
                                                            placeholder="Service name"
                                                            value=move || {
                                                                port_forwards
                                                                    .get()
                                                                    .get(idx)
                                                                    .and_then(|pf| pf.name.clone())
                                                                    .unwrap_or_default()
                                                            }
                                                            on:input=move |ev| {
                                                                let target = ev.target().unwrap();
                                                                let input: HtmlInputElement = target.dyn_into().unwrap();
                                                                let value = input.value();
                                                                let trimmed = value.trim().is_empty();
                                                                set_port_forwards
                                                                    .update(|pfs| {
                                                                        if let Some(pf) = pfs.get_mut(idx) {
                                                                            pf.name = if trimmed { None } else { Some(value.clone()) };
                                                                        }
                                                                    });
                                                            }
                                                        />
                                                    </div>
                                                    <div class="field">
                                                        <label for=local_id.clone()>{local_label.clone()}</label>
                                                        <input
                                                            class="input"
                                                            id=local_id
                                                            placeholder="Local port"
                                                            type="number"
                                                            min="0"
                                                            max="65535"
                                                            value=move || {
                                                                port_forwards
                                                                    .get()
                                                                    .get(idx)
                                                                    .map(|pf| pf.local_port.to_string())
                                                                    .unwrap_or_default()
                                                            }
                                                            on:input=move |ev| {
                                                                let target = ev.target().unwrap();
                                                                let input: HtmlInputElement = target.dyn_into().unwrap();
                                                                let value = input.value();
                                                                let parsed = value.parse::<u16>().unwrap_or(0);
                                                                set_port_forwards
                                                                    .update(|pfs| {
                                                                        if let Some(pf) = pfs.get_mut(idx) {
                                                                            pf.local_port = parsed;
                                                                        }
                                                                    });
                                                            }
                                                        />
                                                    </div>
                                                    <div class="field">
                                                        <label for=target_id.clone()>{target_label.clone()}</label>
                                                        <input
                                                            class="input"
                                                            id=target_id
                                                            placeholder="Target port"
                                                            type="number"
                                                            min="0"
                                                            max="65535"
                                                            value=move || {
                                                                port_forwards
                                                                    .get()
                                                                    .get(idx)
                                                                    .map(|pf| pf.target_port.to_string())
                                                                    .unwrap_or_default()
                                                            }
                                                            on:input=move |ev| {
                                                                let target = ev.target().unwrap();
                                                                let input: HtmlInputElement = target.dyn_into().unwrap();
                                                                let value = input.value();
                                                                let parsed = value.parse::<u16>().unwrap_or(0);
                                                                set_port_forwards
                                                                    .update(|pfs| {
                                                                        if let Some(pf) = pfs.get_mut(idx) {
                                                                            pf.target_port = parsed;
                                                                        }
                                                                    });
                                                            }
                                                        />
                                                    </div>
                                                </div>
                                            </div>
                                        }
                                    }
                                />
                            </div>
                        </Show>
                    </div>

                    <div class="field">
                        <label for="inactivity_period">"Inactivity Period (minutes)"</label>
                        <input
                            type="number"
                            id="inactivity_period"
                            name="inactivity_period"
                            class="input"
                            min="1"
                            value=move || inactivity_period.get().to_string()
                            on:input=move |ev| {
                                let target = ev.target().unwrap();
                                let input: HtmlInputElement = target.dyn_into().unwrap();
                                if let Ok(value) = input.value().parse() {
                                    set_inactivity_period.set(value);
                                }
                            }
                        />
                    </div>

                    <div class="form-footer">
                        <button
                            type="submit"
                            class="btn btn-primary"
                            disabled=move || loading.get()
                        >
                            {move || if loading.get() { "Saving..." } else { "Save changes" }}
                        </button>
                    </div>
                </form>
            </div>

            <div class="card card-actions">
                <header class="card-header">
                    <h3 class="card-title">"Remote controls"</h3>
                    <p class="card-subtitle">"Send wake and shutdown signals instantly."</p>
                </header>
                <div class="actions-row">
                    <button
                        type="button"
                        class="btn btn-success"
                        on:click=trigger_wake
                        disabled=move || wake_loading.get()
                    >
                        {move || if wake_loading.get() { "Waking..." } else { "Wake machine" }}
                    </button>
                    <button
                        type="button"
                        class="btn btn-danger"
                        on:click=trigger_turn_off
                        disabled=move || turn_off_loading.get() || !can_turn_off_machine.get()
                    >
                        {move || {
                            if turn_off_loading.get() {
                                "Turning off..."
                            } else {
                                "Turn off machine"
                            }
                        }}
                    </button>
                </div>
                {move || {
                    if let Some((success, message)) = wake_feedback.get() {
                        let class = if success {
                            "feedback feedback--success"
                        } else {
                            "feedback feedback--danger"
                        }
                            .to_string();
                        view! { <p class=class>{message}</p> }
                    } else {
                        let class = "feedback feedback--hidden".to_string();
                        let empty = String::new();
                        view! { <p class=class>{empty}</p> }
                    }
                }}
                {move || {
                    if let Some((success, message)) = turn_off_feedback.get() {
                        let class = if success {
                            "feedback feedback--success"
                        } else {
                            "feedback feedback--danger"
                        }
                            .to_string();
                        view! { <p class=class>{message}</p> }
                    } else {
                        let class = "feedback feedback--hidden".to_string();
                        let empty = String::new();
                        view! { <p class=class>{empty}</p> }
                    }
                }}
                <Show when=move || !can_turn_off_machine.get() fallback=|| view! { <></> }>
                    <p class="field-help">
                        "Enable remote turn off in the settings to activate this action."
                    </p>
                </Show>
            </div>

            <div class="card">
                <header class="card-header">
                    <h3 class="card-title">"Raw machine data"</h3>
                    <p class="card-subtitle">"Debug snapshot of the API payload."</p>
                </header>
                <pre class="code-block">
                    {move || {
                        serde_json::to_string_pretty(&machine_details.get()).unwrap_or_default()
                    }}
                </pre>
            </div>
        </div>
    }
}

#[component]
fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Html attr:lang="en" />
        <Stylesheet id="leptos" href="/style/main.css" />
        <Title text="Wakezilla" />
        <Router>
            <main class="container">
                <Routes fallback=|| "Page not found">
                    <Route path=path!("/") view=HomePage />
                    <Route path=path!("/machines/:mac") view=MachineDetailPage />
                </Routes>
            </main>
        </Router>
    }
}

#[component]
fn Header(
    set_machine: WriteSignal<Machine>,
    registred_machines: ReadSignal<Vec<Machine>>,
) -> impl IntoView {
    let (discovered_devices, set_discovered_devices) = signal::<Vec<DiscoveredDevice>>(vec![]);
    let (interfaces, set_interfaces) = signal::<Vec<NetworkInterface>>(vec![]);
    let (interface, set_interface) = signal::<String>("".to_string());
    let (loading, set_loading) = signal(false);

    // Load initial data
    Effect::new(move || {
        leptos::task::spawn_local(async move {
            if let Ok(cats) = fetch_interfaces().await {
                set_interfaces.set(cats);
            }
        });
    });

    fn handle_interface_change(value: String, set_interface: WriteSignal<String>) {
        let log_mesasge = format!("Selected interface: {}", value);
        console_log(&log_mesasge);
        set_interface.set(value);
    }

    let on_submit = move |ev: SubmitEvent| {
        let set_loading = set_loading;
        set_loading.set(true);
        set_discovered_devices.set(vec![]);
        // stop the page from reloading!
        ev.prevent_default();
        console::log_1(&format!("Form submitted with value: {}", interface.get()).into());
        leptos::task::spawn_local(async move {
            fetch_scan_network(interface.get())
                .await
                .map(|devices| {
                    console::log_1(&format!("Discovered devices: {:?}", devices).into());
                    // does not diplay the machine if it's already registred
                    let registred_machines = registred_machines.get();
                    let devices: Vec<DiscoveredDevice> = devices
                        .into_iter()
                        .filter(|device| {
                            !registred_machines
                                .iter()
                                .any(|machine| machine.mac == device.mac)
                        })
                        .collect();

                    set_discovered_devices.set(devices);
                })
                .unwrap_or_else(|err| {
                    window()
                        .unwrap()
                        .alert_with_message("Error scanning network, check the logs in the backend")
                        .unwrap();
                    console::log_1(&format!("Error scanning network: {}", err).into());
                });

            set_loading.set(false);
        });
    };

    fn handle_add_machine(
        device: DiscoveredDevice,
        set_machine: WriteSignal<Machine>,
        set_discovered_devices: WriteSignal<Vec<DiscoveredDevice>>,
    ) {
        let new_machine = Machine {
            name: device.hostname.clone().unwrap_or_default(),
            mac: device.mac.clone(),
            ip: device.ip.clone(),
            description: None,
            turn_off_port: Some(3000),
            can_be_turned_off: false,
            inactivity_period: 60,
            port_forwards: vec![PortForward {
                name: None,
                local_port: 0,
                target_port: 0,
            }],
            idle_minutes: None,
            offline_minutes: None,
        };
        set_machine.set(new_machine);
        set_discovered_devices.set(vec![]);
    }

    view! {
        <div class="section-stack">
            <div class="card scan-card">
                <header class="card-header card-header--with-logo">
                    <div class="card-header__text">
                        <div style="display: flex; align-items: center; gap: 10px;">
                            <img
                                src="/images/wakezilla.png"
                                alt="Wakezilla logo"
                                class="card-header__logo"
                            />
                            <h1 class="">"Wakezilla"</h1>
                        </div>
                        <p class="card-subtitle">
                            "Wake, manage, and forward to your registered machines."
                        </p>
                    </div>
                </header>
                <form on:submit=on_submit class="scan-grid">
                    <select
                        id="interface-select"
                        class="input"
                        on:change:target=move |ev| {
                            handle_interface_change(ev.target().value(), set_interface);
                        }
                        prop:value=move || interface.get().to_string()
                    >
                        <option value="">"Auto-detect interface"</option>
                        {move || {
                            interfaces
                                .get()
                                .iter()
                                .map(|iface| {
                                    view! {
                                        <option value=iface
                                            .name
                                            .clone()>
                                            {format!("{} · {} ({})", iface.name, iface.ip, iface.mac)}
                                        </option>
                                    }
                                })
                                .collect::<Vec<_>>()
                        }}
                    </select>
                    <button id="scan-btn" class="btn btn-primary" disabled=move || loading.get()>
                        {move || { if loading.get() { "Scanning…" } else { "Scan network" } }}
                    </button>
                </form>
            </div>

            <Show when=move || { !discovered_devices.get().is_empty() } fallback=|| view! { <></> }>
                <div class="card table-card" id="scan-results-container">
                    <div class="card-header">
                        <h3 class="card-title">"Discovered devices"</h3>
                        <p class="card-subtitle">
                            "Tap a device to pre-fill the create form below."
                        </p>
                    </div>
                    <div class="table-container">
                        <table class="table" id="scan-results-table">
                            <thead>
                                <tr>
                                    <th>"IP address"</th>
                                    <th>"Hostname"</th>
                                    <th>"MAC address"</th>
                                    <th>"Action"</th>
                                </tr>
                            </thead>
                            <tbody>
                                <For
                                    each=move || discovered_devices.get()
                                    key=|device| device.ip.clone()
                                    children=move |device| {
                                        view! {
                                            <tr>
                                                <td attr:data-label="IP address">{device.ip.clone()}</td>
                                                <td attr:data-label="Hostname">
                                                    {device
                                                        .hostname
                                                        .clone()
                                                        .unwrap_or_else(|| "N/A".to_string())}
                                                </td>
                                                <td attr:data-label="MAC address">{device.mac.clone()}</td>
                                                <td attr:data-label="Action" class="table-actions">
                                                    <button
                                                        class="btn-icon btn-icon--positive"
                                                        title="Use this device"
                                                        on:click=move |_| {
                                                            handle_add_machine(
                                                                device.clone(),
                                                                set_machine,
                                                                set_discovered_devices,
                                                            );
                                                        }
                                                    >
                                                        "＋"
                                                    </button>
                                                </td>
                                            </tr>
                                        }
                                    }
                                />
                            </tbody>
                        </table>
                    </div>
                </div>
            </Show>
        </div>
    }
}

#[component]
fn RegistredMachines(
    machines: ReadSignal<Vec<Machine>>,
    status_machine: ReadSignal<HashMap<String, bool>>,
    set_registred_machines: WriteSignal<Vec<Machine>>,
) -> impl IntoView {
    let (wake_in_progress, set_wake_in_progress) = signal::<Option<String>>(None);
    let (turn_off_in_progress, set_turn_off_in_progress) = signal::<Option<String>>(None);

    let on_delete = move |mac_to_delete: String| {
        leptos::task::spawn_local(async move {
            // Call the API to delete the machine
            if let Err(err) = delete_machine(&mac_to_delete).await {
                console_log(&format!(
                    "Error deleting machine {}: {}",
                    mac_to_delete, err
                ));
                window()
                    .unwrap()
                    .alert_with_message(&format!("Error deleting machine: {}", err))
                    .unwrap();
                return;
            }

            // Remove the machine from the local state
            let current_machines = machines.get();
            let filtered_machines: Vec<Machine> = current_machines
                .into_iter()
                .filter(|m| m.mac != mac_to_delete)
                .collect();

            set_registred_machines.set(filtered_machines);
            console_log(&format!("Successfully deleted machine: {}", mac_to_delete));
        });
    };

    view! {
        <section class="card table-card">
            <div class="card-header">
                <div>
                    <h2 class="card-title">"Registered machines"</h2>
                    <p class="card-subtitle">
                        {move || {
                            let count = machines.get().len();
                            if count == 0 {
                                "No machines registered yet.".to_string()
                            } else if count == 1 {
                                "1 machine online or ready".to_string()
                            } else {
                                format!("{} machines online or ready", count)
                            }
                        }}
                    </p>
                </div>
            </div>
            <div class="table-container">
                <table class="table">
                    <thead>
                        <tr>
                            <th>"Name"</th>
                            <th class="hide-mobile">"MAC"</th>
                            <th class="hide-mobile">"IP"</th>
                            <th class="hide-mobile">"Description"</th>
                            <th class="hide-mobile">"Port"</th>
                            <th class="hide-mobile">"Turn Off"</th>
                            <th>"Status"</th>
                            <th class="hide-mobile">"Idle"</th>
                            <th class="hide-mobile">"Forwards"</th>
                            <th>"Actions"</th>
                        </tr>
                    </thead>
                    <tbody>
                        <Show
                            when=move || !machines.get().is_empty()
                            fallback=|| {
                                view! {
                                    <tr>
                                        <td colspan=10 class="table-empty">
                                            "No machines yet. Use the form below to add one."
                                        </td>
                                    </tr>
                                }
                            }
                        >
                            <For
                                each=move || machines.get()
                                key=|machine| machine.mac.clone()
                                children=move |machine| {
                                    let mac_href = machine.mac.clone();
                                    let mac_display = mac_href.clone();
                                    let ip_display = machine.ip.clone();
                                    let description_display = machine
                                        .description
                                        .clone()
                                        .unwrap_or_else(|| "-".to_string());
                                    let status_mac = mac_href.clone();
                                    let idle_mac = mac_href.clone();
                                    let wake_mac_disabled = mac_href.clone();
                                    let wake_mac_click = mac_href.clone();
                                    let wake_mac_task = mac_href.clone();
                                    let turn_off_mac_disabled = mac_href.clone();
                                    let turn_off_mac_click = mac_href.clone();
                                    let turn_off_mac_task = mac_href.clone();
                                    let turn_off_mac_label = mac_href.clone();
                                    let delete_mac = mac_href.clone();
                                    let name_link = machine.name.clone();
                                    let name_for_wake = machine.name.clone();
                                    let name_for_turnoff = machine.name.clone();
                                    let name_for_confirm = machine.name.clone();
                                    let set_wake_in_progress_btn = set_wake_in_progress;
                                    let wake_in_progress_for_disable = wake_in_progress;
                                    let wake_in_progress_for_click = wake_in_progress;
                                    let set_turn_off_in_progress_btn = set_turn_off_in_progress;
                                    let turn_off_in_progress_for_disable = turn_off_in_progress;
                                    let turn_off_in_progress_for_click = turn_off_in_progress;
                                    let turn_off_in_progress_for_label = turn_off_in_progress;
                                    let can_turn_off_machine = machine.can_be_turned_off
                                        && machine.turn_off_port.is_some();
                                    let turn_off_port_text = machine
                                        .turn_off_port
                                        .map(|port| port.to_string())
                                        .unwrap_or_else(|| "-".to_string());
                                    let can_turn_off_text = if machine.can_be_turned_off {
                                        "Yes".to_string()
                                    } else {
                                        "No".to_string()
                                    };
                                    let port_forwards_text = if machine.port_forwards.is_empty() {
                                        "-".to_string()
                                    } else {
                                        machine
                                            .port_forwards
                                            .iter()
                                            .map(|pf| {
                                                let pf_name = pf
                                                    .name
                                                    .clone()
                                                    .unwrap_or_else(|| "-".to_string());
                                                format!(
                                                    "{} → {} ({})",
                                                    pf.local_port,
                                                    pf.target_port,
                                                    pf_name,
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    };
                                    let mobile_port_forward_labels: Vec<String> = if machine
                                        .port_forwards
                                        .is_empty()
                                    {
                                        vec!["-".to_string()]
                                    } else {
                                        machine
                                            .port_forwards
                                            .iter()
                                            .map(|pf| {
                                                let pf_name = pf
                                                    .name
                                                    .clone()
                                                    .unwrap_or_else(|| "-".to_string());
                                                format!(
                                                    "{} → {} ({})",
                                                    pf.local_port,
                                                    pf.target_port,
                                                    pf_name,
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    };

                                    view! {
                                        <tr>
                                            <td>
                                                <a
                                                    class="text-link"
                                                    href=format!("/machines/{}", mac_href.clone())
                                                >
                                                    {name_link}
                                                </a>
                                            </td>
                                            <td class="hide-mobile">
                                                <code>{mac_display.clone()}</code>
                                            </td>
                                            <td class="hide-mobile">
                                                <code>{ip_display.clone()}</code>
                                            </td>
                                            <td class="hide-mobile">{description_display.clone()}</td>
                                            <td class="hide-mobile">
                                                <span class="font-mono text-xs sm:text-sm">
                                                    {move || turn_off_port_text.clone()}
                                                </span>
                                            </td>
                                            <td class="hide-mobile">
                                                <span class="text-xs sm:text-sm">
                                                    {move || can_turn_off_text.clone()}
                                                </span>
                                            </td>
                                            <td>
                                                {move || {
                                                    let key = status_mac.clone();
                                                    let is_online = status_machine
                                                        .get()
                                                        .get(&key)
                                                        .cloned()
                                                        .unwrap_or(false);
                                                    if is_online {
                                                        view! {
                                                            <span class="status-pill status-pill--online">
                                                                "Online"
                                                            </span>
                                                        }
                                                    } else {
                                                        view! {
                                                            <span class="status-pill status-pill--offline">
                                                                "Offline"
                                                            </span>
                                                        }
                                                    }
                                                }}
                                            </td>
                                            <td class="hide-mobile">
                                                <span class="text-xs sm:text-sm">
                                                    {move || {
                                                        let key = idle_mac.clone();
                                                        let is_online = status_machine
                                                            .get()
                                                            .get(&key)
                                                            .cloned()
                                                            .unwrap_or(false);
                                                        if is_online {
                                                            let idle = machines
                                                                .get()
                                                                .into_iter()
                                                                .find(|m| m.mac == key)
                                                                .and_then(|m| m.idle_minutes);
                                                            match idle {
                                                                Some(mins) => format!("{} min", mins),
                                                                None => "-".to_string(),
                                                            }
                                                        } else {
                                                            let offline_mins = machines
                                                                .get()
                                                                .into_iter()
                                                                .find(|m| m.mac == key)
                                                                .and_then(|m| m.offline_minutes);
                                                            match offline_mins {
                                                                Some(mins) => format!("{}m offline", mins),
                                                                None => "-".to_string(),
                                                            }
                                                        }
                                                    }}
                                                </span>
                                            </td>
                                            <td class="hide-mobile">
                                                <span class="font-mono text-xs sm:text-sm">
                                                    {move || port_forwards_text.clone()}
                                                </span>
                                            </td>
                                            <td class="table-actions">
                                                <button
                                                    class="btn-icon btn-icon--positive"
                                                    title="Wake machine"
                                                    disabled=move || {
                                                        wake_in_progress_for_disable
                                                            .get()
                                                            .as_ref()
                                                            .map(|current| current == &wake_mac_disabled)
                                                            .unwrap_or(false)
                                                    }
                                                    on:click=move |_| {
                                                        if wake_in_progress_for_click
                                                            .get()
                                                            .as_ref()
                                                            .map(|current| current == &wake_mac_click)
                                                            .unwrap_or(false)
                                                        {
                                                            return;
                                                        }
                                                        set_wake_in_progress_btn.set(Some(wake_mac_task.clone()));
                                                        let set_wake_after = set_wake_in_progress_btn;
                                                        let mac_for_request = wake_mac_task.clone();
                                                        let name_for_alert = name_for_wake.clone();
                                                        leptos::task::spawn_local(async move {
                                                            match wake_machine(&mac_for_request).await {
                                                                Ok(message) => {
                                                                    if let Some(win) = window() {
                                                                        let _ = win.alert_with_message(&message);
                                                                    }
                                                                    console_log(
                                                                        &format!(
                                                                            "Wake request sent for {} ({})",
                                                                            name_for_alert,
                                                                            mac_for_request,
                                                                        ),
                                                                    );
                                                                }
                                                                Err(err) => {
                                                                    if let Some(win) = window() {
                                                                        let _ = win
                                                                            .alert_with_message(
                                                                                &format!("Failed to wake machine: {}", err),
                                                                            );
                                                                    }
                                                                    console_log(
                                                                        &format!(
                                                                            "Failed to wake {} ({}): {}",
                                                                            name_for_alert,
                                                                            mac_for_request,
                                                                            err,
                                                                        ),
                                                                    );
                                                                }
                                                            }
                                                            set_wake_after.set(None);
                                                        });
                                                    }
                                                >
                                                    "⚡"
                                                </button>
                                                <button
                                                    class="btn-icon"
                                                    title="Turn off machine"
                                                    disabled=move || {
                                                        !can_turn_off_machine
                                                            || turn_off_in_progress_for_disable
                                                                .get()
                                                                .as_ref()
                                                                .map(|current| current == &turn_off_mac_disabled)
                                                                .unwrap_or(false)
                                                    }
                                                    on:click=move |_| {
                                                        if !can_turn_off_machine {
                                                            if let Some(win) = window() {
                                                                let _ = win
                                                                    .alert_with_message(
                                                                        "Enable remote turn-off with a valid port before triggering this action.",
                                                                    );
                                                            }
                                                            return;
                                                        }
                                                        if turn_off_in_progress_for_click
                                                            .get()
                                                            .as_ref()
                                                            .map(|current| current == &turn_off_mac_click)
                                                            .unwrap_or(false)
                                                        {
                                                            return;
                                                        }
                                                        set_turn_off_in_progress_btn
                                                            .set(Some(turn_off_mac_task.clone()));
                                                        let set_turn_off_after = set_turn_off_in_progress_btn;
                                                        let mac_for_request = turn_off_mac_task.clone();
                                                        let name_for_alert = name_for_turnoff.clone();
                                                        leptos::task::spawn_local(async move {
                                                            match turn_off_machine(&mac_for_request).await {
                                                                Ok(message) => {
                                                                    if let Some(win) = window() {
                                                                        let _ = win.alert_with_message(&message);
                                                                    }
                                                                    console_log(
                                                                        &format!(
                                                                            "Turn-off request sent for {} ({})",
                                                                            name_for_alert,
                                                                            mac_for_request,
                                                                        ),
                                                                    );
                                                                }
                                                                Err(err) => {
                                                                    if let Some(win) = window() {
                                                                        let _ = win
                                                                            .alert_with_message(
                                                                                &format!("Failed to turn off machine: {}", err),
                                                                            );
                                                                    }
                                                                    console_log(
                                                                        &format!(
                                                                            "Failed to turn off {} ({}): {}",
                                                                            name_for_alert,
                                                                            mac_for_request,
                                                                            err,
                                                                        ),
                                                                    );
                                                                }
                                                            }
                                                            set_turn_off_after.set(None);
                                                        });
                                                    }
                                                >
                                                    {move || {
                                                        if turn_off_in_progress_for_label
                                                            .get()
                                                            .as_ref()
                                                            .map(|current| current == &turn_off_mac_label)
                                                            .unwrap_or(false)
                                                        {
                                                            "⏳"
                                                        } else {
                                                            "⏻"
                                                        }
                                                    }}
                                                </button>
                                                <button
                                                    class="btn-icon btn-icon--danger"
                                                    title="Delete machine"
                                                    on:click=move |_| {
                                                        if window()
                                                            .unwrap()
                                                            .confirm_with_message(
                                                                &format!(
                                                                    "Are you sure you want to delete machine {}?",
                                                                    name_for_confirm.clone(),
                                                                ),
                                                            )
                                                            .unwrap_or(false)
                                                        {
                                                            on_delete(delete_mac.clone());
                                                        }
                                                    }
                                                >
                                                    "🗑"
                                                </button>
                                                <div class="mobile-port-forwards show-mobile">
                                                    <span class="mobile-port-forwards__title">
                                                        "Port forwards"
                                                    </span>
                                                    <div class="mobile-port-forwards__list">
                                                        {mobile_port_forward_labels
                                                            .iter()
                                                            .cloned()
                                                            .map(|label| {
                                                                view! {
                                                                    <span class="mobile-port-forwards__chip">{label}</span>
                                                                }
                                                            })
                                                            .collect::<Vec<_>>()}
                                                    </div>
                                                </div>
                                            </td>
                                        </tr>
                                    }
                                }
                            />
                        </Show>
                    </tbody>
                </table>
            </div>
        </section>
    }
}

#[component]
fn AddMachine(
    machine: ReadSignal<Machine>,
    registred_machines: ReadSignal<Vec<Machine>>,
    set_registred_machines: WriteSignal<Vec<Machine>>,
) -> impl IntoView {
    let navigate = leptos_router::hooks::use_navigate();
    let (machine_form_data, set_machine_form_data) = signal::<Machine>(machine.get());
    let (show_turn_off_port, set_show_turn_off_port) = signal(false);
    let (port_forwards, set_port_forwards) = signal::<Vec<PortForward>>(vec![]);
    let (erros, set_errors) = signal::<HashMap<String, Vec<String>>>(HashMap::new());
    let (loading, set_loading) = signal(false);
    Effect::new(move |_| {
        let current = machine.get();
        set_show_turn_off_port.set(current.can_be_turned_off);
        console_log(&format!(
            "Pre-filling form with discovered machine: {:?} ({})",
            current.port_forwards.clone(),
            current.ip
        ));
        set_port_forwards.set(current.port_forwards.clone());
        set_machine_form_data.set(current);
    });

    Effect::new(move |_| {
        let forwards = port_forwards.get();
        set_machine_form_data.update(|machine| {
            machine.port_forwards = forwards.clone();
        });
    });

    fn set_input_value(
        key: &str,
        value: String,
        set_machine_form_data: WriteSignal<Machine>,
        machine_form_data: ReadSignal<Machine>,
        set_show_turn_off_port: WriteSignal<bool>,
    ) {
        let mut current = machine_form_data.get();
        match key {
            "name" => current.name = value,
            "mac" => current.mac = value,
            "ip" => current.ip = value,
            "description" => current.description = Some(value),
            "turn_off_port" => {
                let trimmed = value.trim();
                current.turn_off_port = if trimmed.is_empty() {
                    None
                } else {
                    trimmed.parse().ok()
                };
            }
            "can_be_turned_off" => {
                let enabled = value == "on";
                current.can_be_turned_off = enabled;
                if !enabled {
                    current.turn_off_port = None;
                }
                set_show_turn_off_port.set(enabled);
            }
            _ => {}
        };
        set_machine_form_data.set(current);
    }

    let on_submit = move |ev: SubmitEvent| {
        // stop the page from reloading!
        ev.prevent_default();

        set_loading.set(true);
        let navigate = navigate.clone();
        let validation_errors = validate_machine_form(&machine_form_data.get());
        if validation_errors.is_empty() {
            console::log_1(&"Form is valid".into());
            let current_machines = registred_machines.get();
            let mut new_machines = current_machines.clone();

            leptos::task::spawn_local(async move {
                if (create_machine(machine_form_data.get()).await).is_ok() {
                    //console_log(&format!("Loaded {:?} machines", machines));
                    let new_machine = machine_form_data.get();
                    new_machines.insert(0, new_machine.clone());

                    set_registred_machines.set(new_machines);
                    // Clear the form
                    set_machine_form_data.set(Machine {
                        name: "".to_string(),
                        mac: "".to_string(),
                        ip: "".to_string(),
                        description: None,
                        turn_off_port: None,
                        can_be_turned_off: false,
                        inactivity_period: 60,
                        port_forwards: vec![],
                        idle_minutes: None,
                        offline_minutes: None,
                    });
                    set_port_forwards.set(vec![]);
                    set_show_turn_off_port.set(false);
                    set_errors.set(HashMap::new());
                    let url = format!("/machines/{}", new_machine.mac);
                    navigate(&url, Default::default());
                } else {
                    console_log("Error creating machine");
                }
            });
            set_loading.set(false);
        } else {
            set_errors.set(validation_errors);
            set_loading.set(false);
        }
    };

    view! {
        <section class="card">
            <header class="card-header">
                <h3 class="card-title">"Add new machine"</h3>
                <p class="card-subtitle">
                    {move || {
                        let ip_hint = machine_form_data.get().ip;
                        if ip_hint.is_empty() {
                            "Register a device to make it available for wake and forwarding."
                                .to_string()
                        } else {
                            format!("Pre-filled from discovery: {}", ip_hint)
                        }
                    }}
                </p>
            </header>
            <form on:submit=on_submit class="form-grid">
                <div class="form-grid two-column">
                    <div class="field">
                        <label for="name">"Name"</label>
                        <input
                            type="text"
                            id="name"
                            name="name"
                            class="input"
                            required
                            on:input:target=move |ev| {
                                let input_value = ev.target().value();
                                set_input_value(
                                    "name",
                                    input_value,
                                    set_machine_form_data,
                                    machine_form_data,
                                    set_show_turn_off_port,
                                );
                            }
                            prop:value=move || machine_form_data.get().name
                        />
                        <ErrorDisplay erros=erros key="name" />
                    </div>
                    <div class="field">
                        <label for="mac">"MAC address"</label>
                        <input
                            type="text"
                            id="mac"
                            name="mac"
                            class="input"
                            required
                            on:input:target=move |ev| {
                                let input_value = ev.target().value();
                                set_input_value(
                                    "mac",
                                    input_value,
                                    set_machine_form_data,
                                    machine_form_data,
                                    set_show_turn_off_port,
                                );
                            }
                            prop:value=move || machine_form_data.get().mac
                        />
                        <ErrorDisplay erros=erros key="mac" />
                    </div>
                </div>

                <div class="form-grid two-column">
                    <div class="field">
                        <label for="ip">"IP address"</label>
                        <input
                            type="text"
                            id="ip"
                            name="ip"
                            class="input"
                            required
                            on:input:target=move |ev| {
                                let input_value = ev.target().value();
                                set_input_value(
                                    "ip",
                                    input_value,
                                    set_machine_form_data,
                                    machine_form_data,
                                    set_show_turn_off_port,
                                );
                            }
                            prop:value=move || machine_form_data.get().ip
                        />
                        <ErrorDisplay erros=erros key="ip" />
                    </div>
                </div>

                <div class="field">
                    <label for="description">"Description (optional)"</label>
                    <input
                        type="text"
                        id="description"
                        name="description"
                        class="input"
                        on:input:target=move |ev| {
                            let input_value = ev.target().value();
                            set_input_value(
                                "description",
                                input_value,
                                set_machine_form_data,
                                machine_form_data,
                                set_show_turn_off_port,
                            );
                        }
                        prop:value=move || {
                            machine_form_data.get().description.clone().unwrap_or_default()
                        }
                    />
                    <ErrorDisplay erros=erros key="description" />
                </div>

                <div class="field">
                    <div class="field-header">
                        <label>"Port forwards"</label>
                        <button
                            type="button"
                            class="btn btn-soft btn-sm"
                            on:click=move |_| {
                                set_port_forwards
                                    .update(|pfs| {
                                        pfs.push(PortForward {
                                            name: None,
                                            local_port: 0,
                                            target_port: 0,
                                        });
                                    });
                            }
                        >
                            "+ Add port"
                        </button>
                    </div>
                    <p class="field-help">
                        "Expose local TCP ports that should forward to the machine once it wakes."
                    </p>
                    <Show
                        when=move || !port_forwards.get().is_empty()
                        fallback=|| {
                            view! { <p class="field-empty">"No port forwards configured."</p> }
                        }
                    >
                        <div class="port-forward-list">
                            <For
                                each=move || {
                                    port_forwards
                                        .get()
                                        .into_iter()
                                        .enumerate()
                                        .collect::<Vec<(usize, PortForward)>>()
                                }
                                key=|(idx, _)| *idx
                                children=move |(idx, _)| {
                                    let row = idx + 1;
                                    let name_id = format!("pf-name-{}", row);
                                    let local_id = format!("pf-local-{}", row);
                                    let target_id = format!("pf-target-{}", row);

                                    view! {
                                        <div class="port-forward-item">
                                            <div class="port-forward-item__header">
                                                <span class="port-forward-item__title">
                                                    {format!("Forward {}", row)}
                                                </span>
                                                <button
                                                    type="button"
                                                    class="btn btn-ghost btn-sm port-forward-item__remove"
                                                    on:click=move |_| {
                                                        set_port_forwards
                                                            .update(|pfs| {
                                                                if idx < pfs.len() {
                                                                    pfs.remove(idx);
                                                                }
                                                            });
                                                    }
                                                >
                                                    "Remove"
                                                </button>
                                            </div>
                                            <div class="port-forward-item__grid">
                                                <div class="field">
                                                    <label for=name_id
                                                        .clone()>{format!("Service name {}", row)}</label>
                                                    <input
                                                        class="input"
                                                        id=name_id
                                                        placeholder="Service name"
                                                        prop:value=move || {
                                                            port_forwards
                                                                .get()
                                                                .get(idx)
                                                                .and_then(|pf| pf.name.clone())
                                                                .unwrap_or_default()
                                                        }
                                                        on:input=move |ev| {
                                                            let target = ev.target().unwrap();
                                                            let input: HtmlInputElement = target.dyn_into().unwrap();
                                                            let value = input.value();
                                                            let trimmed = value.trim().is_empty();
                                                            set_port_forwards
                                                                .update(|pfs| {
                                                                    if let Some(pf) = pfs.get_mut(idx) {
                                                                        pf.name = if trimmed { None } else { Some(value.clone()) };
                                                                    }
                                                                });
                                                        }
                                                    />
                                                </div>
                                                <div class="field">
                                                    <label for=local_id
                                                        .clone()>{format!("Local port {}", row)}</label>
                                                    <input
                                                        class="input"
                                                        id=local_id
                                                        placeholder="Local port"
                                                        type="number"
                                                        min="1"
                                                        max="65535"
                                                        prop:value=move || {
                                                            port_forwards
                                                                .get()
                                                                .get(idx)
                                                                .map(|pf| pf.local_port.to_string())
                                                                .unwrap_or_default()
                                                        }
                                                        on:input=move |ev| {
                                                            let target = ev.target().unwrap();
                                                            let input: HtmlInputElement = target.dyn_into().unwrap();
                                                            let parsed = input.value().parse::<u16>().unwrap_or(0);
                                                            set_port_forwards
                                                                .update(|pfs| {
                                                                    if let Some(pf) = pfs.get_mut(idx) {
                                                                        pf.local_port = parsed;
                                                                    }
                                                                });
                                                        }
                                                    />
                                                </div>
                                                <div class="field">
                                                    <label for=target_id
                                                        .clone()>{format!("Target port {}", row)}</label>
                                                    <input
                                                        class="input"
                                                        id=target_id
                                                        placeholder="Target port"
                                                        type="number"
                                                        min="1"
                                                        max="65535"
                                                        prop:value=move || {
                                                            port_forwards
                                                                .get()
                                                                .get(idx)
                                                                .map(|pf| pf.target_port.to_string())
                                                                .unwrap_or_default()
                                                        }
                                                        on:input=move |ev| {
                                                            let target = ev.target().unwrap();
                                                            let input: HtmlInputElement = target.dyn_into().unwrap();
                                                            let parsed = input.value().parse::<u16>().unwrap_or(0);
                                                            set_port_forwards
                                                                .update(|pfs| {
                                                                    if let Some(pf) = pfs.get_mut(idx) {
                                                                        pf.target_port = parsed;
                                                                    }
                                                                });
                                                        }
                                                    />
                                                </div>
                                            </div>
                                        </div>
                                    }
                                }
                            />
                        </div>
                    </Show>
                </div>

                <div class="field field-toggle">
                    <input
                        type="checkbox"
                        id="can_be_turned_off"
                        name="can_be_turned_off"
                        class="checkbox"
                        prop:checked=move || machine_form_data.get().can_be_turned_off
                        on:input:target=move |ev| {
                            let input_value = if ev.target().checked() { "on" } else { "off" }
                                .to_string();
                            set_input_value(
                                "can_be_turned_off",
                                input_value,
                                set_machine_form_data,
                                machine_form_data,
                                set_show_turn_off_port,
                            );
                        }
                    />
                    <div class="field-toggle__content">
                        <label for="can_be_turned_off">"Allow remote turn off"</label>
                        <p class="field-help">
                            "Requires the machine to expose a shutdown endpoint."
                        </p>
                    </div>
                </div>

                <Show when=move || show_turn_off_port.get() fallback=|| view! { <></> }>
                    {move || {
                        view! {
                            <div class="field">
                                <label for="turn_off_port">"Turn off port (optional)"</label>
                                <input
                                    type="number"
                                    id="turn_off_port"
                                    name="turn_off_port"
                                    class="input"
                                    min="1"
                                    max="65535"
                                    on:input:target=move |ev| {
                                        let input_value = ev.target().value();
                                        set_input_value(
                                            "turn_off_port",
                                            input_value,
                                            set_machine_form_data,
                                            machine_form_data,
                                            set_show_turn_off_port,
                                        );
                                    }
                                    prop:value=move || {
                                        machine_form_data
                                            .get()
                                            .turn_off_port
                                            .map(|port| port.to_string())
                                            .unwrap_or_default()
                                    }
                                />
                                <ErrorDisplay erros=erros key="turn_off_port" />
                            </div>
                        }
                    }}
                </Show>
                <div class="form-footer">
                    <button type="submit" class="btn btn-primary" disabled=move || loading.get()>
                        {move || {
                            if loading.get() { "Adding machine…" } else { "Add machine" }
                        }}
                    </button>
                </div>
            </form>
        </section>
    }
}
#[component]
fn HomePage() -> impl IntoView {
    let default_machine = Machine {
        name: "".to_string(),
        mac: "".to_string(),
        ip: "".to_string(),
        description: None,
        turn_off_port: Some(3000),
        can_be_turned_off: false,
        inactivity_period: 60,
        port_forwards: vec![PortForward {
            name: None,
            local_port: 0,
            target_port: 0,
        }],
        idle_minutes: None,
        offline_minutes: None,
    };
    let (machine, set_machine) = signal::<Machine>(default_machine);

    let (registred_machines, set_registred_machines) = signal::<Vec<Machine>>(vec![]);
    let (status_machine, set_status_machine) = signal::<HashMap<String, bool>>(HashMap::new());

    // Drives the periodic re-check below. Bumping it is what makes a machine
    // that comes up a few minutes after a wake show as online without the
    // user having to reload the page.
    let (status_tick, set_status_tick) = signal(0u32);
    set_interval(
        move || set_status_tick.update(|tick| *tick = tick.wrapping_add(1)),
        std::time::Duration::from_secs(STATUS_REFRESH_SECS),
    );

    // Load initial registred machines
    Effect::new(move || {
        leptos::task::spawn_local(async move {
            if let Ok(machines) = fetch_machines().await {
                //console_log(&format!("Loaded {:?} machines", machines));
                set_registred_machines.set(machines);
            }
        });
    });

    // check the status of registred machines when they change, and again on
    // every refresh tick
    Effect::new(move |_| {
        let _ = status_tick.get();
        let machines = registred_machines.get();
        if machines.is_empty() {
            // console_log("No registred machines");
            return;
        }

        // Spawn the async task to check all machines
        leptos::task::spawn_local(async move {
            // Create a vector of futures to check each machine concurrently
            let mut futures = Vec::new();

            for m in machines {
                let machine_mac = m.mac.clone();
                let future = async move { (machine_mac, is_machine_online(&m.mac).await) };
                futures.push(future);
            }

            // Wait for all futures to complete, regardless of individual failures
            let results = futures::future::join_all(futures).await;

            // Build the status map from all results
            let mut status_map = HashMap::new();
            for (mac, is_online) in results {
                status_map.insert(mac, is_online);
            }
            set_status_machine.set(status_map);
        });
    });

    view! {
        <Header set_machine=set_machine registred_machines=registred_machines />
        <Show when=move || { !registred_machines.get().is_empty() } fallback=|| view! {}>
            <RegistredMachines
                machines=registred_machines
                status_machine=status_machine
                set_registred_machines=set_registred_machines
            />
        </Show>
        <AddMachine
            machine=machine
            registred_machines=registred_machines
            set_registred_machines=set_registred_machines
        />
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App)
}

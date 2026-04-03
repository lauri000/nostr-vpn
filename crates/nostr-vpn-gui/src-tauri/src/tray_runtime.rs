use std::collections::HashSet;
use std::io::Write;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::process::{Command as ProcessCommand, Stdio};

use anyhow::{Context, Result, anyhow};
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
use tauri::Manager;
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
use tauri::menu::{
    CheckMenuItemBuilder, Menu, MenuItemBuilder, PredefinedMenuItem, Submenu, SubmenuBuilder,
};

use super::{
    AppState, NvpnBackend, ParticipantView, TRAY_EXIT_NODE_MENU_ID_PREFIX,
    TRAY_EXIT_NODE_NONE_MENU_ID, TRAY_ICON_ID, TRAY_OPEN_MENU_ID, TRAY_QUIT_UI_MENU_ID,
    TRAY_RUN_EXIT_NODE_MENU_ID, TRAY_THIS_DEVICE_MENU_ID, TRAY_VPN_TOGGLE_MENU_ID,
    TrayExitNodeEntry, TrayMenuItemSpec, TrayNetworkGroup, TrayRuntimeState,
};

fn apply_windows_subprocess_flags(command: &mut ProcessCommand) -> &mut ProcessCommand {
    #[cfg(target_os = "windows")]
    {
        use super::CREATE_NO_WINDOW;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

pub(crate) fn short_text(value: &str, leading: usize, trailing: usize) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= leading + trailing + 3 {
        return value.to_string();
    }

    format!(
        "{}...{}",
        chars.iter().take(leading).collect::<String>(),
        chars[chars.len() - trailing..].iter().collect::<String>()
    )
}

pub(crate) fn display_tunnel_ip(tunnel_ip: &str) -> String {
    let trimmed = tunnel_ip.trim();
    if trimmed.is_empty() {
        "-".to_string()
    } else {
        trimmed.split('/').next().unwrap_or(trimmed).to_string()
    }
}

pub(crate) fn tray_status_text(
    session_active: bool,
    service_setup_required: bool,
    service_enable_required: bool,
    session_status: &str,
) -> String {
    if session_active {
        "Connected".to_string()
    } else if service_setup_required {
        "Install background service".to_string()
    } else if service_enable_required {
        "Enable background service".to_string()
    } else if session_status.trim().is_empty() || session_status == "Disconnected" {
        "Disconnected".to_string()
    } else {
        session_status.to_string()
    }
}

pub(crate) fn tray_vpn_status_menu_text(status_text: &str) -> String {
    format!("VPN Status: {status_text}")
}

pub(crate) fn tray_vpn_toggle_text(session_active: bool) -> &'static str {
    if session_active {
        "Turn VPN Off"
    } else {
        "Turn VPN On"
    }
}

pub(crate) fn copy_text_to_clipboard(text: &str) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        return Err(anyhow!("npub unavailable"));
    }

    #[cfg(target_os = "macos")]
    {
        copy_text_with_command("pbcopy", &[], text)
    }

    #[cfg(target_os = "linux")]
    {
        let candidates: [(&str, &[&str]); 3] = [
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        let mut last_error = None;
        for (program, args) in candidates {
            match copy_text_with_command(program, args, text) {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("no clipboard command available")))
    }

    #[cfg(target_os = "windows")]
    {
        copy_text_with_command("cmd", &["/C", "clip"], text)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(anyhow!("clipboard copy is unsupported on this platform"))
    }
}

fn copy_text_with_command(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut command = ProcessCommand::new(program);
    let mut child = apply_windows_subprocess_flags(&mut command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start {program}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("failed to open stdin for {program}"))?;
    stdin
        .write_all(text.as_bytes())
        .with_context(|| format!("failed to send text to {program}"))?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .with_context(|| format!("failed to wait for {program}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(anyhow!("{program} exited with status {}", output.status))
        } else {
            Err(anyhow!("{program} failed: {stderr}"))
        }
    }
}

pub(crate) fn tray_participant_display_name(participant: &ParticipantView) -> String {
    let alias = participant.magic_dns_alias.trim();
    if !alias.is_empty() {
        return alias.to_string();
    }

    if let Some(label) = participant
        .magic_dns_name
        .split('.')
        .find(|segment| !segment.is_empty())
    {
        return label.to_string();
    }

    short_text(&participant.npub, 16, 8)
}

pub(crate) fn tray_network_groups(networks: &[super::NetworkView]) -> Vec<TrayNetworkGroup> {
    let mut groups = Vec::new();

    for network in networks.iter().filter(|network| network.enabled) {
        let devices = network
            .participants
            .iter()
            .filter(|participant| participant.state != "local")
            .map(|participant| {
                format!(
                    "{} ({})",
                    tray_participant_display_name(participant),
                    participant.state
                )
            })
            .collect::<Vec<_>>();

        if devices.is_empty() {
            continue;
        }

        groups.push(TrayNetworkGroup {
            title: format!(
                "{} ({}/{} online)",
                network.name, network.online_count, network.expected_count
            ),
            devices,
        });
    }

    groups
}

pub(crate) fn tray_exit_node_entries(
    networks: &[super::NetworkView],
    selected_exit_node: &str,
) -> Vec<TrayExitNodeEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();

    for network in networks.iter().filter(|network| network.enabled) {
        for participant in &network.participants {
            if participant.state == "local"
                || !participant.offers_exit_node
                || !seen.insert(participant.pubkey_hex.clone())
            {
                continue;
            }

            entries.push(TrayExitNodeEntry {
                pubkey_hex: participant.pubkey_hex.clone(),
                title: tray_participant_display_name(participant),
                selected: participant.pubkey_hex == selected_exit_node,
            });
        }
    }

    entries.sort_by(|left, right| {
        left.title
            .cmp(&right.title)
            .then(left.pubkey_hex.cmp(&right.pubkey_hex))
    });
    entries
}

pub(crate) fn tray_menu_spec(runtime_state: &TrayRuntimeState) -> Vec<TrayMenuItemSpec> {
    let mut network_items = Vec::new();
    if runtime_state.network_groups.is_empty() {
        network_items.push(TrayMenuItemSpec::Text {
            id: None,
            text: "No network devices configured".to_string(),
            enabled: false,
        });
    } else {
        for group in &runtime_state.network_groups {
            network_items.push(TrayMenuItemSpec::Submenu {
                text: group.title.clone(),
                enabled: true,
                items: group
                    .devices
                    .iter()
                    .map(|device| TrayMenuItemSpec::Text {
                        id: None,
                        text: device.clone(),
                        enabled: false,
                    })
                    .collect(),
            });
        }
    }

    let mut exit_node_items = vec![
        TrayMenuItemSpec::Check {
            id: TRAY_EXIT_NODE_NONE_MENU_ID.to_string(),
            text: "None".to_string(),
            enabled: true,
            checked: !runtime_state.advertise_exit_node
                && !runtime_state.exit_nodes.iter().any(|entry| entry.selected),
        },
        TrayMenuItemSpec::Check {
            id: TRAY_RUN_EXIT_NODE_MENU_ID.to_string(),
            text: "Offer Private Exit Node".to_string(),
            enabled: true,
            checked: runtime_state.advertise_exit_node,
        },
    ];
    exit_node_items.push(TrayMenuItemSpec::Separator);
    if runtime_state.exit_nodes.is_empty() {
        exit_node_items.push(TrayMenuItemSpec::Text {
            id: None,
            text: "No exit nodes available".to_string(),
            enabled: false,
        });
    } else {
        exit_node_items.extend(runtime_state.exit_nodes.iter().map(|entry| {
            TrayMenuItemSpec::Check {
                id: format!("{TRAY_EXIT_NODE_MENU_ID_PREFIX}{}", entry.pubkey_hex),
                text: entry.title.clone(),
                enabled: true,
                checked: entry.selected,
            }
        }));
    }

    vec![
        TrayMenuItemSpec::Text {
            id: None,
            text: tray_vpn_status_menu_text(&runtime_state.status_text),
            enabled: false,
        },
        TrayMenuItemSpec::Text {
            id: Some(TRAY_VPN_TOGGLE_MENU_ID.to_string()),
            text: tray_vpn_toggle_text(runtime_state.session_active).to_string(),
            enabled: true,
        },
        TrayMenuItemSpec::Separator,
        TrayMenuItemSpec::Text {
            id: Some(TRAY_THIS_DEVICE_MENU_ID.to_string()),
            text: runtime_state.this_device_text.clone(),
            enabled: !runtime_state.this_device_copy_value.trim().is_empty(),
        },
        TrayMenuItemSpec::Submenu {
            text: "Network Devices".to_string(),
            enabled: true,
            items: network_items,
        },
        TrayMenuItemSpec::Submenu {
            text: "Exit Nodes".to_string(),
            enabled: true,
            items: exit_node_items,
        },
        TrayMenuItemSpec::Separator,
        TrayMenuItemSpec::Text {
            id: Some(TRAY_OPEN_MENU_ID.to_string()),
            text: "Settings...".to_string(),
            enabled: true,
        },
        TrayMenuItemSpec::Text {
            id: Some(TRAY_QUIT_UI_MENU_ID.to_string()),
            text: "Quit".to_string(),
            enabled: true,
        },
    ]
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn current_tray_runtime_state<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> TrayRuntimeState {
    let Some(state) = app.try_state::<AppState>() else {
        return TrayRuntimeState::default();
    };
    let Ok(backend) = state.backend.lock() else {
        return TrayRuntimeState::default();
    };
    backend.tray_runtime_state()
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn append_tray_spec_to_menu<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    menu: &Menu<R>,
    spec: &TrayMenuItemSpec,
) -> tauri::Result<()> {
    match spec {
        TrayMenuItemSpec::Check {
            id,
            text,
            enabled,
            checked,
        } => {
            let item = CheckMenuItemBuilder::with_id(id.clone(), text)
                .enabled(*enabled)
                .checked(*checked)
                .build(app)?;
            menu.append(&item)?;
        }
        TrayMenuItemSpec::Text { id, text, enabled } => {
            let item = if let Some(id) = id {
                MenuItemBuilder::with_id(id.clone(), text)
                    .enabled(*enabled)
                    .build(app)?
            } else {
                MenuItemBuilder::new(text).enabled(*enabled).build(app)?
            };
            menu.append(&item)?;
        }
        TrayMenuItemSpec::Submenu {
            text,
            enabled,
            items,
        } => {
            let submenu = build_tray_submenu_from_spec(app, text, *enabled, items)?;
            menu.append(&submenu)?;
        }
        TrayMenuItemSpec::Separator => {
            let separator = PredefinedMenuItem::separator(app)?;
            menu.append(&separator)?;
        }
    }

    Ok(())
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn append_tray_spec_to_submenu<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    submenu: &Submenu<R>,
    spec: &TrayMenuItemSpec,
) -> tauri::Result<()> {
    match spec {
        TrayMenuItemSpec::Check {
            id,
            text,
            enabled,
            checked,
        } => {
            let item = CheckMenuItemBuilder::with_id(id.clone(), text)
                .enabled(*enabled)
                .checked(*checked)
                .build(app)?;
            submenu.append(&item)?;
        }
        TrayMenuItemSpec::Text { id, text, enabled } => {
            let item = if let Some(id) = id {
                MenuItemBuilder::with_id(id.clone(), text)
                    .enabled(*enabled)
                    .build(app)?
            } else {
                MenuItemBuilder::new(text).enabled(*enabled).build(app)?
            };
            submenu.append(&item)?;
        }
        TrayMenuItemSpec::Submenu {
            text,
            enabled,
            items,
        } => {
            let child = build_tray_submenu_from_spec(app, text, *enabled, items)?;
            submenu.append(&child)?;
        }
        TrayMenuItemSpec::Separator => {
            let separator = PredefinedMenuItem::separator(app)?;
            submenu.append(&separator)?;
        }
    }

    Ok(())
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn build_tray_submenu_from_spec<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    text: &str,
    enabled: bool,
    items: &[TrayMenuItemSpec],
) -> tauri::Result<Submenu<R>> {
    let submenu = SubmenuBuilder::new(app, text).enabled(enabled).build()?;
    for item in items {
        append_tray_spec_to_submenu(app, &submenu, item)?;
    }
    Ok(submenu)
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn build_tray_menu<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    runtime_state: &TrayRuntimeState,
) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    for item in tray_menu_spec(runtime_state) {
        append_tray_spec_to_menu(app, &menu, &item)?;
    }
    Ok(menu)
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn refresh_tray_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Some(tray) = app.tray_by_id(TRAY_ICON_ID) else {
        return;
    };

    let runtime_state = current_tray_runtime_state(app);
    let Some(state) = app.try_state::<AppState>() else {
        if let Ok(menu) = build_tray_menu(app, &runtime_state) {
            let _ = tray.set_menu(Some(menu));
        }
        return;
    };
    let Ok(mut last_tray_runtime_state) = state.last_tray_runtime_state.lock() else {
        return;
    };

    if *last_tray_runtime_state == runtime_state {
        return;
    }

    if let Ok(menu) = build_tray_menu(app, &runtime_state)
        && tray.set_menu(Some(menu)).is_ok()
    {
        *last_tray_runtime_state = runtime_state;
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub(crate) fn refresh_tray_menu<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) {}

pub(crate) fn run_tray_backend_action<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    action: impl FnOnce(&mut NvpnBackend) -> Result<()>,
) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Ok(mut backend) = state.backend.lock() else {
        return;
    };

    if let Err(error) = action(&mut backend) {
        backend.session_status = format!("Tray action failed: {error}");
    }
    backend.tick();
}

use std::collections::HashSet;
use std::env;
use std::process::Command as ProcessCommand;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use nostr_vpn_core::config::{normalize_advertised_route, normalize_nostr_pubkey};
use tauri::Manager;

use super::{
    AUTOSTART_LAUNCH_ARG, AppState, DEBUG_AUTOMATION_DEEP_LINK_PREFIX, NETWORK_INVITE_PREFIX,
    NVPN_GUI_IFACE_ENV, NvpnBackend,
};

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunningGuiInstance {
    pub(crate) pid: u32,
    pub(crate) autostart: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GuiLaunchDisposition {
    Continue {
        terminate_pids: Vec<u32>,
    },
    #[cfg(any(test, target_os = "macos", target_os = "linux"))]
    Exit,
}

pub(crate) fn should_close_to_tray<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> bool {
    let Some(state) = app.try_state::<AppState>() else {
        return true;
    };
    let Ok(backend) = state.backend.lock() else {
        return true;
    };
    backend.config.close_to_tray_on_close
}

pub(crate) fn started_from_autostart_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .any(|arg| arg.as_ref() == AUTOSTART_LAUNCH_ARG)
}

pub(crate) fn started_from_autostart() -> bool {
    started_from_autostart_args(env::args())
}

pub(crate) fn env_flag_is_truthy(name: &str) -> bool {
    env::var(name).is_ok_and(|value| {
        let trimmed = value.trim();
        !trimmed.is_empty()
            && trimmed != "0"
            && !trimmed.eq_ignore_ascii_case("false")
            && !trimmed.eq_ignore_ascii_case("no")
            && !trimmed.eq_ignore_ascii_case("off")
    })
}

pub(crate) fn tauri_automation_enabled() -> bool {
    env_flag_is_truthy("TAURI_AUTOMATION")
}

pub(crate) fn nvpn_gui_iface_override() -> Option<String> {
    std::env::var(NVPN_GUI_IFACE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn should_surface_existing_instance_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    !started_from_autostart_args(args)
}

pub(crate) fn is_valid_relay_url(value: &str) -> bool {
    value.starts_with("ws://") || value.starts_with("wss://")
}

pub(crate) fn parse_exit_node_input(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("off")
        || trimmed.eq_ignore_ascii_case("none")
    {
        return Ok(String::new());
    }

    normalize_nostr_pubkey(trimmed)
}

pub(crate) fn parse_advertised_routes_input(value: &str) -> Result<Vec<String>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(Vec::new());
    }

    let mut routes = Vec::new();
    for raw in value.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }

        let normalized = normalize_advertised_route(raw)
            .ok_or_else(|| anyhow!("invalid advertised route '{raw}'"))?;
        if !routes.iter().any(|existing| existing == &normalized) {
            routes.push(normalized);
        }
    }

    Ok(routes)
}

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
pub(crate) fn is_nostr_vpn_gui_process(command: &str) -> bool {
    command.contains("Contents/MacOS/nostr-vpn-gui")
        || command.ends_with("nostr-vpn-gui")
        || command.contains("/nostr-vpn-gui ")
        || command.contains("/nostr-vpn-gui --")
}

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
pub(crate) fn parse_running_gui_instances(raw: &str, current_pid: u32) -> Vec<RunningGuiInstance> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }

            let pid_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
            let pid = trimmed[..pid_end].parse::<u32>().ok()?;
            if pid == current_pid {
                return None;
            }

            let command = trimmed[pid_end..].trim();
            if !is_nostr_vpn_gui_process(command) {
                return None;
            }

            Some(RunningGuiInstance {
                pid,
                autostart: started_from_autostart_args(command.split_whitespace()),
            })
        })
        .collect()
}

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
pub(crate) fn gui_launch_disposition(
    launched_from_autostart: bool,
    other_instances: &[RunningGuiInstance],
) -> GuiLaunchDisposition {
    if launched_from_autostart {
        if other_instances.is_empty() {
            GuiLaunchDisposition::Continue {
                terminate_pids: Vec::new(),
            }
        } else {
            GuiLaunchDisposition::Exit
        }
    } else {
        let terminate_pids = other_instances
            .iter()
            .filter(|instance| instance.autostart)
            .map(|instance| instance.pid)
            .collect::<Vec<_>>();
        GuiLaunchDisposition::Continue { terminate_pids }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn resolve_gui_launch_conflicts(
    launched_from_autostart: bool,
) -> Result<GuiLaunchDisposition> {
    let output = ProcessCommand::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .context("failed to list running GUI processes")?;
    let raw = String::from_utf8_lossy(&output.stdout);
    let other_instances = parse_running_gui_instances(&raw, std::process::id());
    Ok(gui_launch_disposition(
        launched_from_autostart,
        &other_instances,
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn resolve_gui_launch_conflicts(
    _launched_from_autostart: bool,
) -> Result<GuiLaunchDisposition> {
    Ok(GuiLaunchDisposition::Continue {
        terminate_pids: Vec::new(),
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn terminate_gui_instances(pids: &[u32]) {
    if pids.is_empty() {
        return;
    }

    let mut command = ProcessCommand::new("kill");
    for pid in pids {
        command.arg(pid.to_string());
    }
    let _ = command.status();
    std::thread::sleep(Duration::from_millis(300));
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn terminate_gui_instances(_pids: &[u32]) {}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn hide_main_window_to_tray<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    let _ = window.minimize();
    let _ = window.hide();
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub(crate) fn hide_main_window_to_tray<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) {}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub(crate) fn show_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };

    let _ = window.unminimize();
    window.show()?;
    window.set_focus()?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub(crate) fn show_main_window<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    Ok(())
}

pub(crate) fn extract_invite_from_deep_link(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let payload = trimmed.strip_prefix(NETWORK_INVITE_PREFIX)?;
    if payload.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

pub(crate) fn extract_app_deep_links_from_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut urls = Vec::new();
    let mut seen = HashSet::new();
    for arg in args {
        let value = arg.as_ref().trim();
        if value.is_empty() {
            continue;
        }
        if extract_invite_from_deep_link(value).is_none()
            && extract_debug_automation_command_from_deep_link(value).is_none()
        {
            continue;
        }
        if seen.insert(value.to_string()) {
            urls.push(value.to_string());
        }
    }
    urls
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DebugAutomationCommand {
    RequestActiveJoin,
    AcceptActiveJoin { requester_npub: String },
    Tick,
}

pub(crate) fn debug_deep_link_query_value(value: &str, key: &str) -> Option<String> {
    let (_, query) = value.split_once('?')?;
    query.split('&').find_map(|entry| {
        let (name, raw_value) = entry.split_once('=')?;
        if name != key {
            return None;
        }
        let trimmed = raw_value.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(trimmed.to_string())
    })
}

pub(crate) fn extract_debug_automation_command_from_deep_link(
    value: &str,
) -> Option<DebugAutomationCommand> {
    if !cfg!(debug_assertions) {
        return None;
    }

    let trimmed = value.trim();
    let payload = trimmed.strip_prefix(DEBUG_AUTOMATION_DEEP_LINK_PREFIX)?;
    let action = payload.split('?').next()?.trim_matches('/');
    match action {
        "request-join" => Some(DebugAutomationCommand::RequestActiveJoin),
        "accept-join" => {
            let requester_npub = debug_deep_link_query_value(trimmed, "requester")?;
            Some(DebugAutomationCommand::AcceptActiveJoin { requester_npub })
        }
        "tick" => Some(DebugAutomationCommand::Tick),
        _ => None,
    }
}

pub(crate) fn run_debug_automation_command(
    backend: &mut NvpnBackend,
    command: &DebugAutomationCommand,
) -> Result<()> {
    match command {
        DebugAutomationCommand::RequestActiveJoin => {
            let network_id = backend.config.active_network().id.clone();
            backend.request_network_join(&network_id)
        }
        DebugAutomationCommand::AcceptActiveJoin { requester_npub } => {
            let network_id = backend.config.active_network().id.clone();
            backend.accept_join_request(&network_id, requester_npub)
        }
        DebugAutomationCommand::Tick => {
            backend.tick();
            Ok(())
        }
    }
}

pub(crate) fn import_network_invite_from_deep_link<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    url: &str,
) -> Result<bool> {
    let Some(invite) = extract_invite_from_deep_link(url) else {
        return Ok(false);
    };
    let Some(state) = app.try_state::<AppState>() else {
        return Err(anyhow!("application state is unavailable"));
    };

    super::with_backend(state, |backend| backend.import_network_invite(&invite))
        .map_err(|error| anyhow!(error))?;
    super::refresh_tray_menu(app);
    let _ = show_main_window(app);
    Ok(true)
}

pub(crate) fn run_debug_automation_from_deep_link<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    url: &str,
) -> Result<bool> {
    let Some(command) = extract_debug_automation_command_from_deep_link(url) else {
        return Ok(false);
    };
    let Some(state) = app.try_state::<AppState>() else {
        return Err(anyhow!("application state is unavailable"));
    };

    super::with_backend(state, |backend| {
        run_debug_automation_command(backend, &command)
    })
    .map_err(|error| anyhow!(error))?;
    super::refresh_tray_menu(app);
    let _ = show_main_window(app);
    Ok(true)
}

pub(crate) fn import_network_invites_from_deep_links<R: tauri::Runtime, I, S>(
    app: &tauri::AppHandle<R>,
    urls: I,
) -> Result<usize>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut imported = 0;
    for url in urls {
        if import_network_invite_from_deep_link(app, url.as_ref())?
            || run_debug_automation_from_deep_link(app, url.as_ref())?
        {
            imported += 1;
        }
    }
    Ok(imported)
}

pub(crate) fn gui_requires_service_install(
    service_supported: bool,
    service_installed: bool,
    daemon_running: bool,
) -> bool {
    service_supported && !service_installed && !daemon_running
}

pub(crate) fn gui_requires_service_enable(
    service_enablement_supported: bool,
    service_installed: bool,
    service_disabled: bool,
    daemon_running: bool,
) -> bool {
    service_enablement_supported && service_installed && service_disabled && !daemon_running
}

pub(crate) fn should_start_gui_daemon_on_launch(
    vpn_session_control_supported: bool,
    wants_background_start: bool,
    service_setup_required: bool,
) -> bool {
    vpn_session_control_supported && wants_background_start && !service_setup_required
}

pub(crate) fn should_defer_gui_daemon_start_to_service_on_autostart(
    launched_from_autostart: bool,
    service_installed: bool,
    service_disabled: bool,
) -> bool {
    cfg!(target_os = "macos") && launched_from_autostart && service_installed && !service_disabled
}

pub(crate) fn should_defer_gui_daemon_start_until_first_tick(
    platform: super::RuntimePlatform,
    should_start_on_launch: bool,
    defer_to_installed_service: bool,
) -> bool {
    platform == super::RuntimePlatform::Ios && should_start_on_launch && !defer_to_installed_service
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingLaunchAction {
    None,
    StartDaemon,
    ForceConnect,
}

pub(crate) fn pending_launch_action(
    launch_start_pending: bool,
    force_connect_pending: bool,
) -> PendingLaunchAction {
    if force_connect_pending {
        PendingLaunchAction::ForceConnect
    } else if launch_start_pending {
        PendingLaunchAction::StartDaemon
    } else {
        PendingLaunchAction::None
    }
}

pub(crate) fn gui_service_setup_status_text(autoconnect: bool) -> &'static str {
    if autoconnect {
        super::GUI_SERVICE_SETUP_REQUIRED_AUTOCONNECT_STATUS
    } else {
        super::GUI_SERVICE_SETUP_REQUIRED_STATUS
    }
}

pub(crate) fn gui_service_enable_status_text(autoconnect: bool) -> &'static str {
    if autoconnect {
        "Background service is disabled in launchd; enable it to auto-connect from the app"
    } else {
        "Background service is disabled in launchd; enable it to turn VPN on from the app"
    }
}

pub(crate) fn local_join_request_listener_enabled(config: &super::AppConfig) -> bool {
    let Ok(own_pubkey) = config.own_nostr_pubkey_hex() else {
        return false;
    };
    config.networks.iter().any(|network| {
        network.listen_for_join_requests && network.admins.iter().any(|admin| admin == &own_pubkey)
    })
}

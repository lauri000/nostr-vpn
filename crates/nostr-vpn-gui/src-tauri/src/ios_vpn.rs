use std::ffi::{CStr, CString, c_char};
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::DaemonRuntimeState;

pub(crate) const IOS_TUN_MTU: u16 = 1_280;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartVpnArgs {
    pub session_name: String,
    pub config_json: String,
    pub local_address: String,
    pub dns_servers: Vec<String>,
    pub search_domains: Vec<String>,
    pub mtu: u16,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct VpnStatus {
    pub prepared: bool,
    pub active: bool,
    pub error: Option<String>,
    pub state: Option<DaemonRuntimeState>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeStatus {
    prepared: bool,
    active: bool,
    error: Option<String>,
    state_json: Option<String>,
}

type BridgeNoArgsFn = unsafe extern "C" fn() -> *mut c_char;
type BridgeWithJsonFn = unsafe extern "C" fn(*const c_char) -> *mut c_char;
type BridgeFreeStringFn = unsafe extern "C" fn(*mut c_char);

#[derive(Clone, Copy)]
struct BridgeSymbols {
    prepare: BridgeNoArgsFn,
    start: BridgeWithJsonFn,
    stop: BridgeNoArgsFn,
    status: BridgeNoArgsFn,
    free_string: BridgeFreeStringFn,
}

pub(crate) fn prepare() -> Result<VpnStatus> {
    bridge_status_from_result(invoke_bridge_no_args(bridge_symbols()?.prepare)?)
}

pub(crate) fn start(args: &StartVpnArgs) -> Result<VpnStatus> {
    let request = serde_json::to_string(args).context("failed to serialize iOS VPN start args")?;
    bridge_status_from_result(invoke_bridge_with_json(bridge_symbols()?.start, &request)?)
}

pub(crate) fn stop() -> Result<VpnStatus> {
    bridge_status_from_result(invoke_bridge_no_args(bridge_symbols()?.stop)?)
}

pub(crate) fn status() -> Result<VpnStatus> {
    bridge_status_from_result(invoke_bridge_no_args(bridge_symbols()?.status)?)
}

pub(crate) fn local_address_for_tunnel(tunnel_ip: &str) -> String {
    if tunnel_ip.contains('/') {
        tunnel_ip.to_string()
    } else {
        format!("{}/32", strip_cidr(tunnel_ip))
    }
}

fn invoke_bridge_no_args(function: unsafe extern "C" fn() -> *mut c_char) -> Result<String> {
    let response_ptr = unsafe { function() };
    owned_string_from_bridge(response_ptr)
}

fn invoke_bridge_with_json(
    function: unsafe extern "C" fn(*const c_char) -> *mut c_char,
    payload: &str,
) -> Result<String> {
    let payload = CString::new(payload).context("bridge request contained an interior nul byte")?;
    let response_ptr = unsafe { function(payload.as_ptr()) };
    owned_string_from_bridge(response_ptr)
}

fn owned_string_from_bridge(response_ptr: *mut c_char) -> Result<String> {
    if response_ptr.is_null() {
        return Err(anyhow!("iOS VPN bridge returned a null response"));
    }

    let free_string = bridge_symbols()?.free_string;
    let text = unsafe {
        let text = CStr::from_ptr(response_ptr)
            .to_str()
            .context("iOS VPN bridge returned invalid UTF-8")?
            .to_string();
        free_string(response_ptr);
        text
    };

    Ok(text)
}

fn bridge_status_from_result(response: String) -> Result<VpnStatus> {
    let parsed = serde_json::from_str::<BridgeStatus>(&response)
        .with_context(|| format!("failed to parse iOS VPN bridge response: {response}"))?;

    let state = parsed
        .state_json
        .as_deref()
        .map(serde_json::from_str::<DaemonRuntimeState>)
        .transpose()
        .context("failed to parse iOS daemon state")?;

    Ok(VpnStatus {
        prepared: parsed.prepared,
        active: parsed.active,
        error: parsed.error,
        state,
    })
}

fn strip_cidr(value: &str) -> &str {
    value.split('/').next().unwrap_or(value)
}

fn bridge_symbols() -> Result<BridgeSymbols> {
    static BRIDGE_SYMBOLS: OnceLock<Result<BridgeSymbols, String>> = OnceLock::new();

    BRIDGE_SYMBOLS
        .get_or_init(|| {
            Ok(BridgeSymbols {
                prepare: resolve_symbol("nvpn_ios_prepare")?,
                start: resolve_symbol("nvpn_ios_start")?,
                stop: resolve_symbol("nvpn_ios_stop")?,
                status: resolve_symbol("nvpn_ios_status")?,
                free_string: resolve_symbol("nvpn_ios_free_string")?,
            })
        })
        .clone()
        .map_err(|error| anyhow!(error))
}

fn resolve_symbol<T>(symbol_name: &str) -> std::result::Result<T, String>
where
    T: Copy,
{
    let symbol_name =
        CString::new(symbol_name).map_err(|_| "symbol name contained interior nul".to_string())?;
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, symbol_name.as_ptr()) };
    if symbol.is_null() {
        return Err(format!(
            "missing iOS bridge symbol {}",
            symbol_name.to_string_lossy()
        ));
    }

    Ok(unsafe { std::mem::transmute_copy(&symbol) })
}

//! Tauri control panel for Alfone.
//!
//! This process never captures input or owns a session. It reads the daemon's
//! configuration and identity, and will later talk to the daemon over a local
//! IPC socket. Keeping the UI out of the input path is deliberate: closing the
//! window must not interrupt keyboard/mouse sharing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use alfone_core::{default_config_path, Config};
use alfone_net::{generate_pin as make_pin, Identity};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    initialized: bool,
    config_path: String,
    device_name: String,
    device_id: String,
    platform: String,
    fingerprint: Option<String>,
    short_code: Option<String>,
}

#[tauri::command]
fn status() -> Result<Status, String> {
    let path = default_config_path();
    let initialized = path.exists();
    if !initialized {
        return Ok(Status {
            initialized: false,
            config_path: path.display().to_string(),
            device_name: hostname(),
            device_id: "—".into(),
            platform: platform(),
            fingerprint: None,
            short_code: None,
        });
    }

    let (config, path) =
        Config::load_or_create(Some(path.as_path())).map_err(|e| e.to_string())?;
    let dir = identity_dir(&path);
    let identity = if Identity::cert_path(&dir).exists() {
        Some(Identity::load_or_create(&dir).map_err(|e| e.to_string())?)
    } else {
        None
    };

    Ok(Status {
        initialized: true,
        config_path: path.display().to_string(),
        device_name: config.device_name,
        device_id: config.device_id,
        platform: platform(),
        fingerprint: identity.as_ref().map(Identity::fingerprint_hex),
        short_code: identity.as_ref().map(Identity::short_code),
    })
}

#[tauri::command]
fn init_device() -> Result<Status, String> {
    let (config, path) = Config::load_or_create(None).map_err(|e| e.to_string())?;
    let identity =
        Identity::load_or_create(identity_dir(&path)).map_err(|e| e.to_string())?;
    Ok(Status {
        initialized: true,
        config_path: path.display().to_string(),
        device_name: config.device_name,
        device_id: config.device_id,
        platform: platform(),
        fingerprint: Some(identity.fingerprint_hex()),
        short_code: Some(identity.short_code()),
    })
}

/// One-shot PIN generator for the "start a session" helper. The daemon still
/// owns the real PIN when it is the one listening.
#[tauri::command]
fn new_pin() -> String {
    make_pin()
}

// ---------------------------------------------------------------------------
// Local HTTP control plane (daemon -> UI)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    ok: bool,
    state: u8,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutScreen {
    id: String,
    name: String,
    width: u32,
    height: u32,
    left: Option<String>,
    right: Option<String>,
    top: Option<String>,
    bottom: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutResponse {
    screens: Vec<LayoutScreen>,
    primary: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ScreenInput {
    id: String,
    name: String,
    width: u32,
    height: u32,
    left: Option<String>,
    right: Option<String>,
    top: Option<String>,
    bottom: Option<String>,
}

#[derive(Debug, Serialize)]
struct SetLayoutRequest<'a> {
    screens: &'a [ScreenInput],
    primary: &'a str,
}

/// Localhost control-plane health check (`GET /health`).
#[tauri::command]
async fn health() -> Result<HealthResponse, String> {
    let (base_url, token) = ipc_endpoint();
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base_url}/health"))
        .bearer_auth(&token)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("alfone daemon unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("alfone daemon error: {}", resp.status()));
    }
    resp.json()
        .await
        .map_err(|e| format!("invalid response from the alfone daemon: {e}"))
}

/// Current screen layout (`GET /layout`).
#[tauri::command]
async fn get_layout() -> Result<LayoutResponse, String> {
    let (base_url, token) = ipc_endpoint();
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base_url}/layout"))
        .bearer_auth(&token)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("alfone daemon unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("alfone daemon error: {}", resp.status()));
    }
    resp.json()
        .await
        .map_err(|e| format!("invalid response from the alfone daemon: {e}"))
}

/// Persist a new screen layout (`PUT /layout`).
#[tauri::command]
async fn set_layout(
    screens: Vec<ScreenInput>,
    primary: String,
) -> Result<LayoutResponse, String> {
    let (base_url, token) = ipc_endpoint();
    let client = reqwest::Client::new();
    let body = SetLayoutRequest {
        screens: &screens,
        primary: &primary,
    };
    let resp = client
        .put(format!("{base_url}/layout"))
        .bearer_auth(&token)
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("alfone daemon unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("alfone daemon error: {}", resp.status()));
    }
    resp.json()
        .await
        .map_err(|e| format!("invalid response from the alfone daemon: {e}"))
}

/// Base URL of the daemon's local HTTP control plane.
#[tauri::command]
async fn ipc_base_url() -> String {
    let (base_url, _token) = ipc_endpoint();
    base_url
}

fn ipc_endpoint() -> (String, String) {
    let path = default_config_path();
    // Read-only: opening the UI must not create the config (that is what
    // `init_device` / `alfone init` are for). Without a config there is no
    // token yet, so the daemon simply answers 401 until it is initialized.
    let config = if path.exists() {
        Config::load_or_create(Some(path.as_path()))
            .map(|(config, _)| config)
            .unwrap_or_default()
    } else {
        Config::default()
    };
    let addr = config
        .server
        .http_listen
        .parse::<std::net::SocketAddr>()
        .unwrap_or_else(|_| {
            std::net::SocketAddr::new(std::net::IpAddr::from([127, 0, 0, 1]), 24801)
        });
    (
        format!("http://{addr}"),
        config.security.http_token.clone(),
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            status,
            init_device,
            new_pin,
            health,
            get_layout,
            set_layout,
            ipc_base_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Alfone control panel");
}

fn identity_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|parent| parent.join("identity"))
        .unwrap_or_else(|| PathBuf::from("identity"))
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "alfone".into())
}

fn platform() -> String {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "Linux/Wayland".into()
        } else {
            "Linux/X11".into()
        }
    }
    #[cfg(target_os = "windows")]
    {
        "Windows".into()
    }
    #[cfg(target_os = "macos")]
    {
        "macOS".into()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        std::env::consts::OS.to_string()
    }
}

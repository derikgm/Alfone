//! Tauri control panel for Alfone.
//!
//! This process never captures input or owns a session. It reads the daemon's
//! configuration and identity, and will later talk to the daemon over a local
//! IPC socket. Keeping the UI out of the input path is deliberate: closing the
//! window must not interrupt keyboard/mouse sharing.

use std::fs::OpenOptions;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
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
// Session control (the UI only starts/stops the headless daemon)
// ---------------------------------------------------------------------------

/// What the control panel is allowed to know about a session. The UI never
/// owns the KVM session: it only remembers the PID it spawned and reads the
/// daemon's own view over the local HTTP plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionInfo {
    /// `GET /session` from a running `alfone serve`, if reachable.
    daemon: Option<DaemonSession>,
    /// Child process launched by this panel, if any.
    local: LocalSession,
    /// Where the child's stdout/stderr are appended.
    log_path: String,
    /// Resolved `alfone` executable.
    binary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonSession {
    role: String,
    state: u8,
    pin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalSession {
    running: bool,
    role: Option<String>,
    pid: Option<u32>,
    exit_code: Option<i32>,
}

struct TrackedProcess {
    child: Child,
    role: String,
}

/// Only the PID (and role) of a spawned child lives in the GUI process.
#[derive(Default)]
struct SessionProcesses(Mutex<Option<TrackedProcess>>);

impl SessionProcesses {
    fn observe(&self) -> LocalSession {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tracked) = slot.as_mut() {
            match tracked.child.try_wait() {
                Ok(Some(status)) => {
                    let role = tracked.role.clone();
                    let pid = tracked.child.id();
                    *slot = None;
                    return LocalSession {
                        running: false,
                        role: Some(role),
                        pid: Some(pid),
                        exit_code: status.code(),
                    };
                }
                Ok(None) => {
                    return LocalSession {
                        running: true,
                        role: Some(tracked.role.clone()),
                        pid: Some(tracked.child.id()),
                        exit_code: None,
                    };
                }
                Err(_) => {
                    *slot = None;
                }
            }
        }
        LocalSession {
            running: false,
            role: None,
            pid: None,
            exit_code: None,
        }
    }

    fn store(&self, tracked: TrackedProcess) -> Result<(), String> {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err("ya hay una sesión controlada por esta ventana".into());
        }
        *slot = Some(tracked);
        Ok(())
    }

    fn take(&self) -> Option<TrackedProcess> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Combined view used by every session command and by the polling effect.
async fn current_session_info(state: &SessionProcesses) -> Result<SessionInfo, String> {
    Ok(SessionInfo {
        daemon: daemon_session().await,
        local: state.observe(),
        log_path: session_log_path().display().to_string(),
        binary: alfone_binary().display().to_string(),
    })
}

/// `GET /session`; absent when no daemon (or a client) is running locally.
async fn daemon_session() -> Option<DaemonSession> {
    let (base_url, token) = ipc_endpoint();
    let response = reqwest::Client::new()
        .get(format!("{base_url}/session"))
        .bearer_auth(&token)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

/// POST to the graceful-shutdown endpoint of a running daemon.
async fn request_daemon_shutdown() -> Result<(), String> {
    let (base_url, token) = ipc_endpoint();
    let response = reqwest::Client::new()
        .post(format!("{base_url}/session/shutdown"))
        .bearer_auth(&token)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("no se pudo contactar con el daemon: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "el daemon rechazó la parada: {}",
            response.status()
        ));
    }
    Ok(())
}

/// Raw TCP probe: catches a daemon started outside the panel even when the
/// configured token would not authenticate us.
fn daemon_port_open() -> bool {
    let (base_url, _) = ipc_endpoint();
    let Ok(addr) = base_url
        .trim_start_matches("http://")
        .parse::<SocketAddr>()
    else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

#[tauri::command]
async fn session_info(state: tauri::State<'_, SessionProcesses>) -> Result<SessionInfo, String> {
    current_session_info(&state).await
}

/// Spawn `alfone serve`. The daemon owns the session from this point on.
#[tauri::command]
async fn start_server(
    state: tauri::State<'_, SessionProcesses>,
    pin: Option<String>,
    side: Option<String>,
) -> Result<SessionInfo, String> {
    if state.observe().running || daemon_port_open() {
        return Err(
            "ya hay una sesión en marcha; detenla antes de iniciar otra".into(),
        );
    }
    let side = side.unwrap_or_else(|| "right".into());
    if !matches!(side.as_str(), "left" | "right" | "top" | "bottom") {
        return Err(format!("lado no válido: {side}"));
    }
    let mut args = vec![
        "serve".to_string(),
        "--side".to_string(),
        side,
    ];
    if let Some(pin) = pin {
        let pin = pin.trim();
        if !pin.is_empty() {
            args.push("--pin".into());
            args.push(pin.into());
        }
    }
    let tracked = spawn_session("server", &args)?;
    state.store(tracked)?;
    current_session_info(&state).await
}

/// Spawn `alfone connect`. Refused while the local daemon is listening: one
/// machine cannot be server and client at the same time.
#[tauri::command]
async fn start_client(
    state: tauri::State<'_, SessionProcesses>,
    server: String,
    pin: String,
    fingerprint: Option<String>,
) -> Result<SessionInfo, String> {
    if state.observe().running {
        return Err("ya hay una sesión controlada por esta ventana".into());
    }
    if daemon_port_open() {
        return Err(
            "el daemon local está en marcha; detenlo antes de conectar como cliente".into(),
        );
    }
    let server = server.trim();
    if server.is_empty() {
        return Err("escribe la dirección del servidor (IP:24800)".into());
    }
    let mut args = vec!["connect".to_string(), "--server".to_string(), server.into()];
    let pin = pin.trim();
    if !pin.is_empty() {
        args.push("--pin".into());
        args.push(pin.into());
    }
    if let Some(fingerprint) = fingerprint {
        let fingerprint = fingerprint.trim();
        if !fingerprint.is_empty() {
            args.push("--fingerprint".into());
            args.push(fingerprint.into());
        }
    }
    let tracked = spawn_session("client", &args)?;
    state.store(tracked)?;
    current_session_info(&state).await
}

/// Graceful stop for the daemon; immediate kill for a client (it has no HTTP
/// endpoint). Only children spawned by this panel are killed.
#[tauri::command]
async fn stop_session(state: tauri::State<'_, SessionProcesses>) -> Result<SessionInfo, String> {
    let mut tracked = state.take();
    let mut errors = Vec::new();

    if let Some(process) = tracked.as_mut() {
        if process.role == "server" {
            if let Err(e) = request_daemon_shutdown().await {
                errors.push(e);
            }
            // Give the accept loop a moment to close the QUIC session cleanly.
            for _ in 0..20 {
                match process.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(e) => {
                        errors.push(format!("no se pudo esperar al daemon: {e}"));
                        break;
                    }
                }
            }
        }
        if matches!(process.child.try_wait(), Ok(None)) {
            if let Err(e) = process.child.kill() {
                errors.push(format!("no se pudo detener el proceso: {e}"));
            }
            let _ = process.child.wait();
        }
    } else if daemon_port_open() {
        if let Err(e) = request_daemon_shutdown().await {
            errors.push(e);
        }
    }

    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    current_session_info(&state).await
}

/// Launch an `alfone` child with both streams appended to a log file so the
/// panel never blocks on a pipe the user is not reading.
fn spawn_session(role: &str, args: &[String]) -> Result<TrackedProcess, String> {
    let binary = alfone_binary();
    let log_path = session_log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("no se pudo crear {}: {e}", parent.display()))?;
    }
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("no se pudo abrir {}: {e}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("no se pudo abrir el log de errores: {e}"))?;
    let child = Command::new(&binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| format!("no se pudo lanzar {}: {e}", binary.display()))?;
    Ok(TrackedProcess {
        child,
        role: role.to_string(),
    })
}

fn session_log_path() -> PathBuf {
    default_config_path()
        .parent()
        .map(|parent| parent.join("alfone-session.log"))
        .unwrap_or_else(|| std::env::temp_dir().join("alfone-session.log"))
}

/// Find the CLI next to the panel first, then in any `target/{debug,release}`
/// above it, then let the OS resolve it from PATH.
fn alfone_binary() -> PathBuf {
    if let Ok(value) = std::env::var("ALFONE_BIN") {
        if !value.trim().is_empty() {
            return PathBuf::from(value.trim());
        }
    }
    let name = if cfg!(windows) { "alfone.exe" } else { "alfone" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join(name);
            if sibling.exists() {
                return sibling;
            }
            for ancestor in parent.ancestors() {
                for profile in ["debug", "release"] {
                    let candidate = ancestor.join("target").join(profile).join(name);
                    if candidate.exists() {
                        return candidate;
                    }
                }
            }
        }
    }
    PathBuf::from(name)
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClipboardState {
    enabled: bool,
}

/// Read clipboard consent from the daemon when it is up, otherwise from the
/// saved config (the panel must not create a config just to read it).
#[tauri::command]
async fn clipboard_status() -> Result<ClipboardState, String> {
    let (base_url, token) = ipc_endpoint();
    let response = reqwest::Client::new()
        .get(format!("{base_url}/clipboard"))
        .bearer_auth(&token)
        .timeout(Duration::from_secs(2))
        .send()
        .await;
    if let Ok(response) = response {
        if response.status().is_success() {
            return response
                .json()
                .await
                .map_err(|e| format!("invalid clipboard response: {e}"));
        }
    }
    let path = default_config_path();
    let enabled = if path.exists() {
        Config::load_or_create(Some(path.as_path()))
            .map(|(config, _)| config.clipboard)
            .map_err(|e| e.to_string())?
    } else {
        true
    };
    Ok(ClipboardState { enabled })
}

/// Persist consent and, when possible, apply it to the live daemon.
#[tauri::command]
async fn set_clipboard(enabled: bool) -> Result<ClipboardState, String> {
    let (base_url, token) = ipc_endpoint();
    let response = reqwest::Client::new()
        .put(format!("{base_url}/clipboard"))
        .bearer_auth(&token)
        .json(&ClipboardState { enabled })
        .timeout(Duration::from_secs(5))
        .send()
        .await;
    if let Ok(response) = response {
        if response.status().is_success() {
            return Ok(ClipboardState { enabled });
        }
    }
    let path = default_config_path();
    let mut config = Config::load_or_create(Some(path.as_path()))
        .map(|(config, _)| config)
        .map_err(|e| e.to_string())?;
    config.clipboard = enabled;
    config.save_to(&path).map_err(|e| e.to_string())?;
    Ok(ClipboardState { enabled })
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
        .manage(SessionProcesses::default())
        .invoke_handler(tauri::generate_handler![
            status,
            init_device,
            new_pin,
            session_info,
            start_server,
            start_client,
            stop_session,
            health,
            clipboard_status,
            set_clipboard,
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

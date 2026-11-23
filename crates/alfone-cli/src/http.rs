//! Local HTTP control plane.
//!
//! `alfone serve` exposes a small REST-ish API on localhost so the Tauri
//! desktop UI (and `alfone doctor`) can inspect and reconfigure the device
//! without the UI being part of the daemon binary. The daemon keeps owning
//! the QUIC session; the HTTP plane is read-only state inspection plus layout
//! persistence for now.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use alfone_core::{Config, Layout, LayoutConfig, DEFAULT_HTTP_PORT};
use alfone_net::Identity;
use anyhow::Result;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum::routing::get;
use axum::Router;
use serde_json::json;
use tokio::net::TcpListener;

/// Session state of the daemon: 0 = idle, 1 = active input session.
pub const STATE_IDLE: u8 = 0;
/// A QUIC session is currently forwarding input.
pub const STATE_ACTIVE: u8 = 1;

#[derive(Debug, Clone)]
pub struct HttpState {
    /// Bearer token required by every endpoint.
    pub token: String,
    /// Config file the daemon persists layout changes to.
    pub config_path: PathBuf,
    /// 0 = idle, 1 = active.
    pub state: Arc<AtomicU8>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Health {
    pub ok: bool,
    pub state: u8,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub initialized: bool,
    pub config_path: String,
    pub device_name: String,
    pub device_id: String,
    pub platform: String,
    pub fingerprint: Option<String>,
    pub short_code: Option<String>,
    pub http_listen: String,
    pub state: u8,
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/layout", get(layout_get).put(layout_put))
        .with_state(state)
}

pub async fn serve(listener: TcpListener, state: HttpState) -> Result<()> {
    axum::serve(listener, router(state))
        .await
        .map_err(|e| anyhow::anyhow!("http control plane failed: {e}"))
}

fn authorized(state: &HttpState, headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return false;
    };
    token.len() == state.token.len() && token == state.token
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

fn server_error(body: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": body })),
    )
        .into_response()
}

async fn health(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let body = Health {
        ok: true,
        state: state.state.load(Ordering::SeqCst),
    };
    (StatusCode::OK, Json(body)).into_response()
}

async fn status(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let state = state;
    if !state.config_path.exists() {
        let body = StatusResponse {
            initialized: false,
            config_path: state.config_path.display().to_string(),
            device_name: "alfone".into(),
            device_id: "—".into(),
            platform: std::env::consts::OS.to_string(),
            fingerprint: None,
            short_code: None,
            http_listen: format!("127.0.0.1:{DEFAULT_HTTP_PORT}"),
            state: state.state.load(Ordering::SeqCst),
        };
        return (StatusCode::OK, Json(body)).into_response();
    }
    let Ok((config, _path)) = Config::load_or_create(Some(&state.config_path)) else {
        return server_error("could not load the config".into());
    };
    let identity_dir = state
        .config_path
        .parent()
        .map(|parent| parent.join("identity"))
        .unwrap_or_else(|| PathBuf::from("identity"));
    let identity = Identity::load_or_create(&identity_dir).ok();
    let body = StatusResponse {
        initialized: true,
        config_path: state.config_path.display().to_string(),
        device_name: config.device_name.clone(),
        device_id: config.device_id.clone(),
        platform: std::env::consts::OS.to_string(),
        fingerprint: identity.as_ref().map(|i| i.fingerprint_hex()),
        short_code: identity.as_ref().map(|i| i.short_code()),
        http_listen: config.server.http_listen.clone(),
        state: state.state.load(Ordering::SeqCst),
    };
    (StatusCode::OK, Json(body)).into_response()
}

async fn layout_get(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let Ok((config, _path)) = Config::load_or_create(Some(&state.config_path)) else {
        return server_error("could not load the config".into());
    };
    let Some(layout) = Layout::new(&config.layout).ok() else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "the stored layout is invalid" })),
        )
            .into_response();
    };
    (StatusCode::OK, Json(layout.snapshot())).into_response()
}

async fn layout_put(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(input): Json<LayoutConfig>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    if Layout::new(&input).is_err() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "the layout is invalid" })),
        )
            .into_response();
    }
    let mut config = Config::load_or_create(Some(&state.config_path))
        .map(|loaded| loaded.0)
        .unwrap_or_else(|_| Config::default());
    config.layout = input.clone();
    if let Err(e) = config.save_to(&state.config_path) {
        return server_error(format!("could not save the layout: {e}"));
    }
    (
        StatusCode::OK,
        Json(LayoutConfig::snapshot(&input)),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state(token: &str) -> HttpState {
        HttpState {
            token: token.into(),
            config_path: std::env::temp_dir().join(format!(
                "alfone-http-test-{}-{}",
                std::process::id(),
                token.len()
            )),
            state: Arc::new(AtomicU8::new(STATE_IDLE)),
        }
    }

    #[tokio::test]
    async fn health_rejects_missing_token() {
        let app = router(test_state("secret"));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn health_ok_with_token() {
        let app = router(test_state("secret"));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], true);
    }

    #[tokio::test]
    async fn layout_roundtrip() {
        let app = router(test_state("secret"));
        let put = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/layout")
                    .header("authorization", "Bearer secret")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"screens":[{"id":"local","name":"This computer","width":1920,"height":1080}],"primary":"local"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::OK);

        let get = app
            .oneshot(
                Request::builder()
                    .uri("/layout")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
    }
}

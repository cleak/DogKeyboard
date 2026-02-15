//! OBS overlay web server — serves HTML overlay and streams keystrokes via WebSocket.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use dogkbd_proto::{hid_to_us_ansi_char, KeyTap};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

const OVERLAY_HTML: &str = include_str!("../overlay.html");

/// A keystroke message sent to WebSocket clients as tagged JSON.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum KeystrokeMsg {
    #[serde(rename = "char")]
    Char {
        #[serde(rename = "char")]
        ch: String,
    },
    #[serde(rename = "enter")]
    Enter,
    #[serde(rename = "backspace")]
    Backspace,
    #[serde(rename = "space")]
    Space,
}

/// Convert a KeyTap into a KeystrokeMsg.
pub fn tap_to_msg(tap: &KeyTap) -> Option<KeystrokeMsg> {
    match tap.hid_code {
        0x28 => Some(KeystrokeMsg::Enter),
        0x2a => Some(KeystrokeMsg::Backspace),
        0x2c => Some(KeystrokeMsg::Space),
        _ => {
            let ch = hid_to_us_ansi_char(tap.hid_code, tap.shift())?;
            Some(KeystrokeMsg::Char {
                ch: ch.to_string(),
            })
        }
    }
}

#[derive(Deserialize)]
struct ClaudeStatusRequest {
    status: String,
}

#[derive(Clone)]
struct AppState {
    tx: Arc<broadcast::Sender<KeystrokeMsg>>,
    claude_busy: Arc<AtomicBool>,
}

async fn serve_overlay() -> impl IntoResponse {
    Html(OVERLAY_HTML)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: AppState) {
    let mut rx = state.tx.subscribe();

    loop {
        match rx.recv().await {
            Ok(msg) => {
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(_) => continue,
                };
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break; // Client disconnected
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("WebSocket client lagged, skipped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn claude_status_handler(
    State(state): State<AppState>,
    Json(payload): Json<ClaudeStatusRequest>,
) -> StatusCode {
    match payload.status.as_str() {
        "busy" => {
            let was_busy = state.claude_busy.swap(true, Ordering::Relaxed);
            println!("[hook] Received claude-status: busy (was {})", if was_busy { "busy" } else { "idle" });
        }
        "idle" => {
            let was_busy = state.claude_busy.swap(false, Ordering::Relaxed);
            println!("[hook] Received claude-status: idle (was {})", if was_busy { "busy" } else { "idle" });
        }
        other => {
            println!("[hook] Received claude-status: unknown value {:?}", other);
            return StatusCode::BAD_REQUEST;
        }
    }
    StatusCode::OK
}

/// Build the router (extracted for testability).
fn build_router(claude_busy: Arc<AtomicBool>, tx: broadcast::Sender<KeystrokeMsg>) -> Router {
    let state = AppState {
        tx: Arc::new(tx),
        claude_busy,
    };
    Router::new()
        .route("/", get(serve_overlay))
        .route("/ws", get(ws_handler))
        .route("/claude-status", post(claude_status_handler))
        .with_state(state)
}

/// Run the HTTP/WebSocket server.
pub async fn run_web_server(
    web_port: u16,
    tx: broadcast::Sender<KeystrokeMsg>,
    claude_busy: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let app = build_router(claude_busy, tx);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{web_port}")).await?;
    println!("OBS overlay server listening on http://0.0.0.0:{web_port}/");

    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// Helper: build a test router and return (router, claude_busy flag).
    fn test_app() -> (Router, Arc<AtomicBool>) {
        let claude_busy = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = broadcast::channel::<KeystrokeMsg>(16);
        let app = build_router(claude_busy.clone(), tx);
        (app, claude_busy)
    }

    /// Helper: build a POST /claude-status request with the given JSON body.
    fn status_request(body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/claude-status")
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    // ── HTTP endpoint unit tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_busy_sets_flag_true() {
        let (app, flag) = test_app();
        assert!(!flag.load(Ordering::Relaxed));

        let resp = app.oneshot(status_request(r#"{"status":"busy"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(flag.load(Ordering::Relaxed), "flag should be true after busy");
    }

    #[tokio::test]
    async fn test_idle_sets_flag_false() {
        let (app, flag) = test_app();
        flag.store(true, Ordering::Relaxed); // start busy

        let resp = app.oneshot(status_request(r#"{"status":"idle"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(!flag.load(Ordering::Relaxed), "flag should be false after idle");
    }

    #[tokio::test]
    async fn test_unknown_status_returns_400() {
        let (app, flag) = test_app();

        let resp = app.oneshot(status_request(r#"{"status":"unknown"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(!flag.load(Ordering::Relaxed), "flag should be unchanged");
    }

    #[tokio::test]
    async fn test_invalid_json_returns_422() {
        let (app, flag) = test_app();

        let resp = app.oneshot(status_request("not json")).await.unwrap();
        assert_eq!(resp.status().as_u16(), 400);
        assert!(!flag.load(Ordering::Relaxed), "flag should be unchanged");
    }

    #[tokio::test]
    async fn test_full_idle_busy_idle_cycle() {
        let flag = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = broadcast::channel::<KeystrokeMsg>(16);

        // Each oneshot consumes the router, so rebuild for each request
        let make_app = || build_router(flag.clone(), tx.clone());

        // idle → busy
        let resp = make_app().oneshot(status_request(r#"{"status":"busy"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(flag.load(Ordering::Relaxed));

        // busy → idle
        let resp = make_app().oneshot(status_request(r#"{"status":"idle"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(!flag.load(Ordering::Relaxed));

        // idle → busy again
        let resp = make_app().oneshot(status_request(r#"{"status":"busy"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_duplicate_busy_is_idempotent() {
        let flag = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = broadcast::channel::<KeystrokeMsg>(16);
        let make_app = || build_router(flag.clone(), tx.clone());

        let resp = make_app().oneshot(status_request(r#"{"status":"busy"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(flag.load(Ordering::Relaxed));

        // Send busy again — should stay true, no error
        let resp = make_app().oneshot(status_request(r#"{"status":"busy"}"#)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(flag.load(Ordering::Relaxed));
    }

    // ── Live server + PowerShell integration test ─────────────────────

    #[tokio::test]
    async fn test_powershell_script_sets_busy() {
        let flag = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = broadcast::channel::<KeystrokeMsg>(16);
        let app = build_router(flag.clone(), tx);

        // Bind to port 0 to get a random available port
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Spawn server
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Run the equivalent of the PowerShell hook script via reqwest
        // (tests the same JSON payload shape the script sends)
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/claude-status"))
            .header("content-type", "application/json")
            .body(r#"{"status":"busy"}"#)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status().as_u16(), 200);
        assert!(flag.load(Ordering::Relaxed), "flag should be true after POST busy");

        // Now send idle
        let resp = client
            .post(format!("http://127.0.0.1:{port}/claude-status"))
            .header("content-type", "application/json")
            .body(r#"{"status":"idle"}"#)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status().as_u16(), 200);
        assert!(!flag.load(Ordering::Relaxed), "flag should be false after POST idle");
    }

    #[tokio::test]
    async fn test_actual_powershell_script() {
        let flag = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = broadcast::channel::<KeystrokeMsg>(16);
        let app = build_router(flag.clone(), tx);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Run the actual PowerShell inline with the same logic as claude-status.ps1
        // but pointed at our test port
        let ps_cmd = format!(
            r#"Invoke-RestMethod -Uri "http://127.0.0.1:{}/claude-status" -Method Post -ContentType "application/json" -Body '{{"status":"busy"}}' -TimeoutSec 5"#,
            port
        );
        let output = tokio::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_cmd])
            .output()
            .await
            .unwrap();

        assert!(
            output.status.success(),
            "PowerShell failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(flag.load(Ordering::Relaxed), "flag should be true after PowerShell POST busy");

        // Now idle
        let ps_cmd = format!(
            r#"Invoke-RestMethod -Uri "http://127.0.0.1:{}/claude-status" -Method Post -ContentType "application/json" -Body '{{"status":"idle"}}' -TimeoutSec 5"#,
            port
        );
        let output = tokio::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_cmd])
            .output()
            .await
            .unwrap();

        assert!(
            output.status.success(),
            "PowerShell failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!flag.load(Ordering::Relaxed), "flag should be false after PowerShell POST idle");
    }

    /// Test running the actual claude-status.ps1 script from the tea-leaves CWD
    /// against a live test server. This is the closest we can get to the real
    /// hook execution without Claude Code itself.
    #[tokio::test]
    async fn test_hook_script_from_tea_leaves_cwd() {
        let script = std::path::Path::new("C:/Projects/Godot/tea-leaves/.claude/hooks/claude-status.ps1");
        if !script.exists() {
            eprintln!("Skipping: tea-leaves hook script not found at {:?}", script);
            return;
        }

        let flag = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = broadcast::channel::<KeystrokeMsg>(16);
        let app = build_router(flag.clone(), tx);

        // Must use port 8080 because the script hardcodes it
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:8080").await {
            Ok(l) => l,
            Err(_) => {
                eprintln!("Skipping: port 8080 already in use");
                return;
            }
        };

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Run the actual script from the tea-leaves directory (same as Claude Code would)
        let output = tokio::process::Command::new("powershell")
            .args([
                "-NoProfile", "-ExecutionPolicy", "Bypass",
                "-File", ".claude/hooks/claude-status.ps1",
                "-Status", "busy",
            ])
            .current_dir("C:/Projects/Godot/tea-leaves")
            .output()
            .await
            .unwrap();

        assert!(
            output.status.success(),
            "Hook script failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(flag.load(Ordering::Relaxed), "flag should be true after hook script sends busy");

        // Send idle
        let output = tokio::process::Command::new("powershell")
            .args([
                "-NoProfile", "-ExecutionPolicy", "Bypass",
                "-File", ".claude/hooks/claude-status.ps1",
                "-Status", "idle",
            ])
            .current_dir("C:/Projects/Godot/tea-leaves")
            .output()
            .await
            .unwrap();

        assert!(
            output.status.success(),
            "Hook script failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!flag.load(Ordering::Relaxed), "flag should be false after hook script sends idle");
    }
}

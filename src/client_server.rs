use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::process::Command;
use tracing::{info, warn};
use wakezilla_common::{RunScriptRequest, RunScriptResponse};

use crate::system;

/// Script output beyond this many bytes per stream is cut off in responses.
const MAX_SCRIPT_OUTPUT: usize = 4096;

#[derive(Clone)]
struct ClientState {
    allow_scripts: bool,
}

pub async fn start(port: u16, allow_scripts: bool) -> Result<()> {
    if allow_scripts {
        warn!("Port-forward scripts are enabled: requests to /scripts/run execute shell commands");
    }
    let app = Router::new()
        .route("/health", get(health_check))
        .route("/machines/turn-off", post(turn_off_machine))
        .route("/scripts/run", post(run_script_handler))
        .with_state(ClientState { allow_scripts });

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    info!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> impl IntoResponse {
    let status = serde_json::json!({ "status": "ok" });
    Json(status)
}

async fn turn_off_machine() -> impl IntoResponse {
    system::shutdown_machine();
    (
        axum::http::StatusCode::OK,
        "Shutting down this machine".to_string(),
    )
}

async fn run_script_handler(
    State(state): State<ClientState>,
    Json(request): Json<RunScriptRequest>,
) -> impl IntoResponse {
    if !state.allow_scripts {
        warn!(
            "Refusing {} script for port {}: scripts are disabled on this client",
            request.event, request.local_port
        );
        return (
            StatusCode::FORBIDDEN,
            "Scripts are disabled; start the client server with --allow-scripts \
             or WAKEZILLA__SERVER__ALLOW_SCRIPTS=true"
                .to_string(),
        )
            .into_response();
    }
    match run_script(&request).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to run script: {}", e),
        )
            .into_response(),
    }
}

fn shell_command(script: &str) -> Command {
    if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/C", script]);
        command
    } else {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }
}

fn truncated(output: &[u8]) -> String {
    let output = &output[..output.len().min(MAX_SCRIPT_OUTPUT)];
    String::from_utf8_lossy(output).into_owned()
}

/// Runs a port-forward script through the system shell. The script sees the
/// hook and the forward it belongs to in `WAKEZILLA_*` environment variables.
async fn run_script(request: &RunScriptRequest) -> std::io::Result<RunScriptResponse> {
    info!(
        "Running {} script for port forward {} -> {}",
        request.event, request.local_port, request.target_port
    );
    let child = shell_command(&request.script)
        .env("WAKEZILLA_EVENT", &request.event)
        .env("WAKEZILLA_LOCAL_PORT", request.local_port.to_string())
        .env("WAKEZILLA_TARGET_PORT", request.target_port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let timeout = Duration::from_secs(request.timeout_secs);
    let response = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(output) => {
            let output = output?;
            RunScriptResponse {
                exit_code: output.status.code(),
                timed_out: false,
                stdout: truncated(&output.stdout),
                stderr: truncated(&output.stderr),
            }
        }
        // Dropping the future kills the child (kill_on_drop).
        Err(_) => RunScriptResponse {
            exit_code: None,
            timed_out: true,
            stdout: String::new(),
            stderr: String::new(),
        },
    };
    if response.timed_out {
        warn!(
            "{} script for port {} killed after {:?}",
            request.event, request.local_port, timeout
        );
    } else {
        info!(
            "{} script for port {} exited with {:?}",
            request.event, request.local_port, response.exit_code
        );
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn health_check_returns_ok_json() {
        let response = health_check().await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    fn request(script: &str, timeout_secs: u64) -> RunScriptRequest {
        RunScriptRequest {
            event: "connect".into(),
            script: script.into(),
            local_port: 8080,
            target_port: 80,
            timeout_secs,
        }
    }

    #[tokio::test]
    async fn run_script_handler_refuses_when_scripts_disabled() {
        let response = run_script_handler(
            State(ClientState {
                allow_scripts: false,
            }),
            Json(request("true", 5)),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_script_passes_environment_and_captures_output() {
        let response = run_script(&request(
            "echo \"$WAKEZILLA_EVENT $WAKEZILLA_LOCAL_PORT $WAKEZILLA_TARGET_PORT\"; echo oops >&2; exit 3",
            5,
        ))
        .await
        .unwrap();
        assert_eq!(response.exit_code, Some(3));
        assert!(!response.timed_out);
        assert_eq!(response.stdout, "connect 8080 80\n");
        assert_eq!(response.stderr, "oops\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_script_kills_scripts_that_exceed_the_timeout() {
        let started = std::time::Instant::now();
        let response = run_script(&request("sleep 30", 1)).await.unwrap();
        assert!(response.timed_out);
        assert_eq!(response.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}

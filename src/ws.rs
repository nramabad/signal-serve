use axum::{
    extract::{State, Path, ws::{WebSocketUpgrade, WebSocket, Message}},
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use tokio::time::{interval, Duration};
use tracing::{info, debug};

use crate::SharedState;

const KEEPALIVE_SECS: u64 = 15;

pub async fn ws_receive_handler(
    ws: WebSocketUpgrade,
    Path(account): Path<String>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, account, state))
}

async fn handle_ws(socket: WebSocket, account: String, state: SharedState) {
    info!("WS /v1/receive/{} connected", account);

    // Subscribe to SSE broadcast channel before splitting socket
    let mut rx = {
        let guard = state.lock().await;
        guard.sse_tx.subscribe()
    };

    let (mut sink, mut stream) = socket.split();

    // Writer task: forward broadcast messages + periodic keepalive ping
    let writer = async {
        let mut ticker = interval(Duration::from_secs(KEEPALIVE_SECS));
        ticker.tick().await; // first tick fires immediately, skip
        loop {
            tokio::select! {
                biased;
                res = rx.recv() => {
                    match res {
                        Ok(data) => {
                            if sink.send(Message::Text(data.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = ticker.tick() => {
                    if sink.send(Message::Ping(b"keepalive".to_vec().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    };

    // Reader task: consume incoming frames (client pings auto-ponged by axum,
    // handle Close to terminate). Keeps the read half drained so axum doesn't
    // buffer indefinitely and the OS doesn't hit socket read timeouts.
    let reader = async {
        while let Some(Ok(msg)) = stream.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    };

    tokio::select! {
        _ = writer => {}
        _ = reader => {}
    }

    debug!("WS /v1/receive/{} disconnected", account);
}

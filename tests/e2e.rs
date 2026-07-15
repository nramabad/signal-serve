//! E2E integration tests for signal-serve HTTP API.
//!
//! Runs against a live signal-serve instance (linked to a registered Signal
//! account). Requires account, cannot run in CI without a real device link.
//! Feature-gated: `cargo test --features e2e` (e2e.rs only compiles with feature).
//!
//! Environment:
//!   E2E_TARGET_URL  — base URL (default http://127.0.0.1:8088)
//!   E2E_SELF_UUID   — optional; otherwise uses first contact from listContacts.
//!
//! Skipped if signal-serve not reachable. Manual / router runs only.
//! NOTE: No hardcoded phone numbers, UUIDs, or IPs. All from env vars.

#![cfg(feature = "e2e")]

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{E2eClient, self_uuid};
use serde_json::{json, Value};

/// Skip test if signal-serve not reachable.
async fn client_or_skip() -> Option<E2eClient> {
    let c = E2eClient::new();
    let ok = async {
        c.health("/v1/health").await == reqwest::StatusCode::NO_CONTENT
    }
    .await;
    if ok {
        Some(c)
    } else {
        eprintln!("signal-serve not reachable at {} — skipping", c.base_url);
        None
    }
}

/// Resolve self UUID from env or first listContacts entry.
async fn resolve_self(c: &E2eClient) -> String {
    if let Some(u) = self_uuid() {
        return u;
    }
    eprintln!("E2E_SELF_UUID unset — using first contact as self");
    c.first_contact_uuid().await
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::test]
async fn health_returns_204() {
    let c = match client_or_skip().await {
        Some(c) => c,
        None => return,
    };
    assert_eq!(c.health("/v1/health").await, reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        c.health("/api/v1/check").await,
        reqwest::StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn list_contacts_returns_array() {
    let c = match client_or_skip().await {
        Some(c) => c,
        None => return,
    };
    let result = c.rpc_ok("listContacts", json!({})).await;
    let contacts = result["contacts"]
        .as_array()
        .expect("listContacts result missing contacts array");
    assert!(!contacts.is_empty(), "no contacts in store");
    for c in contacts {
        assert!(c["uuid"].as_str().is_some(), "contact missing uuid: {c}");
    }
}

#[tokio::test]
async fn send_message_to_self() {
    let c = match client_or_skip().await {
        Some(c) => c,
        None => return,
    };
    let self_id = resolve_self(&c).await;
    let msg = format!("e2e send test {}", now_ms());
    let result = c.send(&self_id, &msg).await;
    let results = result["results"]
        .as_array()
        .expect("send result missing results array");
    assert!(!results.is_empty(), "send returned no results: {result}");
    let status = results[0]["status"].as_str().expect("missing status");
    assert_eq!(status, "sent", "send failed: {result}");
    assert!(result["timestamp"].as_u64().is_some(), "missing timestamp");
}

#[tokio::test]
async fn send_typing_returns_timestamp() {
    let c = match client_or_skip().await {
        Some(c) => c,
        None => return,
    };
    let self_id = resolve_self(&c).await;
    let result = c
        .rpc_ok("sendTyping", json!({"recipient": [self_id]}))
        .await;
    assert!(
        result["timestamp"].as_u64().is_some(),
        "sendTyping missing timestamp: {result}"
    );
}

#[tokio::test]
async fn send_reaction_returns_timestamp() {
    let c = match client_or_skip().await {
        Some(c) => c,
        None => return,
    };
    let self_id = resolve_self(&c).await;
    // Reactions require a target message. Use a fresh send targetTimestamp.
    let send_result = c.send(&self_id, "e2e reaction target").await;
    let target_ts = send_result["timestamp"].as_u64().expect("missing send ts");
    let result = c
        .rpc_ok(
            "sendReaction",
            json!({
                "recipient": [self_id],
                "emoji": "👍",
                "targetAuthor": self_id,
                "targetTimestamp": target_ts,
                "remove": false,
            }),
        )
        .await;
    assert!(
        result["timestamp"].as_u64().is_some(),
        "sendReaction missing timestamp: {result}"
    );
}

#[tokio::test]
async fn unknown_method_returns_error() {
    let c = match client_or_skip().await {
        Some(c) => c,
        None => return,
    };
    let err = c.rpc_error("nonexistentMethod", json!({}), -1).await;
    let msg = err["message"].as_str().expect("missing error message");
    assert!(
        msg.contains("Unknown method"),
        "unexpected error message: {msg}"
    );
}

#[tokio::test]
async fn missing_recipient_returns_error() {
    let c = match client_or_skip().await {
        Some(c) => c,
        None => return,
    };
    // sendAttachments requires non-empty recipient array.
    let err = c
        .rpc_error(
            "sendAttachments",
            json!({"recipient": [], "attachments": [], "message": "x"}),
            -1,
        )
        .await;
    let msg = err["message"].as_str().expect("missing error message");
    assert!(
        msg.contains("recipient"),
        "expected recipient-related error, got: {msg}"
    );
}

/// Requires external phone to send a message to the linked account within 30s.
/// Manually triggered: cargo test --features e2e sse_receives_data_message -- --ignored
#[tokio::test]
#[ignore = "requires external sender — run with --ignored and send a message from phone"]
async fn sse_receives_data_message() {
    let c = match client_or_skip().await {
        Some(c) => c,
        None => return,
    };
    let self_id = resolve_self(&c).await;
    let url = format!("{}/api/v1/events?account={}", c.base_url, self_id);

    // Stream SSE for up to 30s, find a dataMessage envelope.
    let timeout = Duration::from_secs(30);
    let deadline = SystemTime::now() + timeout;
    let resp = reqwest::get(&url).await.expect("SSE connect failed");
    assert!(resp.status().is_success(), "SSE connect status: {}", resp.status());

    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut got_data_message = false;
    while SystemTime::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                // Parse SSE: lines prefixed with "data:"
                for line in buf.lines() {
                    if let Some(data) = line.strip_prefix("data:") {
                        if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                            if v["envelope"]["dataMessage"].is_object() {
                                got_data_message = true;
                                let msg = v["envelope"]["dataMessage"]["message"]
                                    .as_str()
                                    .unwrap_or("");
                                eprintln!("received DataMessage: {msg}");
                                break;
                            }
                        }
                    }
                }
                buf.clear();
                if got_data_message {
                    break;
                }
            }
            _ => continue,
        }
    }
    assert!(got_data_message, "no DataMessage received within {timeout:?}");
}
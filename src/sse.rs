use axum::{extract::State, response::sse::{Event, Sse}};
use base64::Engine;
use presage::{
    libsignal_service::content::ContentBody,
    model::messages::Received,
};
use std::time::Duration;
use futures::StreamExt;
use serde_json::json;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

use crate::SharedState;

pub async fn events_handler(
    State(state): State<SharedState>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let account = query.get("account").cloned().unwrap_or_default();
    debug!("SSE client connected for account={}", account);

    let mut rx = { state.lock().await.sse_tx.subscribe() };

    // Manual stream from broadcast receiver
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(data) => yield Ok(Event::default().data(data)),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    debug!("SSE client lagged by {} messages", n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(30))
                .text("keepalive"),
        )
}

pub async fn run_message_listener(state: SharedState) -> anyhow::Result<()> {
    // Refresh phone cache on startup
    {
        let mut guard = state.lock().await;
        if let Err(e) = guard.refresh_contact_phones().await {
            error!("Failed to refresh contact phones: {:?}", e);
        }
    }

    loop {
        let mut stream = {
            let mut guard = state.lock().await;
            match guard.manager.receive_messages().await {
                Ok(s) => s,
                Err(e) => {
                    error!("receiver err: {:?}", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
        };

        info!("Message listener started");
        while let Some(msg) = stream.next().await {
            // Resolve sender UUID → E164 phone for SSE source field
            let source_phone = if let Received::Content(c) = &msg {
                let guard = state.lock().await;
                guard.phone_for_service_id(&c.metadata.sender)
            } else {
                None
            };
            // Refresh phone cache when contacts sync
            if matches!(&msg, Received::Contacts) {
                let mut guard = state.lock().await;
                if let Err(e) = guard.refresh_contact_phones().await {
                    error!("Failed to refresh contact phones: {:?}", e);
                }
            }
            let data = serde_json::to_string(&received_to_sse_envelope(&msg)).unwrap_or_default();

            // Patch source field to E164 if resolved
            let resolved = if let Some(ref phone) = source_phone {
                let mut env: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
                if let Some(obj) = env.get_mut("envelope") {
                    if let Some(e) = obj.as_object_mut() {
                        e.insert("source".into(), json!(phone));
                        e.insert("sourceUuid".into(), json!(phone));
                    }
                }
                serde_json::to_string(&env).unwrap_or(data)
            } else {
                data
            };

            // Cache AttachmentPointers for getAttachment RPC (client_uuid → pointer)
            if let Received::Content(c) = &msg {
                if let ContentBody::DataMessage(d) = &c.body {
                    let mut guard = state.lock().await;
                    for a in &d.attachments {
                        if let Some(uuid) = &a.client_uuid {
                            guard.pending_attachments.insert(uuid.clone(), a.clone());
                        }
                    }
                    let _ = guard.sse_tx.send(resolved);
                    continue;
                }
            }
            let _ = state.lock().await.sse_tx.send(resolved);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn received_to_sse_envelope(msg: &Received) -> serde_json::Value {
    match msg {
        Received::QueueEmpty => json!({"envelope": {"queueEmpty": true}}),
        Received::Contacts => json!({"envelope": {"contactsSynced": true}}),
        Received::Content(c) => {
            let src = c.metadata.sender.service_id_string();
            let ts = c.metadata.timestamp.timestamp_millis();

            let (dm, sm) = match &c.body {
                ContentBody::DataMessage(d) => {
                    let mut m = json!({"timestamp": ts});
                    if let Some(b) = &d.body { m["message"] = json!(b); }
                    if !d.attachments.is_empty() {
                        let atts: Vec<serde_json::Value> = d.attachments.iter().map(|a| {
                            let id = a.client_uuid.as_ref()
                                .map(|u| base64::engine::general_purpose::STANDARD.encode(u))
                                .unwrap_or_default();
                            json!({
                                "id": id,
                                "contentType": a.content_type.as_deref().unwrap_or("application/octet-stream"),
                                "fileName": a.file_name,
                                "size": a.size,
                                "caption": a.caption,
                                "width": a.width,
                                "height": a.height,
                                "flags": a.flags,
                                "cdnNumber": a.cdn_number,
                            })
                        }).collect();
                        m["attachments"] = json!(atts);
                    }
                    (Some(m), None)
                }
                ContentBody::SynchronizeMessage(s) => {
                    let mut m = json!({});
                    if let Some(sent) = &s.sent {
                        if let Some(msg) = &sent.message {
                            if let Some(b) = &msg.body { m["message"] = json!(b); }
                            if !msg.attachments.is_empty() {
                                let atts: Vec<serde_json::Value> = msg.attachments.iter().map(|a| {
                                    let id = a.client_uuid.as_ref()
                                        .map(|u| base64::engine::general_purpose::STANDARD.encode(u))
                                        .unwrap_or_default();
                                    json!({
                                        "id": id,
                                        "contentType": a.content_type.as_deref().unwrap_or("application/octet-stream"),
                                        "fileName": a.file_name,
                                        "size": a.size,
                                        "caption": a.caption,
                                        "width": a.width,
                                        "height": a.height,
                                        "flags": a.flags,
                                        "cdnNumber": a.cdn_number,
                                    })
                                }).collect();
                                m["attachments"] = json!(atts);
                            }
                        }
                    }
                    (None, Some(m))
                }
                ContentBody::TypingMessage(_) => return json!({"envelope": {"source": src, "sourceUuid": src, "timestamp": ts, "typingMessage": {"action": 1}}}),
                _ => return json!({"envelope": {"source": src, "sourceUuid": src, "timestamp": ts}}),
            };

            let mut e = json!({"source": src, "sourceUuid": src, "timestamp": ts});
            if let Some(d) = dm { e["dataMessage"] = d; }
            if let Some(s) = sm { e["syncMessage"] = s; }
            json!({"envelope": e})
        }
    }
}
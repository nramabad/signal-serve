use crate::AppState;
use base64::Engine;
use presage::{
    libsignal_service::{
        content::{ContentBody, DataMessage},
        protocol::{Aci, ServiceId},
    },
    store::ContentsStore,
};
use presage::libsignal_service::prelude::Uuid;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

/// Find a contact by recipient address. Tries UUID parse first (hermes passes
/// recipient UUID), falls back to E164 phone substring match. Returns None if
/// neither matches — caller logs "Contact not found".
fn find_contact<'a>(contacts: &'a [presage::model::contacts::Contact], addr: &str) -> Option<&'a presage::model::contacts::Contact> {
    // UUID match: hermes sends recipient as bare UUID string.
    let addr_clean = addr.strip_prefix('+').unwrap_or(addr);
    if let Ok(u) = Uuid::parse_str(addr_clean) {
        if let Some(c) = contacts.iter().find(|c| c.uuid == u) {
            return Some(c);
        }
    }
    // Phone match: E164 substring test (legacy behavior).
    contacts.iter().find(|c| {
        c.phone_number.as_ref().map(|p| format!("{}", p).contains(addr_clean)).unwrap_or(false)
    })
}

pub async fn dispatch(state: &mut AppState, req: &Value) -> Value {
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let rpc_id = req.get("id").cloned().unwrap_or(json!(null));

    let result = match method {
        "send" => handle_send(state, &params).await,
        "sendTyping" => handle_send_typing(state, &params).await,
        "sendReaction" => handle_send_reaction(state, &params).await,
        "listContacts" => handle_list_contacts(state).await,
        "getAttachment" => handle_get_attachment(state, &params).await,
        "sendAttachments" | "send_multiple_images" => handle_send_attachments(state, &params).await,
        other => {
            warn!("Unknown RPC method: {}", other);
            Err(anyhow::anyhow!("Unknown method: {}", other))
        }
    };

    let mut resp = json!({"jsonrpc": "2.0", "id": rpc_id});
    match result {
        Ok(val) => { resp["result"] = val; }
        Err(e) => { resp["error"] = json!({"message": format!("{}", e), "code": -1}); }
    }
    resp
}

fn now_ts() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn msg_text(params: &serde_json::Map<String, Value>) -> String {
    params.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn recipients(params: &serde_json::Map<String, Value>) -> Vec<String> {
    params.get("recipient").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().filter_map(|v| v.as_str().map(String::from)).collect()
    }).unwrap_or_default()
}

fn group_id_bytes(params: &serde_json::Map<String, Value>) -> Option<Vec<u8>> {
    params.get("groupId").and_then(|v| v.as_str()).map(|s| s.as_bytes().to_vec())
}

async fn handle_send(state: &mut AppState, params: &serde_json::Map<String, Value>) -> anyhow::Result<Value> {
    let _account = params.get("account").and_then(|v| v.as_str());
    let text = msg_text(params);
    let ts = now_ts();

    if let Some(gid) = group_id_bytes(params) {
        let mut dm = DataMessage::default();
        dm.body = Some(text);
        state.manager.send_message_to_group(&gid, ContentBody::DataMessage(dm), ts).await?;
        return Ok(json!({"timestamp": ts, "results": [{"recipient": "group", "status": "sent"}]}));
    } else {
        let recip = recipients(params);
        let store = state.manager.store();
        let contacts: Vec<_> = store.contacts().await?
            .filter_map(|c| c.ok())
            .collect();

        let mut results: Vec<Value> = Vec::with_capacity(recip.len());
        for phone in &recip {
            if let Some(c) = find_contact(&contacts, phone) {
                let srv_id = ServiceId::Aci(Aci::from(c.uuid));
                let mut dm = DataMessage::default();
                dm.body = Some(text.clone());
                state.manager.send_message(srv_id, ContentBody::DataMessage(dm), ts).await?;
                debug!("Sent to {}", c.name);
                results.push(json!({"recipient": phone, "status": "sent"}));
            } else {
                warn!("Contact not found: {}", phone);
                results.push(json!({"recipient": phone, "status": "failed", "error": "contact not found"}));
            }
        }
        // Non-empty results tells hermes send succeeded (backoff watch).
        Ok(json!({"timestamp": ts, "results": results}))
    }
}

async fn handle_send_typing(state: &mut AppState, params: &serde_json::Map<String, Value>) -> anyhow::Result<Value> {
    let ts = now_ts();
    let recip = recipients(params);
    if recip.is_empty() { return Ok(json!({"timestamp": ts})); }

    let tm = presage::libsignal_service::proto::TypingMessage {
        timestamp: Some(ts),
        action: Some(presage::libsignal_service::proto::typing_message::Action::Started as i32),
        group_id: None,
    };
    let body = ContentBody::TypingMessage(tm);

    let store = state.manager.store();
    let contacts: Vec<_> = store.contacts().await?
        .filter_map(|c| c.ok())
        .collect();

    for phone in &recip {
        if let Some(c) = find_contact(&contacts, phone) {
            let srv_id = ServiceId::Aci(Aci::from(c.uuid));
            state.manager.send_message(srv_id, body.clone(), ts).await?;
            debug!("Typing indicator sent to {}", c.name);
        } else {
            warn!("Contact not found for typing: {}", phone);
        }
    }

    Ok(json!({"timestamp": ts}))
}

async fn handle_send_reaction(state: &mut AppState, params: &serde_json::Map<String, Value>) -> anyhow::Result<Value> {
    let ts = now_ts();
    let emoji = msg_text(params);
    let target_author = params.get("targetAuthor").and_then(|v| v.as_str()).unwrap_or("");
    let target_timestamp = params.get("targetTimestamp").and_then(|v| v.as_u64()).unwrap_or_default();

    let store = state.manager.store();
    let contacts: Vec<_> = store.contacts().await?
        .filter_map(|c| c.ok())
        .collect();

    // Resolve targetAuthor (E164) → UUID
    let target_uuid = contacts.iter()
        .find(|c| {
            let author_clean = target_author.strip_prefix('+').unwrap_or(target_author);
            c.phone_number.as_ref().map(|p| format!("{}", p).contains(author_clean)).unwrap_or(false)
        })
        .map(|c| c.uuid);

    let reaction = presage::libsignal_service::proto::data_message::Reaction {
        emoji: Some(emoji),
        remove: Some(false),
        target_author_aci: target_uuid.map(|u| u.to_string()),
        target_sent_timestamp: Some(target_timestamp),
        target_author_aci_binary: target_uuid.map(|u| u.as_bytes().to_vec()),
    };

    let mut dm = DataMessage::default();
    dm.reaction = Some(reaction);
    let body = ContentBody::DataMessage(dm);

    let recip = recipients(params);
    for phone in &recip {
        if let Some(c) = find_contact(&contacts, phone) {
            let srv_id = ServiceId::Aci(Aci::from(c.uuid));
            state.manager.send_message(srv_id, body.clone(), ts).await?;
            debug!("Reaction sent to {}", c.name);
        } else {
            warn!("Contact not found for reaction: {}", phone);
        }
    }

    Ok(json!({"timestamp": ts}))
}

async fn handle_list_contacts(state: &mut AppState) -> anyhow::Result<Value> {
    let store = state.manager.store();
    let contacts: Vec<_> = store.contacts().await?
        .filter_map(|c| c.ok())
        .collect();

    let contacts_json: Vec<Value> = contacts.iter().map(|c| {
        json!({
            "uuid": format!("{}", c.uuid),
            "number": c.phone_number.as_ref().map(|p| format!("{}", p)).unwrap_or_default(),
            "name": c.name,
        })
    }).collect();

    Ok(json!({"contacts": contacts_json}))
}

async fn handle_get_attachment(state: &mut AppState, params: &serde_json::Map<String, Value>) -> anyhow::Result<Value> {
    let attachment_id = params.get("attachmentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing attachmentId"))?;

    let key = base64::engine::general_purpose::STANDARD.decode(attachment_id)?;

    let pointer = state.pending_attachments.get(&key)
        .ok_or_else(|| anyhow::anyhow!("attachment not found in cache — may have expired or never arrived"))?
        .clone();

    let bytes = state.manager.get_attachment(&pointer).await?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

    Ok(json!({
        "data": b64,
        "contentType": pointer.content_type.unwrap_or_else(|| "application/octet-stream".to_string()),
        "size": bytes.len(),
    }))
}

async fn handle_send_attachments(state: &mut AppState, params: &serde_json::Map<String, Value>) -> anyhow::Result<Value> {
    let recip = recipients(params);
    if recip.is_empty() {
        return Err(anyhow::anyhow!("missing or empty recipient array"));
    }

    let attachments_arr = params.get("attachments")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing attachments array"))?;

    if attachments_arr.is_empty() {
        return Err(anyhow::anyhow!("attachments array is empty"));
    }

    let mut specs = Vec::new();
    let mut datas = Vec::new();

    for att in attachments_arr {
        let b64_bytes = att.get("bytes")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("each attachment needs 'bytes' field (base64)"))?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64_bytes)?;

        let content_type = att.get("contentType")
            .and_then(|v| v.as_str())
            .unwrap_or("application/octet-stream")
            .to_string();

        let filename = att.get("filename").and_then(|v| v.as_str()).map(String::from);
        let caption = att.get("caption").and_then(|v| v.as_str()).map(String::from);

        let width: Option<u32> = att.get("width").and_then(|v| v.as_u64()).map(|v| v as u32);
        let height: Option<u32> = att.get("height").and_then(|v| v.as_u64()).map(|v| v as u32);

        specs.push(presage::libsignal_service::sender::AttachmentSpec {
            content_type,
            length: bytes.len(),
            file_name: filename,
            preview: None,
            voice_note: None,
            borderless: None,
            width,
            height,
            caption,
            blur_hash: None,
        });
        datas.push(bytes);
    }

    let upload_results = state.manager.upload_attachments(
        specs.into_iter().zip(datas.into_iter()).collect()
    ).await?;

    let mut pointers = Vec::new();
    for r in &upload_results {
        match r {
            Ok(p) => pointers.push(p.clone()),
            Err(e) => warn!("Attachment upload failed: {}", e),
        }
    }

    if pointers.is_empty() {
        return Err(anyhow::anyhow!("all attachment uploads failed"));
    }

    let ts = now_ts();
    let text = msg_text(params);
    let store = state.manager.store();
    let contacts: Vec<_> = store.contacts().await?
        .filter_map(|c| c.ok())
        .collect();

    let mut results = Vec::new();
    for phone in &recip {
        let phone_clean = phone.strip_prefix('+').unwrap_or(phone);
        if let Some(c) = contacts.iter().find(|c| {
            c.phone_number.as_ref().map(|p| format!("{}", p).contains(phone_clean)).unwrap_or(false)
        }) {
            let srv_id = ServiceId::Aci(Aci::from(c.uuid));
            let mut dm = DataMessage::default();
            dm.body = if text.is_empty() { None } else { Some(text.clone()) };
            dm.attachments = pointers.clone();

            state.manager.send_message(
                srv_id,
                ContentBody::DataMessage(dm),
                ts,
            ).await?;

            debug!("Sent {} attachments to {}", pointers.len(), c.name);
            results.push(json!({"recipient": phone, "status": "sent"}));
        } else {
            warn!("Contact not found: {}", phone);
            results.push(json!({"recipient": phone, "status": "not_found"}));
        }
    }

    Ok(json!({"timestamp": now_ts(), "results": results}))
}
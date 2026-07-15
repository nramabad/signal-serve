use std::env;

use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

/// Target URL for e2e tests. Defaults to localhost. Override via env:
///   E2E_TARGET_URL=http://192.168.8.1:8088
pub fn target_url() -> String {
    env::var("E2E_TARGET_URL").unwrap_or_else(|_| "http://127.0.0.1:8088".into())
}

/// Self UUID for send-to-self tests. Override via env:
///   E2E_SELF_UUID=12345678-1234-1234-1234-123456789012
pub fn self_uuid() -> Option<String> {
    env::var("E2E_SELF_UUID").ok()
}

pub struct E2eClient {
    pub client: Client,
    pub base_url: String,
}

impl E2eClient {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            base_url: target_url(),
        }
    }

    /// GET /v1/health or /api/v1/check → expect 204 No Content.
    pub async fn health(&self, path: &str) -> StatusCode {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .send()
            .await
            .expect("health request failed");
        resp.status()
    }

    /// POST /api/v1/rpc with method + params → raw JSON response Value.
    /// id is auto-generated.
    pub async fn rpc(&self, method: &str, params: Value) -> Value {
        let id = format!("e2e_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos());
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });
        let resp = self
            .client
            .post(format!("{}/api/v1/rpc", self.base_url))
            .json(&payload)
            .send()
            .await
            .expect("RPC request failed");
        resp.json().await.expect("RPC response JSON parse failed")
    }

    /// Returns the RPC result value, panicking on error.
    pub async fn rpc_ok(&self, method: &str, params: Value) -> Value {
        let resp = self.rpc(method, params).await;
        if resp.get("error").is_some() {
            panic!(
                "RPC {} returned error: {}",
                method,
                resp["error"]["message"].as_str().unwrap_or("?")
            );
        }
        resp["result"].clone()
    }

    /// Assert RPC returned an error with expected code.
    pub async fn rpc_error(&self, method: &str, params: Value, expect_code: i64) -> Value {
        let resp = self.rpc(method, params).await;
        let err = resp
            .get("error")
            .unwrap_or_else(|| panic!("expected error for method={method}, got {resp}"));
        let code = err["code"].as_i64().unwrap_or(-999);
        assert_eq!(code, expect_code, "error code mismatch: {resp}");
        err.clone()
    }

    /// Syntactic sugar: POST send.
    pub async fn send(&self, recipient_uuid: &str, message: &str) -> Value {
        self.rpc_ok(
            "send",
            json!({"recipient": [recipient_uuid], "message": message}),
        )
        .await
    }

    /// Extract first contact UUID from listContacts for env-less self-uuid.
    pub async fn first_contact_uuid(&self) -> String {
        let result = self.rpc_ok("listContacts", json!({})).await;
        let contacts = result["contacts"]
            .as_array()
            .expect("listContacts result has no contacts array");
        assert!(!contacts.is_empty(), "no contacts in store — link device first");
        contacts[0]["uuid"]
            .as_str()
            .expect("contact missing uuid field")
            .to_string()
    }
}
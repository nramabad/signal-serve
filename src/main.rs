mod rpc;
mod sse;
mod ws;

use std::collections::HashMap;
use std::sync::Arc;
use std::path::Path;

use anyhow::Context;
use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use clap::{Parser, Subcommand};
use presage::{
    libsignal_service::configuration::SignalServers,
    manager::{Manager, Registered},
    model::identity::OnNewIdentity,
};
use presage::store::ContentsStore;
use presage_store_sqlite::SqliteStore;
use serde_json::Value;
use tokio::sync::{broadcast, Mutex};
use tracing::info;

type SharedState = Arc<Mutex<AppState>>;

struct AppState {
    manager: Manager<SqliteStore, Registered>,
    sse_tx: broadcast::Sender<String>,
    pending_attachments: HashMap<Vec<u8>, presage::libsignal_service::proto::AttachmentPointer>,
    /// UUID bytes → E164 phone string cache for SSE source resolution
    contact_phones: HashMap<Vec<u8>, String>,
}

impl AppState {
    fn new(manager: Manager<SqliteStore, Registered>, sse_tx: broadcast::Sender<String>) -> Self {
        Self {
            manager,
            sse_tx,
            pending_attachments: HashMap::new(),
            contact_phones: HashMap::new(),
        }
    }

    /// Refresh in-memory phone lookup cache from SQLite contacts store.
    /// Called on startup and on `Received::Contacts` event.
    async fn refresh_contact_phones(&mut self) -> anyhow::Result<()> {
        let store = self.manager.store();
        let contacts = store.contacts().await?;
        for c in contacts {
            let c = c?;
            if let Some(phone) = &c.phone_number {
                let uuid = c.uuid;
                self.contact_phones.insert(uuid.as_bytes().to_vec(), format!("{}", phone));
            }
        }
        Ok(())
    }

    /// Look up E164 phone for a ServiceId using in-memory cache.
    fn phone_for_service_id(&self, sender: &presage::libsignal_service::protocol::ServiceId) -> Option<String> {
        match sender {
            presage::libsignal_service::protocol::ServiceId::Aci(a) => {
                use presage::libsignal_service::prelude::Uuid;
                let u: Uuid = Uuid::from(*a);
                let key: &[u8] = u.as_bytes();
                self.contact_phones.get(key).cloned()
            }
            _ => None,
        }
    }
}

#[derive(Parser)]
#[command(name = "signal-serve", about = "Pure Rust Signal REST API daemon")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Link as secondary device
    Link {
        /// Path to store directory
        #[arg(long, default_value = "/home/.local/share/signal-cli")]
        store: String,
    },
    /// Run HTTP server
    Serve {
        /// Listen address
        #[arg(long, default_value = "127.0.0.1:8088")]
        listen: String,
        /// Path to store directory
        #[arg(long, default_value = "/home/.local/share/signal-cli")]
        store: String,
    },
}

fn store_url(path: &str) -> String {
    let abs = Path::new(path).join("signal-serve.db");
    format!("sqlite://{}", abs.display())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "signal_serve=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Link { store } => cmd_link(&store).await,
        Commands::Serve { listen, store } => cmd_serve(&listen, &store).await,
    }
}

async fn cmd_link(store_path: &str) -> anyhow::Result<()> {
    info!("Linking secondary device...");

    let url = store_url(store_path);
    let store = SqliteStore::open(&url, OnNewIdentity::Trust)
        .await
        .context("Failed to open store")?;

    let (tx, rx) = futures::channel::oneshot::channel();
    let data_dir = Path::new(store_path).to_path_buf();

    // future::join runs both branches on same thread (Manager is !Send).
    // link_secondary_device sends URL via oneshot, then blocks for phone scan.
    // We receive url_rx -> print QR -> join returns when scan completes.
    let link = Manager::link_secondary_device(
        store,
        SignalServers::Production,
        "signal-serve".to_string(),
        tx,
    );
    let pump = async {
        let url = rx.await.map_err(|_| anyhow::anyhow!("URL channel closed"))?;
        println!("\n==============================================");
        println!("        SIGNAL LINK DEVICE");
        println!("==============================================");
        println!("\nProvisioning URL:");
        println!("  {}", url);
        println!("\nOpen Signal mobile > Linked Devices > Scan this QR:");
        println!("==============================================\n");

        eprintln!("SIGNAL_LINK_URL={}", url);

        // Write URL and HTML to data dir (volume-mounted, survives container exit)
        let _ = std::fs::write(data_dir.join("link-url.txt"), url.to_string().as_bytes());
        let html = format!(
            r#"<!DOCTYPE html><html><head><title>Signal Link</title><meta charset="utf-8"/></head><body>
<h2>Scan with Signal</h2>
<p>Provisioning URL:</p>
<pre style="word-break:break-all;white-space:pre-wrap;background:#eee;padding:8px">{url}</pre>
<p>Use Phone → Signal → Linked Devices → Scan</p>
<script src="https://cdn.jsdelivr.net/npm/qrcodejs@1.0.0/qrcode.min.js"></script>
<div id="qrcode"></div>
<script>new QRCode(document.getElementById("qrcode"), "{url}");</script>
</body></html>"#,
        );
        let _ = std::fs::write(data_dir.join("link-qr.html"), html.as_bytes());
        anyhow::Result::Ok(())
    };

    let (link_result, pump_result): (_, anyhow::Result<()>) =
        futures::future::join(link, pump).await;
    let _manager = match link_result {
        Ok(m) => m,
        Err(e) => {
            eprintln!("LINK ERROR: {:#}", e);
            return Err(e.into());
        }
    };
    pump_result?;

    info!("Device linked successfully!");
    println!("✓ Device linked");
    Ok(())
}

async fn cmd_serve(listen: &str, store_path: &str) -> anyhow::Result<()> {
    info!("Loading store from {}", store_path);
    let url = store_url(store_path);
    let store = SqliteStore::open(&url, OnNewIdentity::Trust)
        .await
        .context("Failed to open store")?;

    info!("Loading registered account...");
    let manager = Manager::load_registered(store)
        .await
        .context("Failed to load registered manager — link first via `signal-serve link`")?;

    let (sse_tx, _) = broadcast::channel(256);
    let state = SharedState::new(Mutex::new(AppState::new(manager, sse_tx.clone())));

    // Background SSE message pump.
    // Presage Manager::receive_messages() returns !Send future (RNG, dyn Store trait objects),
    // so run on a dedicated OS thread with its own tokio runtime.
    let pump_state = state.clone();
    std::thread::Builder::new()
        .name("signal-listener".into())
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                if let Err(e) = sse::run_message_listener(pump_state).await {
                    tracing::error!("SSE listener crashed: {:?}", e);
                }
            });
        })
        .expect("failed to spawn SSE listener thread");


    let app = Router::new()
        .route("/v1/health", get(health_check))
        .route("/api/v1/check", get(health_check))
        .route("/api/v1/events", get(sse::events_handler))
        .route("/v1/receive/{account}", get(ws::ws_receive_handler))
        .route("/api/v1/rpc", post(rpc_handler))
        .with_state(state);

    info!("Listening on {}", listen);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

async fn health_check() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// Presage Manager futures capture !Send types (RNG, dyn Store trait objects).
/// Handler's Future must be Send (bound on Handler trait). Spawn on dedicated
/// OS thread to isolate !Send types, then oneshot back result.
async fn rpc_handler(
    State(state): State<SharedState>,
    Json(req): Json<Value>,
) -> Json<Value> {
    let state = state.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("signal-rpc".into())
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(async {
                let mut guard = state.lock().await;
                rpc::dispatch(&mut guard, &req).await
            });
            let _ = tx.send(result);
        })
        .expect("failed to spawn RPC thread");
    let result = rx.await.unwrap_or_else(|_| {
        serde_json::json!({"jsonrpc": "2.0", "error": {"code": -1, "message": "dispatch failed"}, "id": null})
    });
    Json(result)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_url_builds_sqlite_path() {
        assert_eq!(store_url("/tmp/foo"), "sqlite:///tmp/foo/signal-serve.db");
        assert_eq!(store_url("/home/user/signal"), "sqlite:///home/user/signal/signal-serve.db");
    }
}

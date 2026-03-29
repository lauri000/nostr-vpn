use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, broadcast, mpsc};

#[derive(Debug, Parser)]
#[command(name = "nostr-vpn-relay")]
#[command(about = "Minimal local Nostr relay for nostr-vpn integration testing")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:8080")]
    bind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NostrEvent {
    id: String,
    pubkey: String,
    created_at: u64,
    kind: u32,
    tags: Vec<Vec<String>>,
    content: String,
    sig: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct NostrFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kinds: Option<Vec<u32>>,
    #[serde(rename = "#p", skip_serializing_if = "Option::is_none")]
    p_tags: Option<Vec<String>>,
    #[serde(rename = "#t", skip_serializing_if = "Option::is_none")]
    t_tags: Option<Vec<String>>,
    #[serde(rename = "#d", skip_serializing_if = "Option::is_none")]
    d_tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<u64>,
}

impl NostrFilter {
    fn matches(&self, event: &NostrEvent) -> bool {
        if let Some(ids) = &self.ids
            && !ids.contains(&event.id)
        {
            return false;
        }
        if let Some(authors) = &self.authors
            && !authors.contains(&event.pubkey)
        {
            return false;
        }
        if let Some(kinds) = &self.kinds
            && !kinds.contains(&event.kind)
        {
            return false;
        }
        if let Some(p_tags) = &self.p_tags {
            let has_match = event
                .tags
                .iter()
                .any(|tag| tag.len() >= 2 && tag[0] == "p" && p_tags.contains(&tag[1]));
            if !has_match {
                return false;
            }
        }
        if let Some(t_tags) = &self.t_tags {
            let has_match = event
                .tags
                .iter()
                .any(|tag| tag.len() >= 2 && tag[0] == "t" && t_tags.contains(&tag[1]));
            if !has_match {
                return false;
            }
        }
        if let Some(d_tags) = &self.d_tags {
            let has_match = event
                .tags
                .iter()
                .any(|tag| tag.len() >= 2 && tag[0] == "d" && d_tags.contains(&tag[1]));
            if !has_match {
                return false;
            }
        }
        if let Some(since) = self.since
            && event.created_at < since
        {
            return false;
        }

        true
    }
}

struct Subscription {
    filters: Vec<NostrFilter>,
}

struct RelayState {
    events: RwLock<Vec<NostrEvent>>,
    event_broadcast: broadcast::Sender<NostrEvent>,
}

impl RelayState {
    fn new() -> Self {
        let (event_broadcast, _) = broadcast::channel(2048);
        Self {
            events: RwLock::new(Vec::new()),
            event_broadcast,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let bind: SocketAddr = args
        .bind
        .parse()
        .with_context(|| format!("invalid bind address {}", args.bind))?;

    let state = Arc::new(RelayState::new());
    let app = Router::new().route("/", get(ws_handler)).with_state(state);
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind {bind}"))?;

    println!("nostr-vpn-relay listening on ws://{bind}");
    axum::serve(listener, app)
        .await
        .context("relay server exited unexpectedly")?;

    Ok(())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<RelayState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<RelayState>) {
    let (mut sender, mut receiver) = socket.split();

    let subscriptions: Arc<RwLock<HashMap<String, Subscription>>> =
        Arc::new(RwLock::new(HashMap::new()));

    let (tx, mut rx) = mpsc::channel::<String>(1024);
    let mut event_rx = state.event_broadcast.subscribe();

    let subscriptions_for_broadcast = subscriptions.clone();
    let tx_for_broadcast = tx.clone();

    let broadcast_task = tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            let subscriptions = subscriptions_for_broadcast.read().await;
            for (sub_id, sub) in subscriptions.iter() {
                if sub.filters.iter().any(|filter| filter.matches(&event)) {
                    let message = serde_json::json!(["EVENT", sub_id, event]);
                    let _ = tx_for_broadcast.send(message.to_string()).await;
                }
            }
        }
    });

    let send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if sender.send(Message::Text(message.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(message)) = receiver.next().await {
        let Message::Text(text) = message else {
            continue;
        };

        let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
            continue;
        };

        if parsed.is_empty() {
            continue;
        }

        match parsed[0].as_str().unwrap_or_default() {
            "EVENT" => {
                if parsed.len() < 2 {
                    continue;
                }

                let Ok(event) = serde_json::from_value::<NostrEvent>(parsed[1].clone()) else {
                    continue;
                };

                let event_id = event.id.clone();
                state.events.write().await.push(event.clone());
                let _ = state.event_broadcast.send(event);

                let ok_message = serde_json::json!(["OK", event_id, true, ""]);
                let _ = tx.send(ok_message.to_string()).await;
            }
            "REQ" => {
                if parsed.len() < 3 {
                    continue;
                }

                let sub_id = parsed[1].as_str().unwrap_or_default().to_string();
                let mut filters = Vec::new();

                for raw_filter in parsed.iter().skip(2) {
                    if let Ok(filter) = serde_json::from_value::<NostrFilter>(raw_filter.clone()) {
                        filters.push(filter);
                    }
                }

                let events = state.events.read().await;
                for event in events.iter() {
                    if filters.iter().any(|filter| filter.matches(event)) {
                        let event_message = serde_json::json!(["EVENT", &sub_id, event]);
                        let _ = tx.send(event_message.to_string()).await;
                    }
                }

                let eose_message = serde_json::json!(["EOSE", &sub_id]);
                let _ = tx.send(eose_message.to_string()).await;

                subscriptions
                    .write()
                    .await
                    .insert(sub_id, Subscription { filters });
            }
            "CLOSE" => {
                if parsed.len() >= 2
                    && let Some(sub_id) = parsed[1].as_str()
                {
                    subscriptions.write().await.remove(sub_id);
                }
            }
            _ => {}
        }
    }

    broadcast_task.abort();
    send_task.abort();
}

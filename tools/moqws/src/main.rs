//! moqws - WebSocket-to-MOQT bridge server
//!
//! Each WebSocket connection creates an independent MOQT client.
//!
//! # Usage
//!
//! ```bash
//! moqws --bind 0.0.0.0:8080
//! ```

use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::tungstenite::Message;

#[derive(Parser)]
#[command(name = "moqws")]
#[command(about = "WebSocket-to-MOQT bridge server")]
struct Args {
    /// Address to bind the WebSocket server
    #[arg(short, long, default_value = "127.0.0.1:8765")]
    bind: String,

    /// libquicr log level
    #[arg(short, long, default_value = "warn")]
    log_level: String,
}

// ============================================================================
// Protocol Messages (Client → Server)
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Connect {
        relay_url: String,
        #[serde(default)]
        endpoint_id: Option<String>,
    },
    Disconnect,
    Subscribe {
        id: u32,
        namespace: Vec<String>,
        track: String,
    },
    Unsubscribe {
        id: u32,
    },
    PublishAnnounce {
        id: u32,
        namespace: Vec<String>,
        track: String,
        #[serde(default)]
        track_mode: Option<String>,
        #[serde(default)]
        priority: Option<u8>,
        #[serde(default)]
        ttl: Option<u32>,
    },
    Publish {
        id: u32,
        group_id: u64,
        object_id: u64,
        #[allow(dead_code)] // Reserved for per-object priority override
        #[serde(default)]
        priority: Option<u8>,
        #[allow(dead_code)] // Reserved for per-object TTL override
        #[serde(default)]
        ttl: Option<u32>,
    },
    PublishEnd {
        id: u32,
    },
}

// ============================================================================
// Protocol Messages (Server → Client)
// ============================================================================

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Connected {
        moqt_version: u64,
        server_id: String,
    },
    Disconnected {
        reason: String,
    },
    Error {
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u32>,
    },
    Subscribed {
        id: u32,
    },
    SubscribeError {
        id: u32,
        code: String,
        message: String,
    },
    SubscriptionEnded {
        id: u32,
        reason: String,
    },
    Published {
        id: u32,
    },
    PublishError {
        id: u32,
        code: String,
        message: String,
    },
    Object {
        id: u32,
        group_id: u64,
        object_id: u64,
        payload_length: usize,
    },
}

// ============================================================================
// MOQT Bridge Thread Messages
// ============================================================================

/// Commands sent from WebSocket handler to MOQT thread
#[derive(Debug)]
enum MoqtCommand {
    Connect {
        relay_url: String,
        endpoint_id: String,
    },
    Disconnect,
    Subscribe {
        id: u32,
        namespace: Vec<String>,
        track: String,
    },
    Unsubscribe {
        id: u32,
    },
    PublishAnnounce {
        id: u32,
        namespace: Vec<String>,
        track: String,
        track_mode: String,
        priority: u8,
        ttl: u32,
    },
    Publish {
        id: u32,
        group_id: u64,
        object_id: u64,
        payload: Vec<u8>,
    },
    PublishEnd {
        id: u32,
    },
    Shutdown,
}

/// Events sent from MOQT thread to WebSocket handler
#[derive(Debug)]
enum MoqtEvent {
    Connected {
        moqt_version: u64,
        server_id: String,
    },
    Disconnected {
        reason: String,
    },
    Error {
        code: String,
        message: String,
        id: Option<u32>,
    },
    Subscribed {
        id: u32,
    },
    SubscribeError {
        id: u32,
        code: String,
        message: String,
    },
    SubscriptionEnded {
        id: u32,
        reason: String,
    },
    Published {
        id: u32,
    },
    PublishError {
        id: u32,
        code: String,
        message: String,
    },
    Object {
        id: u32,
        group_id: u64,
        object_id: u64,
        payload: Vec<u8>,
    },
}

// ============================================================================
// MOQT Bridge Thread
// ============================================================================

fn spawn_moqt_thread(
    cmd_rx: Receiver<MoqtCommand>,
    event_tx: tokio_mpsc::UnboundedSender<MoqtEvent>,
    log_level: String,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Run the embassy executor for this MOQT client
        let executor = embassy_executor::Executor::new();
        let executor = Box::leak(Box::new(executor));

        executor.run(|spawner| {
            spawner
                .spawn(moqt_client_task(cmd_rx, event_tx, log_level))
                .unwrap();
        });
    })
}

#[embassy_executor::task]
async fn moqt_client_task(cmd_rx: Receiver<MoqtCommand>, event_tx: tokio_mpsc::UnboundedSender<MoqtEvent>, log_level: String) {
    use embassy_time::{Duration, Timer};
    use quicr::*;

    let log_level = match log_level.to_lowercase().as_str() {
        "trace" | "t" => LogLevel::Trace,
        "debug" | "d" => LogLevel::Debug,
        "info" | "i" => LogLevel::Info,
        "warn" | "warning" | "w" => LogLevel::Warn,
        "error" | "err" | "e" => LogLevel::Error,
        _ => LogLevel::Warn,
    };

    let mut client: Option<Client> = None;
    let mut subscriptions: HashMap<u32, Subscription> = HashMap::new();
    let mut publish_tracks: HashMap<u32, Arc<PublishTrack>> = HashMap::new();

    loop {
        // Check for commands (non-blocking)
        match cmd_rx.try_recv() {
            Ok(cmd) => {
                match cmd {
                    MoqtCommand::Connect { relay_url, endpoint_id } => {
                        if client.is_some() {
                            let _ = event_tx.send(MoqtEvent::Error {
                                code: "already_connected".into(),
                                message: "Already connected to a relay".into(),
                                id: None,
                            });
                            continue;
                        }

                        info!("Connecting to MOQT relay: {}", relay_url);
                        match ClientBuilder::new()
                            .endpoint_id(&endpoint_id)
                            .connect_uri(&relay_url)
                            .log_level(log_level)
                            .build()
                        {
                            Ok(c) => {
                                match c.connect().await {
                                    Ok(()) => {
                                        info!("Connected to MOQT relay");
                                        let _ = event_tx.send(MoqtEvent::Connected {
                                            moqt_version: 1, // TODO: get from server setup
                                            server_id: "moqt-relay".into(),
                                        });
                                        client = Some(c);
                                    }
                                    Err(e) => {
                                        error!("Failed to connect: {}", e);
                                        let _ = event_tx.send(MoqtEvent::Error {
                                            code: "connection_failed".into(),
                                            message: format!("Failed to connect: {}", e),
                                            id: None,
                                        });
                                    }
                                }
                            }
                            Err(e) => {
                                error!("Failed to create client: {}", e);
                                let _ = event_tx.send(MoqtEvent::Error {
                                    code: "connection_failed".into(),
                                    message: format!("Failed to create client: {}", e),
                                    id: None,
                                });
                            }
                        }
                    }

                    MoqtCommand::Disconnect => {
                        if let Some(c) = client.take() {
                            subscriptions.clear();
                            publish_tracks.clear();
                            let _ = c.disconnect().await;
                            let _ = event_tx.send(MoqtEvent::Disconnected {
                                reason: "client_requested".into(),
                            });
                        }
                    }

                    MoqtCommand::Subscribe { id, namespace, track } => {
                        let Some(ref c) = client else {
                            let _ = event_tx.send(MoqtEvent::SubscribeError {
                                id,
                                code: "not_connected".into(),
                                message: "Must connect before subscribing".into(),
                            });
                            continue;
                        };

                        if subscriptions.contains_key(&id) {
                            let _ = event_tx.send(MoqtEvent::SubscribeError {
                                id,
                                code: "duplicate_id".into(),
                                message: "Subscription ID already in use".into(),
                            });
                            continue;
                        }

                        let ns_refs: Vec<&str> = namespace.iter().map(|s| s.as_str()).collect();
                        let track_name = FullTrackName::from_strings(&ns_refs, &track);

                        match c.subscribe(track_name).await {
                            Ok(sub) => {
                                info!("Subscribed to track (id={})", id);
                                subscriptions.insert(id, sub);
                                let _ = event_tx.send(MoqtEvent::Subscribed { id });
                            }
                            Err(e) => {
                                error!("Failed to subscribe: {}", e);
                                let _ = event_tx.send(MoqtEvent::SubscribeError {
                                    id,
                                    code: "subscribe_failed".into(),
                                    message: format!("Failed to subscribe: {}", e),
                                });
                            }
                        }
                    }

                    MoqtCommand::Unsubscribe { id } => {
                        if subscriptions.remove(&id).is_some() {
                            info!("Unsubscribed (id={})", id);
                        } else {
                            let _ = event_tx.send(MoqtEvent::Error {
                                code: "unknown_id".into(),
                                message: format!("Unknown subscription ID: {}", id),
                                id: Some(id),
                            });
                        }
                    }

                    MoqtCommand::PublishAnnounce {
                        id,
                        namespace,
                        track,
                        track_mode,
                        priority,
                        ttl,
                    } => {
                        let Some(ref c) = client else {
                            let _ = event_tx.send(MoqtEvent::PublishError {
                                id,
                                code: "not_connected".into(),
                                message: "Must connect before publishing".into(),
                            });
                            continue;
                        };

                        if publish_tracks.contains_key(&id) {
                            let _ = event_tx.send(MoqtEvent::PublishError {
                                id,
                                code: "duplicate_id".into(),
                                message: "Publish track ID already in use".into(),
                            });
                            continue;
                        }

                        let ns_refs: Vec<&str> = namespace.iter().map(|s| s.as_str()).collect();
                        let track_name = FullTrackName::from_strings(&ns_refs, &track);

                        let mode = if track_mode == "stream" {
                            TrackMode::Stream
                        } else {
                            TrackMode::Datagram
                        };

                        let publish_track = match PublishTrackBuilder::new(track_name)
                            .track_mode(mode)
                            .default_priority(priority)
                            .default_ttl(ttl)
                            .build()
                        {
                            Ok(t) => t,
                            Err(e) => {
                                let _ = event_tx.send(MoqtEvent::PublishError {
                                    id,
                                    code: "internal_error".into(),
                                    message: format!("Failed to create publish track: {}", e),
                                });
                                continue;
                            }
                        };

                        match c.publish_track(publish_track).await {
                            Ok(track) => {
                                info!("Publish track announced (id={})", id);
                                publish_tracks.insert(id, track);
                                let _ = event_tx.send(MoqtEvent::Published { id });
                            }
                            Err(e) => {
                                error!("Failed to announce publish track: {}", e);
                                let _ = event_tx.send(MoqtEvent::PublishError {
                                    id,
                                    code: "announce_failed".into(),
                                    message: format!("Failed to announce: {}", e),
                                });
                            }
                        }
                    }

                    MoqtCommand::Publish {
                        id,
                        group_id,
                        object_id,
                        payload,
                    } => {
                        let Some(track) = publish_tracks.get(&id) else {
                            let _ = event_tx.send(MoqtEvent::Error {
                                code: "unknown_id".into(),
                                message: format!("Unknown publish track ID: {}", id),
                                id: Some(id),
                            });
                            continue;
                        };

                        let headers = ObjectHeaders::new(group_id, object_id);
                        if let Err(e) = track.publish(&headers, &payload) {
                            warn!("Publish failed: {}", e);
                            // Don't send error for every publish failure (could be NoSubscribers)
                        }
                    }

                    MoqtCommand::PublishEnd { id } => {
                        if let Some(ref c) = client {
                            if let Some(track) = publish_tracks.remove(&id) {
                                let _ = c.unpublish_track(&track).await;
                                info!("Publish track ended (id={})", id);
                            }
                        }
                    }

                    MoqtCommand::Shutdown => {
                        info!("MOQT thread shutting down");
                        if let Some(c) = client.take() {
                            let _ = c.disconnect().await;
                        }
                        return;
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                // No command available, continue to poll subscriptions
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                info!("Command channel disconnected, shutting down");
                if let Some(c) = client.take() {
                    let _ = c.disconnect().await;
                }
                return;
            }
        }

        // Poll all subscriptions for received objects
        let mut ended_subs = Vec::new();
        for (&id, sub) in &mut subscriptions {
            // Check if subscription ended
            if sub.is_done() {
                let reason = match sub.status() {
                    SubscribeStatus::DoneByFin => "done_by_fin",
                    SubscribeStatus::DoneByReset => "done_by_reset",
                    SubscribeStatus::Cancelled => "cancelled",
                    _ => "error",
                };
                ended_subs.push((id, reason.to_string()));
                continue;
            }

            // Try to receive objects
            while let Ok(obj) = sub.try_recv() {
                let _ = event_tx.send(MoqtEvent::Object {
                    id,
                    group_id: obj.headers.group_id,
                    object_id: obj.headers.object_id,
                    payload: obj.payload().to_vec(),
                });
            }
        }

        // Remove ended subscriptions and notify
        for (id, reason) in ended_subs {
            subscriptions.remove(&id);
            let _ = event_tx.send(MoqtEvent::SubscriptionEnded { id, reason });
        }

        // Small sleep to avoid busy-waiting
        Timer::after(Duration::from_millis(1)).await;
    }
}

// ============================================================================
// WebSocket Connection Handler
// ============================================================================

async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    log_level: String,
) {
    info!("New WebSocket connection from: {}", addr);

    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            error!("WebSocket handshake failed: {}", e);
            return;
        }
    };

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Create channels for MOQT thread communication
    let (cmd_tx, cmd_rx) = mpsc::channel::<MoqtCommand>();
    let (event_tx, mut event_rx) = tokio_mpsc::unbounded_channel::<MoqtEvent>();

    // Spawn MOQT thread
    let moqt_handle = spawn_moqt_thread(cmd_rx, event_tx, log_level);

    // State for handling binary frames after publish commands
    let pending_publish: Arc<Mutex<Option<(u32, u64, u64)>>> = Arc::new(Mutex::new(None));

    // Main event loop
    loop {
        tokio::select! {
            // Handle WebSocket messages
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        debug!("Received text: {}", text);
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(client_msg) => {
                                if let Err(e) = handle_client_message(
                                    client_msg,
                                    &cmd_tx,
                                    &mut ws_tx,
                                    &pending_publish,
                                ).await {
                                    error!("Error handling message: {}", e);
                                }
                            }
                            Err(e) => {
                                let err = ServerMessage::Error {
                                    code: "invalid_message".into(),
                                    message: format!("Failed to parse message: {}", e),
                                    id: None,
                                };
                                let _ = ws_tx.send(Message::Text(serde_json::to_string(&err).unwrap().into())).await;
                            }
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        // Handle binary frame (payload for pending publish)
                        let pending = pending_publish.lock().unwrap().take();
                        if let Some((id, group_id, object_id)) = pending {
                            let _ = cmd_tx.send(MoqtCommand::Publish {
                                id,
                                group_id,
                                object_id,
                                payload: data.to_vec(),
                            });
                        } else {
                            warn!("Received unexpected binary frame");
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket closed by client");
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws_tx.send(Message::Pong(data)).await;
                    }
                    Some(Ok(_)) => {
                        // Ignore other message types
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        info!("WebSocket stream ended");
                        break;
                    }
                }
            }

            // Handle events from MOQT thread
            event = event_rx.recv() => {
                match event {
                    Some(event) => {
                        if let Err(_) = send_moqt_event(&mut ws_tx, event).await {
                            break;
                        }
                    }
                    None => {
                        // MOQT thread disconnected
                        break;
                    }
                }
            }
        }
    }

    // Cleanup
    let _ = cmd_tx.send(MoqtCommand::Shutdown);
    let _ = moqt_handle.join();
    info!("Connection closed: {}", addr);
}

async fn send_moqt_event(
    ws_tx: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<TcpStream>,
        Message,
    >,
    event: MoqtEvent,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match event {
        MoqtEvent::Object { id, group_id, object_id, payload } => {
            // Object events: send JSON header, then binary payload
            let msg = ServerMessage::Object {
                id,
                group_id,
                object_id,
                payload_length: payload.len(),
            };
            let json = serde_json::to_string(&msg)?;
            ws_tx.send(Message::Text(json.into())).await?;
            ws_tx.send(Message::Binary(payload.into())).await?;
        }
        MoqtEvent::Connected { moqt_version, server_id } => {
            let msg = ServerMessage::Connected { moqt_version, server_id };
            let json = serde_json::to_string(&msg)?;
            ws_tx.send(Message::Text(json.into())).await?;
        }
        MoqtEvent::Disconnected { reason } => {
            let msg = ServerMessage::Disconnected { reason };
            let json = serde_json::to_string(&msg)?;
            ws_tx.send(Message::Text(json.into())).await?;
        }
        MoqtEvent::Error { code, message, id } => {
            let msg = ServerMessage::Error { code, message, id };
            let json = serde_json::to_string(&msg)?;
            ws_tx.send(Message::Text(json.into())).await?;
        }
        MoqtEvent::Subscribed { id } => {
            let msg = ServerMessage::Subscribed { id };
            let json = serde_json::to_string(&msg)?;
            ws_tx.send(Message::Text(json.into())).await?;
        }
        MoqtEvent::SubscribeError { id, code, message } => {
            let msg = ServerMessage::SubscribeError { id, code, message };
            let json = serde_json::to_string(&msg)?;
            ws_tx.send(Message::Text(json.into())).await?;
        }
        MoqtEvent::SubscriptionEnded { id, reason } => {
            let msg = ServerMessage::SubscriptionEnded { id, reason };
            let json = serde_json::to_string(&msg)?;
            ws_tx.send(Message::Text(json.into())).await?;
        }
        MoqtEvent::Published { id } => {
            let msg = ServerMessage::Published { id };
            let json = serde_json::to_string(&msg)?;
            ws_tx.send(Message::Text(json.into())).await?;
        }
        MoqtEvent::PublishError { id, code, message } => {
            let msg = ServerMessage::PublishError { id, code, message };
            let json = serde_json::to_string(&msg)?;
            ws_tx.send(Message::Text(json.into())).await?;
        }
    }
    Ok(())
}

async fn handle_client_message(
    msg: ClientMessage,
    cmd_tx: &Sender<MoqtCommand>,
    ws_tx: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<TcpStream>,
        Message,
    >,
    pending_publish: &Arc<Mutex<Option<(u32, u64, u64)>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match msg {
        ClientMessage::Connect { relay_url, endpoint_id } => {
            let endpoint_id = endpoint_id.unwrap_or_else(|| {
                format!("moqws-{}", std::process::id())
            });
            cmd_tx.send(MoqtCommand::Connect { relay_url, endpoint_id })?;
        }
        ClientMessage::Disconnect => {
            cmd_tx.send(MoqtCommand::Disconnect)?;
        }
        ClientMessage::Subscribe { id, namespace, track } => {
            cmd_tx.send(MoqtCommand::Subscribe { id, namespace, track })?;
        }
        ClientMessage::Unsubscribe { id } => {
            cmd_tx.send(MoqtCommand::Unsubscribe { id })?;
        }
        ClientMessage::PublishAnnounce {
            id,
            namespace,
            track,
            track_mode,
            priority,
            ttl,
        } => {
            cmd_tx.send(MoqtCommand::PublishAnnounce {
                id,
                namespace,
                track,
                track_mode: track_mode.unwrap_or_else(|| "datagram".into()),
                priority: priority.unwrap_or(0),
                ttl: ttl.unwrap_or(1000),
            })?;
        }
        ClientMessage::Publish {
            id,
            group_id,
            object_id,
            priority: _,
            ttl: _,
        } => {
            // Store pending publish info, wait for binary frame
            *pending_publish.lock().unwrap() = Some((id, group_id, object_id));
        }
        ClientMessage::PublishEnd { id } => {
            cmd_tx.send(MoqtCommand::PublishEnd { id })?;
        }
    }
    let _ = ws_tx; // silence unused warning for now
    Ok(())
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    env_logger::init();

    let args = Args::parse();

    let addr: SocketAddr = args.bind.parse().expect("Invalid bind address");

    let listener = TcpListener::bind(&addr).await.expect("Failed to bind");
    info!("moqws server listening on: {}", addr);
    info!("Connect via: ws://{}", addr);

    while let Ok((stream, peer_addr)) = listener.accept().await {
        let log_level = args.log_level.clone();
        tokio::spawn(async move {
            handle_connection(stream, peer_addr, log_level).await;
        });
    }
}

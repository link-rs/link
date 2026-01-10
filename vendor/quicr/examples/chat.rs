//! Simple Pub/Sub Chat Example
//!
//! This example demonstrates how to use quicr.rs for a simple chat application
//! where users can publish and subscribe to chat messages.
//!
//! # Usage
//!
//! Publisher mode (send messages):
//! ```bash
//! cargo run --example chat -- --mode publish --relay moqt://localhost:4433 --room myroom --user alice
//! ```
//!
//! Subscriber mode (receive messages):
//! ```bash
//! cargo run --example chat -- --mode subscribe --relay moqt://localhost:4433 --room myroom
//! ```
//!
//! Both modes (send and receive):
//! ```bash
//! cargo run --example chat -- --mode both --relay moqt://localhost:4433 --room myroom --user alice
//! ```

use clap::{Parser, ValueEnum};
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Timer};
use quicr::*;
use std::io::{self, BufRead, Write};
use std::sync::mpsc;

/// Chat message format
#[derive(Debug, Clone)]
struct ChatMessage {
    user: String,
    content: String,
    timestamp: u64,
}

impl ChatMessage {
    fn new(user: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            content: content.into(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        }
    }

    /// Serialize to bytes (simple format: user\0content\0timestamp)
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.user.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.content.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes
    }

    /// Deserialize from bytes
    fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut parts = data.splitn(3, |&b| b == 0);
        let user = std::str::from_utf8(parts.next()?).ok()?.to_string();
        let content = std::str::from_utf8(parts.next()?).ok()?.to_string();
        let timestamp_bytes = parts.next()?;
        if timestamp_bytes.len() < 8 {
            return None;
        }
        let timestamp = u64::from_le_bytes(timestamp_bytes[..8].try_into().ok()?);
        Some(Self {
            user,
            content,
            timestamp,
        })
    }
}

/// Application configuration
#[derive(Parser)]
#[command(name = "chat")]
#[command(about = "quicr.rs Chat Example", long_about = None)]
#[command(after_help = "Examples:
  # Bob publishes as 'bob' and subscribes to 'alice':
  chat --mode both --user bob --subscribe-to alice --room myroom

  # Alice publishes as 'alice' and subscribes to 'bob':
  chat --mode both --user alice --subscribe-to bob --room myroom")]
struct Config {
    /// Relay URI
    #[arg(short, long, default_value = "moqt://localhost:4433")]
    relay: String,

    /// Chat room name
    #[arg(long, default_value = "default")]
    room: String,

    /// Username to publish as
    #[arg(short, long, default_value = "anonymous")]
    user: String,

    /// Username to subscribe to (required for 'both' mode)
    #[arg(short, long)]
    subscribe_to: Option<String>,

    /// Mode: publish, subscribe, or both
    #[arg(short, long, value_enum, default_value_t = Mode::Both)]
    mode: Mode,

    /// libquicr log level
    #[arg(short, long, value_enum, default_value_t = LogLevelArg::default_for_build())]
    log_level: LogLevelArg,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Mode {
    #[value(alias = "pub", alias = "p")]
    Publish,
    #[value(alias = "sub")]
    Subscribe,
    #[value(alias = "b")]
    Both,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum LogLevelArg {
    #[value(alias = "t")]
    Trace,
    #[value(alias = "d")]
    Debug,
    #[value(alias = "i")]
    Info,
    #[value(alias = "w", alias = "warning")]
    Warn,
    #[value(alias = "e", alias = "err")]
    Error,
    #[value(alias = "c", alias = "crit")]
    Critical,
    #[value(alias = "o", alias = "none")]
    Off,
}

impl LogLevelArg {
    fn default_for_build() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Off
        }
    }
}

impl From<LogLevelArg> for LogLevel {
    fn from(arg: LogLevelArg) -> Self {
        match arg {
            LogLevelArg::Trace => LogLevel::Trace,
            LogLevelArg::Debug => LogLevel::Debug,
            LogLevelArg::Info => LogLevel::Info,
            LogLevelArg::Warn => LogLevel::Warn,
            LogLevelArg::Error => LogLevel::Error,
            LogLevelArg::Critical => LogLevel::Critical,
            LogLevelArg::Off => LogLevel::Off,
        }
    }
}

/// Run the publisher task
async fn run_publisher(
    client: &Client,
    room: String,
    user: String,
    rx: mpsc::Receiver<String>,
) -> Result<()> {
    // Create track name for publishing
    let track_name = FullTrackName::from_strings(&["chat", &room], &user);

    println!("[Publisher] Publishing to track: {}", track_name);
    println!("[Publisher] Track namespace: {:?}", track_name.namespace);
    println!("[Publisher] Track name: {:?}", String::from_utf8_lossy(&track_name.name));

    // Create and register publish track
    println!("[Publisher] Creating publish track handler...");
    let track = client.publish(track_name).await?;
    println!("[Publisher] Track handler created, initial status: {:?}", track.status());

    // Wait for the track to be ready
    println!("[Publisher] Waiting for subscribers (current status: {:?})...", track.status());

    let mut group_id = 0u64;
    let mut object_id = 0u64;

    // Process messages from stdin (poll the channel with timeout)
    loop {
        // Non-blocking receive with timeout
        match rx.try_recv() {
            Ok(content) => {
                if content.is_empty() {
                    continue;
                }

                // Log current track status before publishing
                let current_status = track.status();
                println!("[Publisher] Current track status: {:?}, can_publish: {}",
                         current_status, track.can_publish());

                let msg = ChatMessage::new(&user, &content);
                let data = msg.to_bytes();

                let headers = ObjectHeaders::builder()
                    .group_id(group_id)
                    .object_id(object_id)
                    .build(data.len() as u64);

                println!("[Publisher] Publishing object group={} object={} len={}",
                         group_id, object_id, data.len());

                match track.publish(&headers, &data) {
                    Ok(status) => {
                        println!("[Publisher] Publish result: {:?}", status);
                        if status.is_ok() {
                            println!("[Publisher] Sent: {}", content);
                            object_id += 1;
                        } else if status.can_continue() {
                            // No subscribers yet, but we can continue
                            println!("[Publisher] Message queued (status: {:?})", status);
                        } else {
                            println!("[Publisher] Failed to send: {:?}", status);
                        }
                    }
                    Err(e) => {
                        println!("[Publisher] Error: {}", e);
                    }
                }

                // Increment group on every 100 messages
                if object_id >= 100 {
                    group_id += 1;
                    object_id = 0;
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                // No message available, wait a bit
                Timer::after(Duration::from_millis(50)).await;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Stdin reader thread exited
                break;
            }
        }
    }

    Ok(())
}

/// Run the subscriber task
async fn run_subscriber(
    client: &Client,
    room: String,
    user_filter: Option<String>,
) -> Result<()> {
    // For simplicity, subscribe to a specific user's track
    // In a real app, you'd subscribe to the namespace and handle multiple tracks
    let track_name = match user_filter {
        Some(ref user) => FullTrackName::from_strings(&["chat", &room], user),
        None => {
            // Subscribe to a wildcard track (implementation-specific)
            FullTrackName::from_strings(&["chat", &room], "*")
        }
    };

    println!("[Subscriber] Subscribing to track: {}", track_name);
    println!("[Subscriber] Track namespace: {:?}", track_name.namespace);
    println!("[Subscriber] Track name: {:?}", String::from_utf8_lossy(&track_name.name));

    // Create subscription
    println!("[Subscriber] Creating subscribe track handler...");
    let mut subscription = client.subscribe(track_name).await?;
    println!("[Subscriber] Subscribe track created, status: {:?}", subscription.status());

    println!("[Subscriber] Listening for messages (status: {:?})...", subscription.status());
    println!();

    // Receive and display messages
    loop {
        let object = subscription.recv().await;
        if let Some(msg) = ChatMessage::from_bytes(&object.data) {
            let time = chrono_lite_format(msg.timestamp);
            println!("[{}] {}: {}", time, msg.user, msg.content);
        } else {
            println!("[Subscriber] Received invalid message");
        }

        if subscription.is_done() {
            break;
        }
    }

    println!("[Subscriber] Subscription ended");
    Ok(())
}

/// Simple timestamp formatting (without chrono dependency)
fn chrono_lite_format(timestamp_ms: u64) -> String {
    let secs = timestamp_ms / 1000;
    let hours = (secs / 3600) % 24;
    let mins = (secs / 60) % 60;
    let secs = secs % 60;
    format!("{:02}:{:02}:{:02}", hours, mins, secs)
}

/// Read lines from stdin in a separate thread
fn spawn_stdin_reader() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let stdin = io::stdin();
        let reader = stdin.lock();

        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    rx
}

#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    // Initialize logging
    env_logger::init();

    let config = Config::parse();

    // Validate config for 'both' mode
    if config.mode == Mode::Both && config.subscribe_to.is_none() {
        eprintln!("Error: --subscribe-to is required for 'both' mode");
        eprintln!("Example: --mode both --user bob --subscribe-to alice");
        std::process::exit(1);
    }

    println!("=================================");
    println!("  quicr.rs Chat Example");
    println!("=================================");
    println!();
    println!("Relay:        {}", config.relay);
    println!("Room:         {}", config.room);
    println!("User:         {}", config.user);
    if let Some(ref sub_to) = config.subscribe_to {
        println!("Subscribe to: {}", sub_to);
    }
    println!("Mode:         {:?}", match config.mode {
        Mode::Publish => "publish",
        Mode::Subscribe => "subscribe",
        Mode::Both => "both",
    });
    println!();

    // Create client using builder
    let client = match ClientBuilder::new()
        .endpoint_id(format!("chat-{}", config.user))
        .connect_uri(&config.relay)
        .log_level(config.log_level.into())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to create client: {}", e);
            return;
        }
    };

    // Connect
    println!("Connecting to relay...");
    if let Err(e) = client.connect().await {
        eprintln!("Failed to connect: {}", e);
        return;
    }
    println!("Connected!");
    println!();

    // Announce the chat namespace
    let namespace = TrackNamespace::from_strings(&["chat", &config.room]);
    println!("[Main] Announcing namespace: {:?}", namespace);
    client.publish_namespace(&namespace);
    println!("[Main] Namespace announce sent (waiting for ANNOUNCE_OK from relay)");

    // Give a small delay for the announce to be processed
    Timer::after(Duration::from_millis(100)).await;
    println!("[Main] Proceeding with mode...");

    match config.mode {
        Mode::Publish => {
            println!("Type messages and press Enter to send (Ctrl+C to quit):");
            println!();
            print!("> ");
            io::stdout().flush().unwrap();

            let stdin_rx = spawn_stdin_reader();
            if let Err(e) = run_publisher(&client, config.room, config.user, stdin_rx).await {
                eprintln!("Publisher error: {}", e);
            }
        }
        Mode::Subscribe => {
            // Note: In subscribe mode, we need to know which user's track to subscribe to.
            // The username passed via -u is the user whose messages we want to receive.
            if let Err(e) = run_subscriber(&client, config.room, Some(config.user)).await {
                eprintln!("Subscriber error: {}", e);
            }
        }
        Mode::Both => {
            println!("Type messages and press Enter to send (Ctrl+C to quit):");
            println!();
            print!("> ");
            io::stdout().flush().unwrap();

            let stdin_rx = spawn_stdin_reader();
            let room_pub = config.room.clone();
            let room_sub = config.room.clone();
            let user_pub = config.user.clone();
            let subscribe_to = config.subscribe_to.clone().unwrap(); // Already validated above

            println!("[Both] Publishing as '{}', subscribing to '{}'", user_pub, subscribe_to);

            // Run both publisher and subscriber
            match select(
                run_publisher(&client, room_pub, user_pub, stdin_rx),
                run_subscriber(&client, room_sub, Some(subscribe_to)),
            ).await {
                Either::First(result) => {
                    if let Err(e) = result {
                        eprintln!("Publisher error: {}", e);
                    }
                }
                Either::Second(result) => {
                    if let Err(e) = result {
                        eprintln!("Subscriber error: {}", e);
                    }
                }
            }
        }
    }

    // Disconnect
    if let Err(e) = client.disconnect().await {
        eprintln!("Error disconnecting: {}", e);
    }

    println!("\nGoodbye!");
}

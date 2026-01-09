//! Pub/Sub Clock Example
//!
//! This example demonstrates basic publish/subscribe functionality similar to
//! the clock mode in the C++ client.cpp example.
//!
//! # Usage
//!
//! Publisher mode (publish clock timestamps every second):
//! ```bash
//! cargo run --example pubsub -- --mode publish --relay moqt://localhost:4433 \
//!     --namespace "clock/demo" --track-name "timestamps"
//! ```
//!
//! Subscriber mode (receive clock timestamps):
//! ```bash
//! cargo run --example pubsub -- --mode subscribe --relay moqt://localhost:4433 \
//!     --namespace "clock/demo" --track-name "timestamps"
//! ```

use clap::{Parser, ValueEnum};
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Timer};
use quicr::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Configuration parsed from command line
#[derive(Parser)]
#[command(name = "pubsub")]
#[command(about = "quicr.rs Pub/Sub Clock Example")]
#[command(long_about = "The publisher sends timestamps every second (or custom interval).\nThe subscriber receives and displays them.")]
#[command(after_help = "EXAMPLES:
    # Start a publisher
    cargo run --example pubsub -- --mode publish

    # Start a subscriber
    cargo run --example pubsub -- --mode subscribe

    # Publish every 500ms to a custom track with debug logging
    cargo run --example pubsub -- -m pub -n \"myapp/data\" -t sensor1 -i 500 -l debug")]
struct Config {
    /// Relay URI
    #[arg(short, long, default_value = "moqt://localhost:4433")]
    relay: String,

    /// Track namespace
    #[arg(short, long, default_value = "clock/demo")]
    namespace: String,

    /// Track name
    #[arg(short, long, default_value = "timestamps")]
    track_name: String,

    /// Mode: publish or subscribe
    #[arg(short, long, value_enum, default_value_t = Mode::Publish)]
    mode: Mode,

    /// Endpoint identifier
    #[arg(short, long, default_value = "pubsub-example")]
    endpoint_id: String,

    /// Publish interval in milliseconds
    #[arg(short, long, default_value_t = 1000)]
    interval: u64,

    /// libquicr log level
    #[arg(short, long, value_enum, default_value_t = LogLevelArg::default_for_build())]
    log_level: LogLevelArg,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Mode {
    #[value(alias = "pub", alias = "p")]
    Publish,
    #[value(alias = "sub", alias = "s")]
    Subscribe,
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

/// Get current time as a formatted string
fn get_time_str() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    let total_secs = now.as_secs();
    let hours = (total_secs / 3600) % 24;
    let mins = (total_secs / 60) % 60;
    let secs = total_secs % 60;
    let millis = now.subsec_millis();

    format!("{:02}:{:02}:{:02}.{:03}", hours, mins, secs, millis)
}

/// Run the publisher - publishes clock timestamps at regular intervals
async fn run_publisher(client: &Client, config: &Config, stop: Arc<AtomicBool>) -> Result<()> {
    // Parse namespace into segments (split by '/')
    let ns_parts: Vec<&str> = config.namespace.split('/').collect();
    let track_name = FullTrackName::from_strings(&ns_parts, &config.track_name);

    println!("[Publisher] Track: {}/{}", config.namespace, config.track_name);
    println!("[Publisher] Publishing every {}ms", config.interval);
    println!();

    // Create and register publish track
    let track = client.publish(track_name).await?;

    println!("[Publisher] Track registered, waiting for subscribers...");

    let mut group_id = 0u64;
    let mut object_id = 0u64;
    let interval = Duration::from_millis(config.interval);

    // Main publish loop
    while !stop.load(Ordering::Relaxed) {
        // Check track status
        let status = track.status();
        match status {
            PublishStatus::Ok | PublishStatus::SubscriptionUpdated => {
                // Ready to publish
            }
            PublishStatus::NoSubscribers => {
                // No subscribers yet, but we can still try to publish
                // The track will buffer or drop based on configuration
            }
            PublishStatus::NewGroupRequested => {
                // Start a new group as requested
                if object_id > 0 {
                    group_id += 1;
                    object_id = 0;
                }
                println!("[Publisher] New group requested, now using group {}", group_id);
            }
            _ => {
                // Wait a bit and retry
                Timer::after(Duration::from_millis(100)).await;
                continue;
            }
        }

        // Create timestamp message
        let timestamp = get_time_str();
        let message = format!("Clock: {}", timestamp);
        let data = message.as_bytes();

        // Create object headers
        // Note: TTL must be <= time_queue_max_duration (default 2000ms)
        let headers = ObjectHeaders::builder()
            .group_id(group_id)
            .object_id(object_id)
            .priority(2)
            .ttl(1000)
            .build(data.len() as u64);

        // Publish the object
        match track.publish(&headers, data) {
            Ok(pub_status) => {
                if pub_status.is_ok() {
                    println!(
                        "[Publisher] Group:{}, Object:{} -> {}",
                        group_id, object_id, message
                    );
                    object_id += 1;
                } else if pub_status == PublishObjectStatus::NoSubscribers {
                    // Still waiting for subscribers
                    print!(".");
                    std::io::Write::flush(&mut std::io::stdout()).ok();
                } else {
                    println!("[Publisher] Publish status: {:?}", pub_status);
                }
            }
            Err(e) => {
                println!("[Publisher] Error: {}", e);
            }
        }

        // Start new group every 15 objects (similar to C++ example)
        if object_id > 0 && object_id % 15 == 0 {
            group_id += 1;
            object_id = 0;
            println!("[Publisher] Starting new group {}", group_id);
        }

        // Wait for next interval
        Timer::after(interval).await;
    }

    println!();
    println!("[Publisher] Stopping...");
    client.unpublish_track(&track).await?;

    Ok(())
}

/// Run the subscriber - receives and displays clock timestamps
async fn run_subscriber(client: &Client, config: &Config, stop: Arc<AtomicBool>) -> Result<()> {
    // Parse namespace into segments
    let ns_parts: Vec<&str> = config.namespace.split('/').collect();
    let track_name = FullTrackName::from_strings(&ns_parts, &config.track_name);

    println!("[Subscriber] Track: {}/{}", config.namespace, config.track_name);
    println!("[Subscriber] Waiting for messages...");
    println!();

    // Subscribe to the track
    let mut subscription = client.subscribe(track_name).await?;

    // Receive loop
    loop {
        // Check stop flag with timeout
        let stop_check = async {
            Timer::after(Duration::from_millis(100)).await;
            stop.load(Ordering::Relaxed)
        };

        // Receive next object or check stop
        match select(subscription.recv(), stop_check).await {
            Either::First(obj) => {
                let message = obj.payload_str().unwrap_or("<binary data>");
                println!(
                    "[Subscriber] Group:{}, Object:{} <- {}",
                    obj.headers.group_id,
                    obj.headers.object_id,
                    message
                );
            }
            Either::Second(should_stop) => {
                if should_stop {
                    break;
                }
            }
        }

        // Check if subscription is done
        if subscription.is_done() {
            println!("[Subscriber] Subscription ended");
            break;
        }
    }

    println!("[Subscriber] Stopping...");

    Ok(())
}

#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    // Initialize logging
    env_logger::init();

    let config = Config::parse();

    // Set up Ctrl+C handler
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);

    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl+C, shutting down...");
        stop_clone.store(true, Ordering::SeqCst);
    })
    .expect("Failed to set Ctrl+C handler");

    // Print banner
    println!("============================================");
    println!("  quicr.rs Pub/Sub Clock Example");
    println!("============================================");
    println!();
    println!("Relay:      {}", config.relay);
    println!("Namespace:  {}", config.namespace);
    println!("Track:      {}", config.track_name);
    println!(
        "Mode:       {}",
        match config.mode {
            Mode::Publish => "Publisher",
            Mode::Subscribe => "Subscriber",
        }
    );
    println!("Endpoint:   {}", config.endpoint_id);
    println!();

    // Create client using builder
    let client = match ClientBuilder::new()
        .endpoint_id(&config.endpoint_id)
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

    // Connect to relay
    println!("Connecting to relay...");
    if let Err(e) = client.connect().await {
        eprintln!("Failed to connect: {}", e);
        return;
    }
    println!("Connected!");
    println!();

    // Run in the selected mode
    let result = match config.mode {
        Mode::Publish => run_publisher(&client, &config, stop).await,
        Mode::Subscribe => run_subscriber(&client, &config, stop).await,
    };

    // Disconnect
    if let Err(e) = client.disconnect().await {
        eprintln!("Error disconnecting: {}", e);
    }

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }

    println!("Goodbye!");
}

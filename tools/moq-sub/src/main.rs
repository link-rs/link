//! moq-sub - Simple MoQ subscriber for debugging and monitoring tracks
//!
//! Connects to a MoQ relay, subscribes to a track, and reports statistics.
//!
//! # Usage
//!
//! ```bash
//! # Simple namespace (splits by /)
//! moq-sub --relay moqt://localhost:4433 --namespace hactar/loopback --track audio
//!
//! # Tuple namespace (multiple --ns arguments)
//! moq-sub --relay moqt://localhost:4433 \
//!     --ns "moq://moq.ptt.arpa/v1" \
//!     --ns "org/acme" \
//!     --ns "store/1234" \
//!     --ns "channel/gardening" \
//!     --ns "ptt" \
//!     --track pcm_en_8khz_mono_i16
//! ```

use clap::Parser;
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
use quicr::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "moq-sub")]
#[command(about = "MoQ track subscriber with statistics reporting")]
struct Args {
    /// Relay URI (e.g., moqt://localhost:4433)
    #[arg(short, long)]
    relay: String,

    /// Track namespace as slash-separated string (e.g., "hactar/loopback")
    /// Use this OR --ns, not both.
    #[arg(long)]
    namespace: Option<String>,

    /// Namespace tuple segments. Can be specified multiple times.
    /// Each --ns adds one element to the namespace tuple.
    /// Example: --ns "moq://moq.ptt.arpa/v1" --ns "org/acme" --ns "store/1234"
    #[arg(long = "ns", num_args = 1)]
    ns_segments: Vec<String>,

    /// Track name (e.g., "audio" or "pcm_en_8khz_mono_i16")
    #[arg(short, long)]
    track: String,

    /// Endpoint identifier
    #[arg(short, long, default_value = "moq-sub")]
    endpoint_id: String,

    /// Stats reporting interval in seconds
    #[arg(short, long, default_value_t = 1)]
    interval: u64,

    /// libquicr log level (trace, debug, info, warn, error, off)
    #[arg(short, long, default_value = "warn")]
    log_level: String,

    /// Print received payload as hex (first N bytes, 0 to disable)
    #[arg(long, default_value_t = 0)]
    hex: usize,

    /// Print received payload as UTF-8 text
    #[arg(long, default_value_t = false)]
    text: bool,
}

fn parse_log_level(s: &str) -> LogLevel {
    match s.to_lowercase().as_str() {
        "trace" | "t" => LogLevel::Trace,
        "debug" | "d" => LogLevel::Debug,
        "info" | "i" => LogLevel::Info,
        "warn" | "warning" | "w" => LogLevel::Warn,
        "error" | "err" | "e" => LogLevel::Error,
        "off" | "none" | "o" => LogLevel::Off,
        _ => LogLevel::Warn,
    }
}

/// Statistics for received objects
#[derive(Default)]
struct Stats {
    /// Total objects received
    objects: u64,
    /// Total bytes received (payload only)
    bytes: u64,
    /// Objects received in current interval
    interval_objects: u64,
    /// Bytes received in current interval
    interval_bytes: u64,
    /// Last group ID seen
    last_group_id: u64,
    /// Last object ID seen
    last_object_id: u64,
    /// Number of gaps detected (missing sequence numbers)
    gaps: u64,
    /// Expected next object ID (for gap detection)
    expected_object_id: Option<u64>,
    /// Expected group ID
    expected_group_id: Option<u64>,
}

impl Stats {
    fn record(&mut self, obj: &ReceivedObject) {
        let payload_len = obj.payload().len() as u64;

        self.objects += 1;
        self.bytes += payload_len;
        self.interval_objects += 1;
        self.interval_bytes += payload_len;

        // Gap detection
        let group_id = obj.headers.group_id;
        let object_id = obj.headers.object_id;

        if let (Some(exp_group), Some(exp_obj)) = (self.expected_group_id, self.expected_object_id) {
            if group_id == exp_group {
                // Same group - check object sequence
                if object_id != exp_obj {
                    self.gaps += 1;
                }
            } else if group_id > exp_group {
                // New group started
                // This is expected, not a gap
            } else {
                // Out of order group (older group arrived late)
                self.gaps += 1;
            }
        }

        self.last_group_id = group_id;
        self.last_object_id = object_id;
        self.expected_group_id = Some(group_id);
        self.expected_object_id = Some(object_id + 1);
    }

    fn reset_interval(&mut self) {
        self.interval_objects = 0;
        self.interval_bytes = 0;
    }
}

async fn run_subscriber(client: &Client, args: &Args, ns_display: &str, stop: Arc<AtomicBool>) -> Result<()> {
    // Build namespace from either --namespace or --ns segments
    let track_name = if !args.ns_segments.is_empty() {
        // Use tuple segments
        let ns_refs: Vec<&str> = args.ns_segments.iter().map(|s| s.as_str()).collect();
        FullTrackName::from_strings(&ns_refs, &args.track)
    } else if let Some(ref namespace) = args.namespace {
        // Use slash-separated namespace (legacy behavior)
        let ns_parts: Vec<&str> = namespace.split('/').collect();
        FullTrackName::from_strings(&ns_parts, &args.track)
    } else {
        eprintln!("Error: must specify either --namespace or --ns");
        return Err(Error::config("missing namespace"));
    };

    println!("Subscribing to: {}/{}", ns_display, args.track);

    // Subscribe to the track
    let mut subscription = client.subscribe(track_name).await?;

    println!("Subscribed! Waiting for data...\n");

    let mut stats = Stats::default();
    let mut last_report = Instant::now();
    let report_interval = Duration::from_secs(args.interval);
    let start_time = Instant::now();

    loop {
        // Check stop flag with timeout
        let stop_check = async {
            Timer::after(Duration::from_millis(50)).await;
            stop.load(Ordering::Relaxed)
        };

        // Try to receive or check stop
        match select(subscription.recv(), stop_check).await {
            Either::First(obj) => {
                // Print payload if requested
                if args.hex > 0 {
                    let payload = obj.payload();
                    let len = payload.len().min(args.hex);
                    let hex: String = payload[..len].iter().map(|b| format!("{:02x}", b)).collect();
                    println!("  [g={} o={}] {} bytes: {}{}",
                        obj.headers.group_id, obj.headers.object_id,
                        payload.len(), hex,
                        if payload.len() > args.hex { "..." } else { "" }
                    );
                }
                if args.text {
                    if let Some(text) = obj.payload_str() {
                        println!("  [g={} o={}] \"{}\"",
                            obj.headers.group_id, obj.headers.object_id, text);
                    }
                }

                stats.record(&obj);
            }
            Either::Second(should_stop) => {
                if should_stop {
                    break;
                }
            }
        }

        // Report stats periodically
        if last_report.elapsed() >= report_interval {
            let elapsed_total = start_time.elapsed().as_micros() as f64 / 1_000_000.0;
            let elapsed_interval = last_report.elapsed().as_micros() as f64 / 1_000_000.0;

            let rate_pps = stats.interval_objects as f64 / elapsed_interval;
            let rate_bps = (stats.interval_bytes as f64 * 8.0) / elapsed_interval;

            let status = subscription.status();

            println!(
                "[{:6.1}s] objects: {:4} ({:5.1}/s) | bytes: {:6} ({:6.1} kbps) | total: {} obj, {} bytes | g={} o={} | gaps: {} | status: {:?}",
                elapsed_total,
                stats.interval_objects,
                rate_pps,
                stats.interval_bytes,
                rate_bps / 1000.0,
                stats.objects,
                stats.bytes,
                stats.last_group_id,
                stats.last_object_id,
                stats.gaps,
                status,
            );

            stats.reset_interval();
            last_report = Instant::now();
        }

        // Check if subscription is done
        if subscription.is_done() {
            println!("\nSubscription ended (status: {:?})", subscription.status());
            break;
        }
    }

    // Final stats
    let elapsed = start_time.elapsed().as_micros() as f64 / 1_000_000.0;
    println!("\n--- Final Statistics ---");
    println!("Duration:     {:.1}s", elapsed);
    println!("Objects:      {}", stats.objects);
    println!("Bytes:        {} ({:.1} KB)", stats.bytes, stats.bytes as f64 / 1024.0);
    println!("Avg rate:     {:.1} objects/s, {:.1} kbps",
        stats.objects as f64 / elapsed,
        (stats.bytes as f64 * 8.0) / elapsed / 1000.0
    );
    println!("Gaps:         {}", stats.gaps);

    Ok(())
}

#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    env_logger::init();

    let args = Args::parse();

    // Validate args
    if args.namespace.is_some() && !args.ns_segments.is_empty() {
        eprintln!("Error: cannot use both --namespace and --ns");
        return;
    }
    if args.namespace.is_none() && args.ns_segments.is_empty() {
        eprintln!("Error: must specify either --namespace or --ns");
        return;
    }

    // Build display string for namespace
    let ns_display = if !args.ns_segments.is_empty() {
        format!("[{}]", args.ns_segments.iter()
            .map(|s| format!("\"{}\"", s))
            .collect::<Vec<_>>()
            .join(", "))
    } else {
        args.namespace.clone().unwrap_or_default()
    };

    // Set up Ctrl+C handler
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);

    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl+C, shutting down...");
        stop_clone.store(true, Ordering::SeqCst);
    })
    .expect("Failed to set Ctrl+C handler");

    println!("moq-sub - MoQ Track Subscriber");
    println!("==============================\n");
    println!("Relay:     {}", args.relay);
    println!("Namespace: {}", ns_display);
    println!("Track:     {}", args.track);
    println!("Endpoint:  {}", args.endpoint_id);
    println!();

    // Create client
    let client = match ClientBuilder::new()
        .endpoint_id(&args.endpoint_id)
        .connect_uri(&args.relay)
        .log_level(parse_log_level(&args.log_level))
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
    println!("Connected!\n");

    // Run subscriber
    let result = run_subscriber(&client, &args, &ns_display, stop).await;

    // Disconnect
    if let Err(e) = client.disconnect().await {
        eprintln!("Error disconnecting: {}", e);
    }

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }

    println!("\nGoodbye!");
}

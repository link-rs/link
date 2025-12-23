use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[derive(Parser, Debug)]
#[command(name = "echo-server")]
#[command(about = "WebSocket echo server for testing (WSS with self-signed cert)")]
struct Args {
    /// Address to bind to
    #[arg(short, long, default_value = "0.0.0.0:9001")]
    bind: SocketAddr,

    /// Subject alternative names for the certificate (can specify multiple)
    #[arg(long, default_values_t = vec!["localhost".to_string()])]
    san: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Generate self-signed certificate
    println!("Generating self-signed certificate...");
    let CertifiedKey { cert, key_pair } = generate_simple_self_signed(args.san.clone())?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::try_from(key_pair.serialize_der()).unwrap();

    // Build TLS config
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;

    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind(&args.bind).await?;
    println!("WebSocket echo server listening on wss://{}", args.bind);
    println!("Certificate SANs: {:?}", args.san);
    println!("Note: Clients must disable certificate verification for self-signed certs");

    while let Ok((stream, addr)) = listener.accept().await {
        let acceptor = acceptor.clone();
        tokio::spawn(handle_connection(stream, addr, acceptor));
    }

    Ok(())
}

async fn handle_connection(stream: TcpStream, addr: SocketAddr, acceptor: TlsAcceptor) {
    use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
    use tokio::time::{interval, Duration};

    println!("[{}] New connection", addr);

    // TLS handshake
    let tls_stream = match acceptor.accept(stream).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[{}] TLS handshake failed: {}", addr, e);
            return;
        }
    };

    println!("[{}] TLS connected", addr);

    // WebSocket handshake
    let ws_stream = match accept_async(tls_stream).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("[{}] WebSocket handshake failed: {}", addr, e);
            return;
        }
    };

    println!("[{}] WebSocket connected", addr);

    // Stats counters
    let frames_received = Arc::new(AtomicU64::new(0));
    let bytes_received = Arc::new(AtomicU64::new(0));
    let frames_sent = Arc::new(AtomicU64::new(0));
    let bytes_sent = Arc::new(AtomicU64::new(0));
    let running = Arc::new(AtomicBool::new(true));

    // Spawn stats reporter task
    let stats_task = {
        let frames_received = frames_received.clone();
        let bytes_received = bytes_received.clone();
        let frames_sent = frames_sent.clone();
        let bytes_sent = bytes_sent.clone();
        let running = running.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(1));
            let mut last_frames_rx = 0u64;
            let mut last_bytes_rx = 0u64;
            let mut last_frames_tx = 0u64;
            let mut last_bytes_tx = 0u64;
            while running.load(Ordering::Relaxed) {
                ticker.tick().await;
                let fr = frames_received.load(Ordering::Relaxed);
                let br = bytes_received.load(Ordering::Relaxed);
                let ft = frames_sent.load(Ordering::Relaxed);
                let bt = bytes_sent.load(Ordering::Relaxed);
                let delta_fr = fr - last_frames_rx;
                let delta_br = br - last_bytes_rx;
                let delta_ft = ft - last_frames_tx;
                let delta_bt = bt - last_bytes_tx;
                if delta_fr > 0 || delta_ft > 0 {
                    println!(
                        "[{}] Stats: RX {} frames/{} bytes, TX {} frames/{} bytes (total: RX {}/{}, TX {}/{})",
                        addr, delta_fr, delta_br, delta_ft, delta_bt, fr, br, ft, bt
                    );
                }
                last_frames_rx = fr;
                last_bytes_rx = br;
                last_frames_tx = ft;
                last_bytes_tx = bt;
            }
        })
    };

    let (mut write, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Binary(data)) => {
                let len = data.len();
                frames_received.fetch_add(1, Ordering::Relaxed);
                bytes_received.fetch_add(len as u64, Ordering::Relaxed);
                if let Err(e) = write.send(Message::Binary(data)).await {
                    eprintln!("[{}] Send error: {}", addr, e);
                    break;
                }
                frames_sent.fetch_add(1, Ordering::Relaxed);
                bytes_sent.fetch_add(len as u64, Ordering::Relaxed);
            }
            Ok(Message::Text(text)) => {
                let len = text.len();
                frames_received.fetch_add(1, Ordering::Relaxed);
                bytes_received.fetch_add(len as u64, Ordering::Relaxed);
                if let Err(e) = write.send(Message::Text(text)).await {
                    eprintln!("[{}] Send error: {}", addr, e);
                    break;
                }
                frames_sent.fetch_add(1, Ordering::Relaxed);
                bytes_sent.fetch_add(len as u64, Ordering::Relaxed);
            }
            Ok(Message::Ping(data)) => {
                println!("[{}] Ping", addr);
                if let Err(e) = write.send(Message::Pong(data)).await {
                    eprintln!("[{}] Pong send error: {}", addr, e);
                    break;
                }
            }
            Ok(Message::Pong(_)) => {
                println!("[{}] Pong", addr);
            }
            Ok(Message::Close(_)) => {
                println!("[{}] Close requested", addr);
                break;
            }
            Ok(Message::Frame(_)) => {}
            Err(e) => {
                eprintln!("[{}] Error: {}", addr, e);
                break;
            }
        }
    }

    // Stop stats task
    running.store(false, Ordering::Relaxed);
    let _ = stats_task.await;

    println!("[{}] Disconnected", addr);
}

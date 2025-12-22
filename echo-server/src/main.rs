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

    let (mut write, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Binary(data)) => {
                println!("[{}] Received {} bytes, echoing", addr, data.len());
                if let Err(e) = write.send(Message::Binary(data)).await {
                    eprintln!("[{}] Send error: {}", addr, e);
                    break;
                }
            }
            Ok(Message::Text(text)) => {
                println!("[{}] Received text: {}", addr, text);
                if let Err(e) = write.send(Message::Text(text)).await {
                    eprintln!("[{}] Send error: {}", addr, e);
                    break;
                }
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

    println!("[{}] Disconnected", addr);
}

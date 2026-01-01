use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{net::SocketAddr, sync::Arc};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[derive(Parser, Debug)]
#[command(name = "echo-server")]
#[command(about = "WebSocket echo server for testing (WSS with self-signed cert)")]
struct Args {
    /// Port to bind to
    #[arg(short, long, default_value = "9001")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let bind_addr: SocketAddr = ([0, 0, 0, 0], args.port).into();

    // Generate self-signed certificate
    println!("Generating self-signed certificate...");
    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".to_string()])?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::try_from(key_pair.serialize_der()).unwrap();

    // Build TLS config
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;

    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind(bind_addr).await?;
    println!("WebSocket echo server listening on wss://{}", bind_addr);

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
                if let Err(e) = write.send(Message::Binary(data)).await {
                    eprintln!("[{}] Send error: {}", addr, e);
                    break;
                }
            }
            Ok(Message::Text(text)) => {
                if let Err(e) = write.send(Message::Text(text)).await {
                    eprintln!("[{}] Send error: {}", addr, e);
                    break;
                }
            }
            Ok(Message::Ping(data)) => {
                if let Err(e) = write.send(Message::Pong(data)).await {
                    eprintln!("[{}] Pong send error: {}", addr, e);
                    break;
                }
            }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) => break,
            Ok(Message::Frame(_)) => {}
            Err(e) => {
                eprintln!("[{}] Error: {}", addr, e);
                break;
            }
        }
    }

    println!("[{}] Disconnected", addr);
}

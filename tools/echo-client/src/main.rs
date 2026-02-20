use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::io::{self, Write};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::SerialPortBuilderExt;

#[derive(Parser)]
#[command(name = "echo-client")]
#[command(about = "Serial port echo client for testing")]
struct Args {
    /// Serial port path (e.g., /dev/cu.usbserial-110)
    #[arg(short, long)]
    port: Option<String>,

    /// Baud rate
    #[arg(short, long, default_value = "460800")]
    baud: u32,
}

fn format_byte(b: u8) -> String {
    if b >= 0x20 && b <= 0x7E {
        format!("{}", b as char)
    } else if b == b'\r' {
        String::from("\\r")
    } else if b == b'\n' {
        String::from("\\n")
    } else {
        format!("\\x{:02X}", b)
    }
}

fn auto_detect_port() -> Option<String> {
    // Try to find USB serial port
    if let Ok(ports) = tokio_serial::available_ports() {
        for port in ports {
            if port.port_name.contains("usbserial") {
                return Some(port.port_name);
            }
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Auto-detect port if not specified
    let port_name = match args.port {
        Some(p) => p,
        None => auto_detect_port().ok_or("No USB serial port found")?,
    };

    println!("Opening {} at {} baud...", port_name, args.baud);

    // Open serial port
    let mut port = tokio_serial::new(&port_name, args.baud)
        .open_native_async()?;

    println!("Connected! Type to send, Ctrl+C to exit.\n");

    // Enable raw mode for terminal
    enable_raw_mode()?;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut buf = [0u8; 1024];

        loop {
            // Check for keyboard input (non-blocking)
            if event::poll(Duration::from_millis(10))? {
                if let Event::Key(KeyEvent { code, modifiers, .. }) = event::read()? {
                    match code {
                        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                            // Ctrl+C to exit
                            print!("\r\nExiting...\r\n");
                            io::stdout().flush()?;
                            return Ok(());
                        }
                        KeyCode::Char(c) => {
                            // Send character
                            AsyncWriteExt::write_all(&mut port, &[c as u8]).await?;
                            print!("{}", c);
                            io::stdout().flush()?;
                        }
                        KeyCode::Enter => {
                            // Send CR+LF
                            AsyncWriteExt::write_all(&mut port, b"\r\n").await?;
                            print!("\r\n");
                            io::stdout().flush()?;
                        }
                        KeyCode::Backspace => {
                            // Send backspace
                            AsyncWriteExt::write_all(&mut port, &[0x08]).await?;
                            print!("\x08 \x08"); // Erase character visually
                            io::stdout().flush()?;
                        }
                        _ => {}
                    }
                }
            }

            // Try to read from serial port (non-blocking with timeout)
            match tokio::time::timeout(Duration::from_millis(10), port.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => {
                    // Print received data (use \r\n in raw mode)
                    print!("[RX {} bytes]: ", n);
                    for i in 0..n {
                        print!("{}", format_byte(buf[i]));
                    }
                    print!("\r\n"); // CR+LF for raw mode
                    io::stdout().flush()?;
                }
                _ => {
                    // Timeout or error, continue
                }
            }
        }
    }
    .await;

    // Restore terminal
    disable_raw_mode()?;

    result
}

use clap::{Parser, Subcommand};
use embedded_io_adapters::tokio_1::FromTokio;
use serialport::SerialPortType;
use std::io::Write;
use tokio_serial::SerialPortBuilderExt;

#[derive(Parser)]
#[command(name = "ctl")]
#[command(about = "Control interface for the link device", long_about = None)]
struct Cli {
    /// Serial port to use (auto-detected if not specified)
    #[arg(short, long)]
    port: Option<String>,

    #[arg(short, long, default_value = "115200")]
    baud: u32,

    #[command(subcommand)]
    command: Command,
}

/// Find USB serial ports that might be the link device
fn find_usb_serial_ports() -> Vec<String> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
        .map(|p| p.port_name)
        .collect()
}

fn select_port(specified: Option<String>) -> Result<String, String> {
    if let Some(port) = specified {
        return Ok(port);
    }

    let ports = find_usb_serial_ports();

    match ports.len() {
        0 => Err("No USB serial ports found".to_string()),
        1 => {
            println!("Auto-selected port: {}", ports[0]);
            Ok(ports[0].clone())
        }
        _ => {
            println!("Multiple USB serial ports found:");
            for (i, port) in ports.iter().enumerate() {
                println!("  {}: {}", i + 1, port);
            }
            print!("Select port [1-{}]: ", ports.len());
            std::io::stdout().flush().unwrap();

            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .map_err(|e| format!("Failed to read input: {}", e))?;

            let choice: usize = input
                .trim()
                .parse()
                .map_err(|_| "Invalid number".to_string())?;

            if choice < 1 || choice > ports.len() {
                return Err(format!(
                    "Please select a number between 1 and {}",
                    ports.len()
                ));
            }

            Ok(ports[choice - 1].clone())
        }
    }
}

#[derive(Subcommand)]
enum Command {
    Mgmt {
        #[command(subcommand)]
        action: MgmtAction,
    },

    Ui {
        #[command(subcommand)]
        action: UiAction,
    },

    Net {
        #[command(subcommand)]
        action: NetAction,
    },

    CircularPing {
        #[arg(short, long)]
        reverse: bool,

        #[arg(default_value = "hello")]
        data: String,
    },
}

#[derive(Subcommand)]
enum MgmtAction {
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },
}

#[derive(Subcommand)]
enum UiAction {
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },
}

#[derive(Subcommand)]
enum NetAction {
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let port_name = select_port(cli.port)?;
    let port = tokio_serial::new(&port_name, cli.baud).open_native_async()?;

    println!("Connected to {} at {} baud", port_name, cli.baud);

    // Split the port into read/write halves and wrap for embedded-io-async
    let (reader, writer) = tokio::io::split(port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    let mut app = link::ctl::App::new(writer, reader);

    match cli.command {
        Command::Mgmt { action } => match action {
            MgmtAction::Ping { data } => {
                println!("Sending MGMT ping with data: {}", data);
                app.send_mgmt_ping(data.as_bytes()).await;
                println!("Received pong!");
            }
        },
        Command::Ui { action } => match action {
            UiAction::Ping { data } => {
                println!("Sending UI ping with data: {}", data);
                app.send_ui_ping(data.as_bytes()).await;
                println!("Received pong!");
            }
        },
        Command::Net { action } => match action {
            NetAction::Ping { data } => {
                println!("Sending NET ping with data: {}", data);
                app.send_net_ping(data.as_bytes()).await;
                println!("Received pong!");
            }
        },
        Command::CircularPing { reverse, data } => {
            if reverse {
                println!("Sending NET-first circular ping with data: {}", data);
                app.net_first_circular_ping(data.as_bytes()).await;
            } else {
                println!("Sending UI-first circular ping with data: {}", data);
                app.ui_first_circular_ping(data.as_bytes()).await;
            }
            println!("Completed circular ping!");
        }
    }

    Ok(())
}

use clap::{Parser, Subcommand};
use embedded_io_adapters::tokio_1::FromTokio;
use tokio_serial::SerialPortBuilderExt;

#[derive(Parser)]
#[command(name = "ctl")]
#[command(about = "Control interface for the link device", long_about = None)]
struct Cli {
    #[arg(short, long)]
    port: String,

    #[arg(short, long, default_value = "115200")]
    baud: u32,

    command: Command,
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

    // Open the serial port
    let port = tokio_serial::new(&cli.port, cli.baud).open_native_async()?;

    println!("Connected to {} at {} baud", cli.port, cli.baud);

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

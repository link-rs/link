use clap::Parser;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{self, ClearType},
};
use tokio::io::AsyncWriteExt;
use tokio_serial::SerialPort;

use std::io::{self, Write};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "serial-debug")]
#[command(about = "Debug serial port DTR/RTS signals")]
struct Cli {
    /// Serial port name (e.g. /dev/cu.usbserial-10)
    port: String,

    /// Baud rate
    #[arg(short, long, default_value = "115200")]
    baud: u32,
}

/// Set the terminal scroll region (DECSTBM). top and bottom are 1-based.
fn set_scroll_region(stdout: &mut io::Stdout, top: u16, bottom: u16) -> io::Result<()> {
    write!(stdout, "\x1b[{};{}r", top, bottom)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let builder = tokio_serial::new(&cli.port, cli.baud).parity(tokio_serial::Parity::Even);
    let mut port = tokio_serial::SerialStream::open(&builder)?;

    // Initial signal state
    let mut dtr = false;
    let mut rts = false;
    port.write_data_terminal_ready(dtr)?;
    port.write_request_to_send(rts)?;

    // Enter raw mode
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    let (_, rows) = terminal::size()?;

    // Clear screen, draw header on row 0, set scroll region to rows 2..end
    execute!(stdout, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    draw_header(&mut stdout, &cli.port, dtr, rts)?;
    set_scroll_region(&mut stdout, 2, rows)?; // rows are 1-based for DECSTBM
    execute!(stdout, cursor::MoveTo(0, 1))?;

    let mut buf = [0u8; 256];

    loop {
        // Check for key presses
        if event::poll(Duration::from_millis(10))? {
            match event::read()? {
                Event::Key(KeyEvent { code, .. }) => match code {
                    KeyCode::Char('f') | KeyCode::Char('F') => {
                        dtr = true;
                        port.write_data_terminal_ready(dtr)?;
                        draw_header(&mut stdout, &cli.port, dtr, rts)?;
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => {
                        dtr = false;
                        port.write_data_terminal_ready(dtr)?;
                        draw_header(&mut stdout, &cli.port, dtr, rts)?;
                    }
                    KeyCode::Char('j') | KeyCode::Char('J') => {
                        rts = true;
                        port.write_request_to_send(rts)?;
                        draw_header(&mut stdout, &cli.port, dtr, rts)?;
                    }
                    KeyCode::Char('k') | KeyCode::Char('K') => {
                        rts = false;
                        port.write_request_to_send(rts)?;
                        draw_header(&mut stdout, &cli.port, dtr, rts)?;
                    }
                    KeyCode::Char('s') | KeyCode::Char('S') => {
                        AsyncWriteExt::write_all(&mut port, &[0x7F]).await?;
                        AsyncWriteExt::flush(&mut port).await?;
                    }
                    KeyCode::Char('g') | KeyCode::Char('G') => {
                        AsyncWriteExt::write_all(&mut port, &[0x00, 0xFF]).await?;
                        AsyncWriteExt::flush(&mut port).await?;
                    }
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                        break;
                    }
                    _ => {}
                },
                Event::Resize(_, new_rows) => {
                    set_scroll_region(&mut stdout, 2, new_rows)?;
                    draw_header(&mut stdout, &cli.port, dtr, rts)?;
                }
                _ => {}
            }
        }

        // Read serial data (non-blocking via try_read)
        match port.try_read(&mut buf) {
            Ok(n) if n > 0 => {
                let mut display = String::new();
                for &b in &buf[..n] {
                    if b >= 0x20 && b <= 0x7e {
                        display.push(b as char);
                    } else if b == b'\n' {
                        display.push('\n');
                    } else if b == b'\r' {
                        display.push('\r');
                    } else {
                        display.push_str(&format!("\\x{:02X}", b));
                    }
                }
                write!(stdout, "{}", display)?;
                stdout.flush()?;
            }
            _ => {}
        }
    }

    // Cleanup: reset scroll region, move below content
    set_scroll_region(&mut stdout, 1, rows)?;
    execute!(stdout, cursor::MoveTo(0, rows - 1), cursor::Show)?;
    terminal::disable_raw_mode()?;
    println!();

    Ok(())
}

fn draw_header(
    stdout: &mut io::Stdout,
    port: &str,
    dtr: bool,
    rts: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    execute!(stdout, cursor::SavePosition, cursor::MoveTo(0, 0))?;
    write!(
        stdout,
        "\x1b[7m {port}  |  DTR={dtr:<5}  RTS={rts:<5}  |  F/D=DTR  J/K=RTS  S=0x7F  G=0x00FF  Q=Quit \x1b[0m\x1b[K",
    )?;
    execute!(stdout, cursor::RestorePosition)?;
    stdout.flush()?;
    Ok(())
}

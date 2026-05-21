//! Throughput test against the `mgmt-dfu` firmware's vendor-bulk echo
//! endpoint. Sends a fixed amount of data in 64-byte chunks while
//! concurrently reading the echo back, and prints elapsed time +
//! KB/s + MB/s + baud.

use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use nusb::transfer::RequestBuffer;

const VID: u16 = 0xc0de;
const PID: u16 = 0xcafb;
const CHUNK: usize = 64;

// mgmt-dfu's vendor-class interface: bulk-OUT 0x01, bulk-IN 0x81.
const EP_OUT: u8 = 0x01;
const EP_IN: u8 = 0x81;

#[derive(Parser)]
#[command(about = "USB throughput test against mgmt-dfu's vendor-bulk echo endpoint")]
struct Args {
    /// Total bytes to transfer (rounded down to a multiple of 64).
    #[arg(default_value_t = 1_000_000)]
    bytes: usize,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let chunks = args.bytes / CHUNK;
    let total_bytes = chunks * CHUNK;

    let dev_info = nusb::list_devices()?
        .find(|d| d.vendor_id() == VID && d.product_id() == PID)
        .ok_or_else(|| anyhow!("device {:04x}:{:04x} not found — is mgmt-dfu running?", VID, PID))?;
    let device = dev_info.open().context("opening device")?;
    let interface = device.claim_interface(0).context("claiming interface 0")?;

    let pattern: Vec<u8> = (0..CHUNK).map(|i| (i & 0xff) as u8).collect();

    println!(
        "Sending {} bytes ({} × {}-byte chunks) to {:04x}:{:04x}...",
        total_bytes, chunks, CHUNK, VID, PID,
    );

    let start = Instant::now();

    let send_fut = async {
        for _ in 0..chunks {
            interface
                .bulk_out(EP_OUT, pattern.clone())
                .await
                .status
                .map_err(|e| anyhow!("bulk_out failed: {e}"))?;
        }
        Ok::<(), anyhow::Error>(())
    };

    let recv_fut = async {
        let mut received = 0usize;
        for _ in 0..chunks {
            let comp = interface.bulk_in(EP_IN, RequestBuffer::new(CHUNK)).await;
            received += comp
                .into_result()
                .map_err(|e| anyhow!("bulk_in failed: {e}"))?
                .len();
        }
        Ok::<usize, anyhow::Error>(received)
    };

    let (send_res, recv_res) = tokio::join!(send_fut, recv_fut);
    send_res?;
    let received = recv_res?;

    let elapsed = start.elapsed();
    let secs = elapsed.as_secs_f64();
    let kbps = (total_bytes as f64 / 1024.0) / secs;
    let mbps = kbps / 1024.0;
    let baud = (total_bytes as f64 * 8.0) / secs;
    println!(
        "Sent {} bytes, received {} bytes in {:.3} s = {:.1} KB/s ({:.3} MB/s, {:.0} baud)",
        total_bytes, received, secs, kbps, mbps, baud,
    );

    Ok(())
}

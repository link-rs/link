# MGMT Echo Test Firmware

Simple firmware for the MGMT chip that echoes characters from the CTL UART as fast as possible.

## Usage

### Flash with default baud rate (460800)

```bash
cd tools/mgmt-echo
cargo run
```

### Flash with custom baud rate

```bash
cd tools/mgmt-echo
BAUD_RATE=115200 cargo run
BAUD_RATE=921600 cargo run
BAUD_RATE=1000000 cargo run
```

This will build the firmware with the specified baud rate and automatically flash it to the MGMT chip using the ctl program.

### Build only (no flash)

```bash
cargo build                    # Default 460800
BAUD_RATE=115200 cargo build   # Custom baud rate
```

The binary will be at: `target/thumbv6m-none-eabi/debug/mgmt-echo.bin`

## Testing

After flashing at a specific baud rate, test the echo using the ctl program at the same baud rate:

```bash
cd ../../ctl
cargo run -- --baud 460800 mgmt ping "hello"
cargo run -- --baud 115200 mgmt ping "hello"
```

## Configuration

The UART baud rate is configured at build time via the `BAUD_RATE` environment variable (defaults to 460800).

The firmware uses even parity and 1 stop bit for STM32 bootloader compatibility, so you can always reflash using the bootloader at 115200 baud.

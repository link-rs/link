# Link Quickstart Guide

This guide walks you through setting up a development environment, building the project, flashing firmware to a Link device, and running loopback demos.

## Prerequisites

### Operating System

This guide assumes macOS. Linux users can adapt the commands (use `apt` or equivalent instead of `brew`).

### Hardware Required

- **Link Device** with MGMT, UI, and NET chips
- **USB cable** for connecting to the device

> **Optional for development**: ST-LINK V2 (for STM32 debugging) and ESP-PROG (for ESP32 debugging) are only needed if you want to use `cargo run` for live debugging with RTT output.

## 1. Install Required Tools

### 1.1 Install Rust

If you don't have Rust installed:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

Verify installation:

```bash
rustc --version
cargo --version
```

### 1.2 Install STM32 Toolchains

Add the ARM Cortex-M targets for the MGMT (M0) and UI (M4F) chips:

```bash
rustup target add thumbv6m-none-eabi    # MGMT chip (STM32F072CB, Cortex-M0)
rustup target add thumbv7em-none-eabihf # UI chip (STM32F405RG, Cortex-M4F)
```

### 1.3 Install ESP32 Toolchain

Install the ESP-RS toolchain manager and set up the ESP32-S3 environment:

```bash
cargo install espup --locked
espup install
```

After installation, add the environment variables to your shell. Add this to your `~/.zshrc` or `~/.bashrc`:

```bash
# ESP-RS toolchain
. "$HOME/.rustup/toolchains/esp/env.sh"
```

Then reload your shell:

```bash
source ~/.zshrc  # or source ~/.bashrc
```

### 1.4 Install Probe Tools

Install probe-rs for flashing STM32 chips:

```bash
# macOS
brew install probe-rs-tools

# Or via cargo
cargo install probe-rs-tools --locked
```

### 1.5 Install WASM Tools (for web interfaces)

```bash
cargo install wasm-pack --locked
```

### 1.6 Install Binary Utilities

For creating `.bin` files from ELF:

```bash
cargo install cargo-binutils
rustup component add llvm-tools-preview
```

### 1.7 Verify Installation

Check that all tools are available:

```bash
rustup target list --installed | grep -E "(thumbv6m|thumbv7em)"
probe-rs --version
wasm-pack --version
cargo objcopy --version
```

## 2. Clone and Build

### 2.1 Clone the Repository

```bash
git clone <repository-url> link
cd link
```

### 2.2 Build and Test the Core Library

The `link` crate contains most of the logic and can be tested on the host machine:

```bash
cd link
cargo test --features std
```

You should see output like:

```
running 80 tests
test net::storage::tests::add_and_get_wifi_ssid ... ok
test shared::jitter_buffer::tests::test_initial_state ... ok
...
test result: ok. 80 passed; 0 failed; 0 ignored
```

### 2.3 Build the CTL Tool

The `ctl` command-line tool is used to control and flash the device:

```bash
cd ../ctl
cargo build --release
```

You can run it directly:

```bash
cargo run -- --help
```

## 3. Flash Firmware to Device

The firmware is flashed via USB serial using the built-in bootloaders on each chip. No external debuggers (ST-LINK, ESP-PROG) are required for normal flashing.

### 3.1 Connect the Device

Connect the Link device to your computer via USB. The device should enumerate as a serial port.

### 3.2 Flash All Chips

From the repository root, flash each chip using the Makefile targets:

```bash
# Flash MGMT firmware (requires manual bootloader entry - see note below)
make flash-mgmt

# Flash UI firmware (auto-resets to bootloader)
make flash-ui

# Flash NET firmware (auto-resets to bootloader)
make flash-net
```

> **Note for MGMT chip**: The MGMT chip cannot be auto-reset to bootloader mode. Before running `make flash-mgmt`, you must:
> 1. Set the BOOT0 pin HIGH on the MGMT chip
> 2. Reset the device
> 3. Then run `make flash-mgmt`
> 4. After flashing, set BOOT0 LOW and reset again

The UI and NET chips are automatically reset to bootloader mode by the MGMT chip, so no manual intervention is needed.

### 3.3 Verify Firmware

After flashing, the LEDs should indicate:
- **MGMT**: Red and green LEDs lit
- **UI**: Blue LED lit
- **NET**: Blue LED lit (or red if WiFi not configured)

### 3.4 Verify Device Communication

Test communication with each chip:

```bash
cd ctl
cargo run -- mgmt ping
cargo run -- ui ping
cargo run -- net ping
```

All three should respond with "Received pong!".

## 4. Run Loopback Demos

Loopback modes let you test audio without a network connection.

### 4.1 UI Loopback (Hardware Test)

UI loopback routes microphone audio directly to the speaker, bypassing the network entirely. This tests the audio hardware.

```bash
# Enable UI loopback
cargo run -- ui loopback set true

# Press the mic button on the device and speak
# You should hear your voice through the speaker

# Disable when done
cargo run -- ui loopback set false
```

### 4.2 NET Loopback (Inter-Chip Test)

NET loopback routes audio from UI → NET → UI, testing the full inter-chip audio path without using WiFi.

```bash
# Enable NET loopback
cargo run -- net loopback set true

# Press the mic button and speak
# Audio goes through the jitter buffer on NET and back to UI

# Disable when done
cargo run -- net loopback set false
```

### 4.3 WebSocket Loopback (Full Network Test)

For full network testing, you need a relay server that echoes audio back.

First, configure WiFi and the relay URL:

```bash
# Add WiFi network
cargo run -- net wifi add "YourSSID" "YourPassword"

# Set relay URL (use an echo server for testing)
cargo run -- net relay-url set "wss://your-relay-server.com/echo"

# Reset NET to apply settings
cargo run -- net reset
```

Then test WebSocket connectivity:

```bash
# Test WebSocket ping
cargo run -- net ws-ping

# Run echo test (measures round-trip latency)
cargo run -- net ws-echo-test
```

## 5. Run Web Interfaces

### 5.1 Build Web-CTL

The web-ctl interface provides browser-based device control:

```bash
# From the repository root
make web-ctl
```

This builds the WASM module and copies firmware binaries to the web directory.

### 5.2 Serve Web-CTL

```bash
make serve-ctl
```

Open http://localhost:8080 in Chrome or Edge (WebSerial requires these browsers).

**Using Web-CTL:**
1. Click "Connect to Device"
2. Select the serial port for your Link device
3. All state variables are automatically loaded
4. Use the UI to:
   - Toggle UI/NET loopback
   - Configure WiFi networks
   - Set relay URL
   - Flash firmware

### 5.3 Build and Serve Web-Link (Virtual Device)

Web-Link simulates a Link device in the browser for testing without hardware:

```bash
make web-link
make serve-link
```

Open http://localhost:8081 in your browser.

## 6. Quick Reference

### CTL Commands

```bash
# Ping each chip
ctl mgmt ping
ctl ui ping
ctl net ping

# UI chip commands
ctl ui version                    # Get firmware version
ctl ui version set 123            # Set firmware version
ctl ui sframe-key                 # Get encryption key
ctl ui sframe-key set <32-hex>    # Set encryption key
ctl ui loopback                   # Get loopback state
ctl ui loopback set true          # Enable loopback
ctl ui loopback set false         # Disable loopback

# NET chip commands
ctl net wifi                      # List WiFi networks
ctl net wifi add SSID password    # Add WiFi network
ctl net wifi clear                # Remove all networks
ctl net relay-url                 # Get relay URL
ctl net relay-url set wss://...   # Set relay URL
ctl net loopback                  # Get loopback state
ctl net loopback set true         # Enable loopback
ctl net ws-ping                   # Test WebSocket
ctl net ws-echo-test              # Run echo test

# Flashing (via USB, not debugger)
ctl ui flash firmware.bin
ctl net flash firmware.bin -c     # -c for compressed
ctl mgmt flash firmware.bin       # Requires manual bootloader entry
```

### Makefile Targets

```bash
make flash-ui      # Build and flash UI firmware
make flash-mgmt    # Build and flash MGMT firmware
make flash-net     # Build and flash NET firmware

make web-ctl       # Build web control interface
make serve-web     # Serve web-ctl at localhost:8080

make web-link      # Build virtual device
make serve-link    # Serve web-link at localhost:8081

make clean         # Clean all build artifacts
```

## 7. Troubleshooting

### "No serial port found"

- Ensure the device is connected via USB
- Check that you have permissions to access serial ports
- On macOS, look for `/dev/tty.usbmodem*` or `/dev/tty.usbserial*`

### "Connection test failed"

- Verify all three chips are flashed with compatible firmware
- Check that MGMT is running (LEDs should be lit)
- Try resetting the device

### "WebSerial not available"

- WebSerial only works in Chrome or Edge
- Must be served over HTTPS or localhost
- Check browser console for errors

### NET (ESP32) flash fails

- Check that `espup` environment is loaded (`. ~/.rustup/toolchains/esp/env.sh`)
- Ensure MGMT chip is running (it controls NET reset)
- Try running `ctl net reset` before flashing

### Tests fail with "unresolved module"

- Run tests with std feature: `cargo test --features std`
- The `ctl` module requires std for bootloader support

## 8. Development Workflow

### Iterating on Core Logic

Most logic lives in the `link` crate. Develop and test there first:

```bash
cd link
cargo test --features std
cargo clippy --features std
```

### Monitoring Debug Output (requires debuggers)

For development with live debug output, connect debuggers and use `cargo run`:

```bash
# For UI chip (requires ST-LINK on UI header)
cd ui
cargo run --release

# For MGMT chip (requires ST-LINK on MGMT header)
cd mgmt
cargo run --release

# For NET chip (requires ESP-PROG)
cd net
cargo run --release
```

This flashes firmware and streams RTT debug logs to your terminal.

### Quick Web-CTL Rebuild

To rebuild just the WASM without copying firmware:

```bash
make web-ctl-quick
```

## Next Steps

- Read [ARCHITECTURE.md](ARCHITECTURE.md) for system design details
- See `ctl --help` for complete command reference
- Check the `echo-server/` directory for relay server implementation

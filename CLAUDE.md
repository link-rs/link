# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Link is an embedded audio device built around three microcontrollers that communicate over UART using a TLV (Type-Length-Value) protocol:

- **MGMT** (STM32F072CB, Cortex-M0) - Management hub that bridges USB serial to UI and NET chips, controls reset/boot pins
- **UI** (STM32F405RG, Cortex-M4F) - Handles audio I/O, encryption (SFrame), user buttons
- **NET** (ESP32-S3) - WiFi connectivity and MoQ media relay

Two control programs talk to the device:
- **ctl** - Command-line interface (single commands or interactive REPL)
- **web-ctl** - Browser-based interface using Web Serial API

## Architecture

The `link` crate contains all shared logic and is feature-gated for each target:

```
link/           # Shared library (no_std compatible)
├── shared/     # Protocol, TLV, uart_config, mocks for testing
├── ctl/        # Host-side CtlCore (async, generic over port type)
├── mgmt/       # MGMT chip logic
├── ui/         # UI chip logic (audio, sframe, eeprom)
└── net/        # NET chip logic (wifi, jitter buffer)
```

The firmware crates (`mgmt/`, `net/`, `ui/`) are thin wrappers that instantiate `link` on real hardware. This allows:
- Full-stack integration tests with mock hardware
- Shared code between CLI (`ctl`) and browser (`web-ctl`) via WASM

### Communication Flow

```
CTL <--UART--> MGMT <--UART--> UI <--UART--> NET
                    \                       /
                     +-------UART----------+
```

CTL communicates with UI/NET by tunneling TLVs through MGMT using `ToUi`/`ToNet` and `FromUi`/`FromNet` message types. MGMT forwards raw bytes, so TLVs may be fragmented. CTL maintains per-chip stream buffers and uses sync word scanning ("LINK" = 0x4C494E4B).

## Build Commands

```bash
# Build everything
make all

# Build individual components
cd ctl && cargo build              # CLI tool
cd mgmt && cargo objcopy -- -O binary target/thumbv6m-none-eabi/debug/mgmt.bin
cd ui && cargo objcopy -- -O binary target/thumbv7em-none-eabihf/debug/ui.bin
cd net && cargo build              # ESP32 (requires espup toolchain)
cd web-ctl && wasm-pack build --target web --out-dir www/pkg

# Run unit tests (requires --features std for link crate)
cd link && cargo test --features std

# Flash firmware (requires connected device)
make flash-mgmt
make flash-ui
make flash-net
make flash-all

# Run hardware integration tests (requires connected EV16 device)
make test-ctl

# Format all crates
make format

# Preflight checks (build + test + format check)
make preflight
```

## CTL Usage

```bash
# CLI mode (auto-detects port)
cargo run -- mgmt ping
cargo run -- ui info
cargo run -- net wifi add "SSID" "password"

# REPL mode
cargo run
# then: mgmt ping, ui loopback sframe, net wifi, etc.

# Specify port
cargo run -- -p /dev/ttyUSB0 mgmt ping
```

Note: Each CTL invocation resets the device via DTR. For persistent in-memory state (e.g., loopback mode), use REPL or web-ctl.

## Key Files

- `link/src/shared/protocol.rs` - TLV message type enums (`CtlToMgmt`, `MgmtToCtl`, `CtlToUi`, etc.)
- `link/src/shared/tlv.rs` - TLV encoding/decoding, sync word scanning
- `link/src/ctl/core.rs` - `CtlCore<P>` async operations (ping, flash, tunneling)
- `link/src/mgmt/mod.rs` - MGMT chip main loop, message routing
- `docs/tlv-protocol-spec.md` - Complete protocol specification

## UART Configuration

| Link        | Baud Rate | Parity | Notes                              |
|-------------|-----------|--------|------------------------------------|
| CTL -- MGMT | 1000000   | Even   | DTR controls MGMT reset            |
| MGMT -- UI  | 1000000   | Even   | Switchable to 115200 for flashing  |
| MGMT -- NET | 1000000   | None   | ESP32 bootloader compatibility     |
| UI -- NET   | 1000000   | Even   | Direct audio link                  |

## Testing

- **Unit tests**: `cd link && cargo test --features std` - Tests shared logic with mocks
- **Integration tests**: `link/src/integration_tests.rs` - Full-stack tests with all chips mocked
- **Hardware tests**: `./ctl/test.sh` - Requires physical EV16 device

## Toolchain Requirements

- **STM32 (MGMT, UI)**: `rustup target add thumbv6m-none-eabi thumbv7em-none-eabihf`, `cargo install cargo-binutils`
- **ESP32 (NET)**: Install via `espup install`, source `$HOME/.rustup/toolchains/esp/env.sh`
- **WASM**: `cargo install wasm-pack --locked`

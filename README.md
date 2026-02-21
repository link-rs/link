# Link

## Overview

Link is an embedded audio device built around three microcontrollers:

- **MGMT** (STM32F072CB, Cortex-M0) — management hub that bridges the host, UI, and NET chips over UART
- **UI** (STM32F405RG, Cortex-M4F) — handles audio and user interaction
- **NET** (ESP32-S3) — provides WiFi connectivity and media relay via MoQ

Two control programs talk to the device over USB serial:

- **ctl** — command-line interface (single commands or interactive REPL)
- **web-ctl** — browser-based interface using the Web Serial API

All logic that does not strictly depend on the hardware platform lives in the `link` crate, which is shared across all chips and both control programs.  This allows for robust testing (via mock hardware) and instantiation on multiple platforms (e.g., CLI vs. WASM).  The chip-specific firmware crates (`mgmt`, `net`, `ui`) are thin wrappers that instantiate `link` on real hardware.

## Prerequisites

Install Rust via [rustup](https://rustup.rs/):

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

You only need the toolchains below for the parts of the project you're working on. The `link` crate, `ctl`, and `web-ctl` build with standard Rust.

### For STM32 chips (MGMT and UI)

Add the ARM cross-compilation targets and install `cargo-binutils` (used by `cargo objcopy` to produce `.bin` files):

```
rustup target add thumbv6m-none-eabi thumbv7em-none-eabihf
cargo install cargo-binutils
rustup component add llvm-tools-preview
```

Flashing is done over USB serial via `ctl`, so no hardware debugger or probe-rs install is needed for normal development.

### For ESP32-S3 chip (NET)

Install the Xtensa Rust toolchain with [espup](https://github.com/esp-rs/espup):

```
cargo install espup --locked
espup install
```

Then add the following to your shell profile (`.zshrc`, `.bashrc`, etc.) and reload:

```
. "$HOME/.rustup/toolchains/esp/env.sh"
```

You'll also need the `ldproxy` linker wrapper:

```
cargo install ldproxy
```

### For web-ctl / web-link (WASM)

Install [wasm-pack](https://rustwasm.github.io/wasm-pack/):

```
cargo install wasm-pack --locked
```

Python 3 is needed for the local dev server (`make serve-ctl`). It's pre-installed on macOS and most Linux distributions.

## Running the Device Controllers

**ctl** (CLI): From the `ctl/` directory, run individual commands or start an interactive REPL:

```
cargo run -- mgmt ping
cargo run -- ui info
cargo run -- net wifi status
cargo run                      # starts the REPL
```

You can also run `cargo install` and get a compiled binary that you can invoke anywhere.

Note that as of EV16, each invocation of `ctl` resets the device.  So for example, `ctl ui loopback alaw` will do nothing, because the loopback configuration is only stored in RAM and will revert back to the default (`off`) on reset.  If you want to control in-memory parameters, it's better to use the REPL or `web-ctl`.

**web-ctl** (browser): Build and serve with:

```
make serve-ctl
```

Then open http://localhost:8000 and connect via the Web Serial API.

## Building and Flashing Firmware

Build any firmware with `cargo build` in its directory, or build everything with `make all`.

Don't use `cargo run` for the firmware crates — those expect a hardware debugger probe and are not tested. Instead, flash over USB serial using the Makefile targets:

```
make flash-mgmt
make flash-ui
make flash-net
make flash-all  # all three in sequence
```

These targets build the firmware first, then flash it through `ctl`.

## Crate Map

| Crate | Description |
|-------|-------------|
| `link` | Shared library — protocol definitions, device logic, integration tests |
| `mgmt` | MGMT firmware (STM32F072CB) |
| `ui` | UI firmware (STM32F405RG) |
| `net` | NET firmware (ESP32-S3) |
| `ctl` | CLI control program |
| `web-ctl` | Browser-based control program (WASM) |
| `web-link` | Virtual Link device that runs entirely in the browser, for testing without hardware |
| `tools/` | Assorted development utilities (serial debugger, echo tests, audio debugging, MoQ tools) |

Note that `web-link` is currently broken due to the lack of a web MOQ implementation.
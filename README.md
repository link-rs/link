# Link

## Overview

* `link` - Main logic crate.  Most of the logic should go here, so that we can
  make thorough, end-to-end test cases.
* `mgmt`, `net`, `ui` - Firmwares for the three chips.  Thin wrappers around the
  logic in `link`; basically just instantiates peripherals and an async
  environment and calls through to the `link` logic.
* `ctl` - Control program to run from a laptop connected to the device.
* `lib` - Tools that are helpful across multiple chips
* `vendor` - Dependencies that have been vendored so that they can be modified

## Prerequisites

```
# If you don't already have `rustup`
curl - proto '=https' - tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install probe-rs for connecting to devices
brew install probe-rs-tools

# Install the STM toolchains
rustup target add thumbv7em-none-eabihf
rustup target add thumbv6m-none-eabi

# Install the ESP toolchain
cargo install espup --locked
espup install
# Set env vars: https://github.com/esp-rs/espup?tab=readme-ov-file#environment-variables-setup
```

## Quickstart

```
# Build the core module and run tests
cd link
cargo test

# Build the MGMT firmware and flash it
# Connect ST-LINK to the MGMT header
cd ../mgmt
cargo run
# Wait for debug logs to start appearing, then Ctrl-C
# MGMT LEDs should light up red and green

# Build the NET firmware and flash it
# Connect ESP-PROG to NET header
cd ../net
cargo run
# Wait for debug logs to start appearing, then Ctrl-C
# NET LED should light up blue

# Build the UI firmware and flash it
# Connect ST-LINK to the UI header
cd ../ui
cargo run
# Wait for debug logs to start appearing, then Ctrl-C
# UI LED should light up blue

# Verify that the device works
cd ../ctl
./all-ping.sh
```

## PTT Demo

```
# In one window
cd ui
cargo run

# In another window
cd net
cargo run

# Push buttons A or B, or the mic button
# Behold that audio frames are collected at UI and sent UI->NET
```

# CTL vs WEB-CTL Architecture Comparison

**Date**: 2026-02-20
**Purpose**: Document functional gaps and architectural differences between the native and web implementations.

---

## Executive Summary

Both `ctl` (native CLI) and `web-ctl` (WebAssembly) share the `link::ctl` library for core device communication. Most protocol logic is centralized. Display mappings, validation logic, timeout constants, and utility functions are all in the shared library.

**Remaining gaps:**
1. **Even parity** is hardcoded in both stacks (unavoidable platform glue)

---

## 1. Remaining Feature Gaps

### 1.1 CTL-Only Features

| Feature | CTL Location | Status in WEB-CTL |
|---------|-------------|-------------------|
| **REPL Mode** | `main.rs:629-649` | N/A -- browser has its own UI |
| **Device Auto-Discovery** | `main.rs:398-444` | N/A -- WebSerial requires user gesture |
| **Monitor with ESC to Exit** | `handlers/ui.rs:248-314`, `handlers/net.rs:321-396` | N/A -- web uses polling APIs instead |

### 1.2 WEB-CTL-Only Features

| Feature | WEB-CTL Location | Notes |
|---------|-----------------|-------|
| **Reconnect API** | `lib.rs:117-129` | Web-specific -- handles port disconnect/reconnect |
| **Is Connected Check** | `lib.rs:97-100` | Web-specific -- implicit in native |

### 1.3 Unavoidable Platform Duplication

**Even Parity** -- Both hardcode even parity in their serial port setup. This is a fundamental protocol requirement. The constant exists in `link::uart_config` but parity must be set at the platform level, so this duplication is unavoidable.

---

## 2. Architectural Differences (No Action Needed)

### 2.1 Bootloader Info Commands

**CTL**: Full interactive flow with console output, hex dumps, and vector table analysis.
**WEB-CTL**: Returns structured JavaScript objects. Caller manages bootloader entry/exit separately.

### 2.2 Loopback Mode APIs

**CTL**: Named CLI subcommands (`off`, `raw`, `alaw`, `sframe`).
**WEB-CTL**: Numeric modes (`set_ui_loopback_mode(0-3)`), returns string from getter.

Both call the same `link::ctl` functions underneath -- different API styles for different platforms.

### 2.3 Monitor Commands

**CTL**: Rust async loop with ESC key detection (crossterm), blocks until exit.
**WEB-CTL**: Single-shot read APIs called by JavaScript event loop.

---

## 3. Shared Library Summary

| Module | What's There |
|--------|-------------|
| `protocol.rs` | All TLV types, `ChannelId` (with `ALL` + `Display`), `UiLoopbackMode` / `NetLoopbackMode` (with `Display`), `JitterState` enum, `JitterStatsInfo` |
| `protocol_config.rs` | Retry counts, timeout values (`BOOTLOADER_PROBE_MS`, `MONITOR_MS`, `NORMAL_SECS`), channel ID constants |
| `uart_config.rs` | Baud rates (`HIGH_SPEED`, `LOW_SPEED`), parity settings |
| `channel.rs` | `ChannelConfig` (with `relay_url_display()`) |
| `ctl/core.rs` | All device communication, `CtlError::is_timeout()`, `set_sframe_key()` with validation, `escape_non_ascii()` |
| `ctl/flash.rs` | `FlashPhase` (with `Display`), `MgmtBootloaderInfo` (with `version_major/minor()`, `sp()`, `reset_handler()`, `sp_valid()`, `reset_valid()`), `MgmtBootloaderEntry`, `AsyncDelay` |
| `ctl/mod.rs` | `interpret_esp32_security()` |
| `ctl/stm.rs` | STM32 bootloader protocol, `chip_name()`, `command_name()` |

---

## 4. Previously Resolved Issues

The following issues were identified and resolved by centralizing logic into the shared `link` library:

- **SFrame key validation** -- `set_sframe_key()` now accepts `&[u8]` and validates length internally
- **Jitter buffer state mapping** -- `JitterState` enum with `Display` impl replaces divergent integer mapping
- **Loopback mode strings** -- `Display` impls on `UiLoopbackMode` and `NetLoopbackMode`
- **Timeout constants** -- both stacks now use `link::protocol_config::timeouts::*`
- **NET flash baud rate** -- both now use `link::uart_config::HIGH_SPEED.baudrate`
- **ESP32 security fuse interpretation** -- `interpret_esp32_security()` shared helper
- **NET monitor output** -- both now use `escape_non_ascii()`
- **Bootloader version parsing** -- `version_major()` / `version_minor()` on `MgmtBootloaderInfo`
- **Vector table validation** -- `sp()`, `reset_handler()`, `sp_valid()`, `reset_valid()` on `MgmtBootloaderInfo`
- **Bootloader entry workflow** -- extracted `enter_mgmt_bootloader()` helper in ctl
- **Channel name mapping** -- `ChannelId::ALL` constant and `Display` impl
- **Timeout error matching** -- `CtlError::is_timeout()` method
- **Relay URL display** -- `ChannelConfig::relay_url_display()` method
- **FlashPhase string mapping** -- `Display` impl on `FlashPhase`
- **Channel management** -- added `get/set/clear_channel_config()` and `get_all_channel_configs()` to web-ctl
- **Reset hold/release** -- added `hold_ui_reset()` / `hold_net_reset()` to web-ctl

---

**Document Version**: 3.0
**Last Updated**: 2026-02-20
**Author**: Claude (Code Audit)

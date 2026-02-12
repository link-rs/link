# CTL Architecture Review

## Module Summary

### link::ctl — the shared library

| File | Lines | Role |
|---|---|---|
| `port.rs` | 166 | `CtlPort` trait — async I/O abstraction, `SetTimeout`, `SetBaudRate` |
| `core.rs` | 1287 | `CtlCore<P>` — ALL device operations: TLV protocol, MGMT/UI/NET commands, tunneling |
| `stm.rs` | 537 | STM32 USART bootloader protocol (AN3155) |
| `flash.rs` | 1391 | High-level flashing for MGMT (STM32), UI (STM32), NET (ESP32) |
| `espflash/` | ~2000+ | Embedded/forked espflash for ESP32 flashing |

This is the brain. Every protocol operation — TLV framing, sync word scanning, request/response matching, tunneling (UI and NET through MGMT), bootloader protocols — lives here. Both frontends are generic over `P: CtlPort`.

### ctl — the CLI frontend

| File | Lines | Role |
|---|---|---|
| `main.rs` | 577 | CLI/REPL (clap + reedline), auto-discovery, connection management |
| `serial.rs` | 179 | `TokioSerialPort` — tokio-serial `CtlPort` impl with buffering + timeout |
| `handlers/mgmt.rs` | 427 | MGMT commands: ping, info, flash, baud rate, speed test, stack |
| `handlers/ui.rs` | 303 | UI commands: ping, info, flash, version, sframe key, loopback, monitor, stack |
| `handlers/net.rs` | 398 | NET commands: ping, info, flash, wifi, relay URL, loopback, chat, monitor, channels, jitter |

### web-ctl — the WASM frontend

| File | Lines | Role |
|---|---|---|
| `lib.rs` | 850 | `LinkController` — wasm_bindgen methods exposing all operations to JS |
| `serial.rs` | 633 | `WebSerial` + `WebSerialAdapter` — WebSerial API `CtlPort` impl |

## How thin are the wrappers?

**Overall: quite good.** Both frontends are genuinely wrappers — they call `CtlCore` methods and adapt the results for their platform. There's no protocol logic or TLV parsing in either frontend. The main responsibilities of each frontend are appropriate:

**ctl-appropriate concerns** (not shared, shouldn't be):

- CLI arg parsing, REPL loop
- `indicatif` progress bars
- `crossterm` raw mode for monitor commands
- Auto-discovery (scan ports, hello probe)
- Manual port selection (stdin prompts)
- Speed test (CLI-only diagnostic)

**web-ctl-appropriate concerns** (not shared, shouldn't be):

- `wasm_bindgen` interface + `JsValue` conversions
- `js_sys::Function` progress callbacks
- `get_all_state()` aggregation (web-specific)

## Issues

### 1. Sync delay forces web-ctl to decompose shared operations

`flash.rs` has compound methods like `get_ui_bootloader_info(delay)` and `flash_ui(firmware, delay, ...)` that take a **sync** delay `Fn(u64)`. This works for ctl (`std::thread::sleep`) but not for web-ctl, which needs **async** `js_sleep`.

So web-ctl has to manually replicate the orchestration:

**ctl** (one call):
```rust
let info = core.get_ui_bootloader_info(delay).await?;
```

**web-ctl** (manual decomposition):
```rust
core.reset_ui_to_bootloader().await;
js_sleep(1000).await;
let info = core.query_ui_bootloader().await;
core.reset_ui_to_user().await;
```

Same pattern for `flash_ui` — ctl calls the compound method, web-ctl does reset + sleep + `flash_ui_in_bootloader_mode` + reset. The shared code already uses the async `Fn(u64) -> Future` pattern for `init_port` and `try_enter_mgmt_bootloader`, so these methods could be migrated to the same pattern. That would let web-ctl use them as single calls too.

### 2. Behavioral divergence: hold NET during UI flash

`ctl/handlers/ui.rs` does:
```rust
core.hold_net_reset().await;   // hold NET in reset
// ... flash UI ...
core.reset_net_to_user().await; // release NET
```

`web-ctl/lib.rs` **does not do this**. This means web-ctl UI flashing may get interference from the NET chip. This should either be pushed into the shared `flash_ui` method, or duplicated in web-ctl.

### 3. Bootloader info formatting is duplicated (but mostly unavoidable)

Both frontends parse `MgmtBootloaderInfo` the same way — iterating commands, formatting chip names via `stm::chip_name()`, checking flash samples. ctl prints to stdout; web-ctl builds JS objects. The data fetching is shared; the presentation necessarily differs. Not a real problem, but if the info struct grows, both need updating.

### 4. Channel name mapping duplicated

`ctl/handlers/net.rs` maps channel IDs to names ("Ptt", "PttAi", "ChatAi") in 3 separate match blocks. This could be a method on `ChannelConfig` or a utility function in the shared crate.

### 5. Minor: stack info derived fields

Both frontends compute `stack_free` and `usage_percent` from `StackInfoResult`. Trivial math, but could be methods on the struct if it keeps appearing.

## Summary

The architecture is in good shape. `CtlCore` owns all the real logic; both frontends are thin. The biggest structural issue is **#1** — the sync delay signature in `flash.rs` prevents web-ctl from using compound operations, forcing manual decomposition that risks divergence (which manifests as **#2**). Migrating those methods to the same async `Fn(u64) -> Future` pattern already used by `init_port` and `try_enter_mgmt_bootloader` would eliminate both issues.

# Code Audit Report: Fixed-Length Delays, Magic Constants, and Deep Nesting

**Date**: February 12, 2026
**Scope**: ctl/, web-ctl/, mgmt/, ui/, net/, link/src/ctl/, link/src/mgmt/, link/src/shared/, link/src/ui/, link/src/net/

## Executive Summary

This audit analyzed the codebase for three code quality patterns that impact maintainability and reliability.

### Findings Summary

| Pattern | Must Fix | Should Fix | Optional | Total |
|---------|----------|------------|----------|-------|
| Fixed-Length Delays | 8 | 12 | 3 | 23 |
| Magic Constants | 15 | 18 | 12 | 45 |
| Deep Nesting | 5 patterns (137 instances) | 9 patterns (73 instances) | Remaining | **809** |

**Overall Assessment**: The codebase shows good use of centralized constants for UART configuration in `link/src/shared/uart_config.rs`. However, there are numerous hardcoded timing values and magic numbers scattered throughout that should be consolidated. Deep nesting (three-level `::` paths) appears 809 times with the most common patterns being `js_sys::Reflect::set` (45), `tokio::time::sleep` (40), and `std::io::Error` (34). Per Rust conventions, all instances should use `use` declarations except when disambiguating same-named items from different modules.

---

## 1. Fixed-Length Delays

### MUST FIX (8 issues)

#### 1.1 MGMT bootloader timing - No verification after reset
**File**: `link/src/ctl/flash.rs:729-737`

**Issue**:
```rust
// Establish known starting state (both signals low)
let _ = self.port_mut().write_dtr(false).await;
let _ = self.port_mut().write_rts(false).await;
delay_ms(100).await;

// BOOT0 high, then pulse reset (RTS=true, DTR high→low)
let _ = self.port_mut().write_rts(true).await;
delay_ms(50).await;
let _ = self.port_mut().write_dtr(true).await;
delay_ms(50).await;
let _ = self.port_mut().write_dtr(false).await;
delay_ms(500).await;  // <-- Fixed 500ms wait
```

**Problem**: Uses fixed 500ms delay waiting for bootloader without verification. If bootloader starts faster, this wastes time. If it needs more time, operation fails.

**Proposed Fix**:
```rust
const BOOTLOADER_TIMEOUT_MS: u64 = 1000;
const BOOTLOADER_POLL_MS: u64 = 50;

// After reset sequence:
let start = current_time_ms();
loop {
    delay_ms(BOOTLOADER_POLL_MS).await;
    if self.probe_mgmt_bootloader().await {
        break;
    }
    if elapsed_time_ms(start) > BOOTLOADER_TIMEOUT_MS {
        return MgmtBootloaderEntry::NotDetected;
    }
}
```

**Priority**: Critical path for MGMT flashing reliability

---

#### 1.2 UI bootloader entry delay
**File**: `link/src/ctl/flash.rs:991`

**Issue**:
```rust
// Wait for bootloader to be ready
delay_ms(1000).await;
```

**Problem**: Unconditional 1-second delay. No verification that bootloader is actually ready.

**Proposed Fix**:
```rust
const UI_BOOTLOADER_TIMEOUT_MS: u64 = 1500;
const UI_BOOTLOADER_POLL_MS: u64 = 100;

// Reset UI chip into bootloader mode
let _ = self.reset_ui_to_bootloader().await;

// Poll for bootloader ready
let start = current_time_ms();
loop {
    delay_ms(UI_BOOTLOADER_POLL_MS).await;
    if self.probe_ui_bootloader().await {
        break;
    }
    if elapsed_time_ms(start) > UI_BOOTLOADER_TIMEOUT_MS {
        return Err(stm::Error::Timeout);
    }
}
```

**Priority**: UI flashing reliability

---

#### 1.3 UI flash bootloader delay inconsistency
**File**: `link/src/ctl/flash.rs:1078`

**Issue**:
```rust
// Reset UI chip into bootloader mode
let _ = self.reset_ui_to_bootloader().await;

// Wait for bootloader to be ready
delay_ms(100).await;  // <-- Different from 1000ms above
```

**Problem**: Different delay value (100ms vs 1000ms) for same operation. No clear rationale.

**Proposed Fix**: Use the polling approach from 1.2 with the same constants.

**Priority**: Inconsistency indicates unclear requirements

---

#### 1.4 MGMT firmware boot wait
**File**: `link/src/ctl/flash.rs:957-960`

**Issue**:
```rust
delay_ms(200).await; // Initial wait for boot to start

// Wait for MGMT firmware to come online and be ready for commands
let _ = self.wait_for_mgmt_ready(50).await;
```

**Problem**: Fixed 200ms delay before active polling. This is better than pure delay, but the initial wait should be minimal.

**Proposed Fix**:
```rust
// Start polling immediately - wait_for_mgmt_ready handles timing
let _ = self.wait_for_mgmt_ready(50).await;
```

**Priority**: Remove unnecessary delay, let polling handle timing

---

#### 1.5 NET reset timing
**File**: `link/src/ctl/core.rs:1145, 1151, 1166`

**Issue**:
```rust
pub async fn reset_net_to_bootloader<D, F>(&mut self, delay_ms: D) -> Result<(), CtlError>
{
    // First power cycle (clean slate)
    self.set_net_rst(PinValue::Low).await?;
    delay_ms(10).await;  // <-- Fixed delay
    self.set_net_rst(PinValue::High).await?;
    // Set BOOT low for bootloader mode
    self.set_net_boot(PinValue::Low).await?;
    // Second power cycle - ESP32 samples BOOT when RST goes high
    self.set_net_rst(PinValue::Low).await?;
    delay_ms(10).await;  // <-- Fixed delay
    self.set_net_rst(PinValue::High).await
}
```

**Problem**: Uses hardcoded 10ms delays for ESP32 reset timing. ESP32 datasheet specifies minimum reset pulse width and boot sampling requirements.

**Proposed Fix**:
```rust
// In a constants module:
pub const ESP32_RESET_PULSE_MS: u64 = 10;  // Per ESP32-S3 datasheet
pub const ESP32_BOOT_SAMPLE_MS: u64 = 10;  // Time for BOOT pin to be sampled

// Then use named constants:
delay_ms(ESP32_RESET_PULSE_MS).await;
```

**Priority**: Hardware timing requirements should be documented with named constants

---

#### 1.6 MGMT baud rate change delay
**File**: `link/src/ctl/flash.rs:539`

**Issue**:
```rust
// Set CTL baud rate (ACK comes at old rate, then MGMT switches)
self.send_mgmt_command(CtlToMgmt::SetCtlBaudRate, &baud_bytes).await?;

// Small delay for MGMT to complete the baud rate switch
self.delay.delay_ms(10).await;
```

**Problem**: Arbitrary 10ms delay for baud rate change to complete. No verification.

**Proposed Fix**:
```rust
const BAUD_SWITCH_DELAY_MS: u32 = 10;  // MGMT firmware baud switch time
self.delay.delay_ms(BAUD_SWITCH_DELAY_MS).await;

// Better: Add a verification byte exchange at new rate
// Send dummy command and wait for response to verify new rate
```

**Priority**: Critical for serial communication reliability

---

#### 1.7 ESP32 reset strategy delays
**File**: `link/src/ctl/espflash/connection/reset.rs:87`

**Issue**:
```rust
serial_port.delay_ms(100).await;

set_rts(serial_port, false).await?; // EN = HIGH, chip out of reset
set_dtr(serial_port, true).await?; // IO0 = LOW

serial_port.delay_ms(*delay_ms as u32).await;
```

**Problem**: Hardcoded 100ms delay in reset sequence. This is in espflash vendored code, but still affects reliability.

**Proposed Fix**: Use named constants from reset.rs module (DEFAULT_RESET_DELAY, EXTRA_RESET_DELAY)

**Priority**: Reset reliability

---

#### 1.8 UI probe retry delay
**File**: `ctl/src/main.rs:441`

**Issue**:
```rust
for _attempt in 1..=max_attempts {
    // Set short timeout for probing
    let _ = core.port_mut().set_timeout(Duration::from_millis(100));

    if core.ui_ping(b"probe").await.is_ok() {
        // Restore normal timeout
        let _ = core.port_mut().set_timeout(Duration::from_secs(3));
        return Some((core, port_name.clone()));
    }

    // Wait a bit before retry
    tokio::time::sleep(Duration::from_millis(50)).await;
}
```

**Problem**: Fixed 50ms delay between retries. This is reasonable, but should be a named constant.

**Proposed Fix**:
```rust
const UI_PROBE_RETRY_DELAY_MS: u64 = 50;
tokio::time::sleep(Duration::from_millis(UI_PROBE_RETRY_DELAY_MS)).await;
```

**Priority**: Makes retry behavior configurable

---

### SHOULD FIX (12 issues)

#### 2.1 Monitor timeout values
**Files**:
- `ctl/src/handlers/net.rs:310`
- `ctl/src/handlers/ui.rs:254`

**Issue**:
```rust
// Set a short timeout for non-blocking reads
if let Err(e) = core.port_mut().set_timeout(std::time::Duration::from_millis(100)) {
    eprintln!("Warning: couldn't set timeout: {}", e);
}
```

**Problem**: Hardcoded 100ms timeout for monitor mode. Should be named constant.

**Proposed Fix**:
```rust
const MONITOR_READ_TIMEOUT_MS: u64 = 100;
core.port_mut().set_timeout(Duration::from_millis(MONITOR_READ_TIMEOUT_MS))
```

---

#### 2.2 MGMT bootloader probe timeout
**File**: `ctl/src/handlers/mgmt.rs:34, 147`

**Issue**:
```rust
// Set short timeout for probing
let _ = core.port_mut().set_timeout(Duration::from_millis(200));
```

**Problem**: Hardcoded 200ms timeout. Should match the pattern used elsewhere or be documented why different.

**Proposed Fix**:
```rust
const MGMT_BOOTLOADER_PROBE_TIMEOUT_MS: u64 = 200;
let _ = core.port_mut().set_timeout(Duration::from_millis(MGMT_BOOTLOADER_PROBE_TIMEOUT_MS));
```

---

#### 2.3 Hello handshake timeout
**File**: `ctl/src/main.rs:394`

**Issue**:
```rust
// Set short timeout for hello check
core.port_mut().set_timeout(Duration::from_millis(500)).ok()?;
```

**Problem**: Different from the hello timeout used in `wait_for_mgmt_ready` (100ms).

**Proposed Fix**:
```rust
const HELLO_TIMEOUT_MS: u64 = 100;  // Move to shared location
core.port_mut().set_timeout(Duration::from_millis(HELLO_TIMEOUT_MS)).ok()?;
```

---

#### 2.4-2.12 Additional timing constants

Multiple instances in handlers where timeout values should be consolidated into a shared constants module.

**Proposed Solution**: Create `ctl/src/constants.rs`:
```rust
pub mod timeouts {
    use std::time::Duration;

    pub const MONITOR_READ_MS: u64 = 100;
    pub const BOOTLOADER_PROBE_MS: u64 = 200;
    pub const HELLO_HANDSHAKE_MS: u64 = 500;
    pub const NORMAL_OPERATION_MS: u64 = 3000;

    pub fn monitor_read() -> Duration {
        Duration::from_millis(MONITOR_READ_MS)
    }

    pub fn normal_operation() -> Duration {
        Duration::from_millis(NORMAL_OPERATION_MS)
    }
}
```

---

### OPTIONAL (3 issues)

#### 3.1 Test mock delays
**File**: `link/src/shared/mocks.rs:430, 486`

**Issue**:
```rust
// Simulate real audio timing (80ms per frame at 8kHz stereo with A-law)
// Use shorter delay in tests to speed them up while still allowing scheduler to run
tokio::time::sleep(std::time::Duration::from_millis(10)).await;
```

**Assessment**: These are test mocks and the comments explain the values. Acceptable as-is, but could be constants if tests need tuning.

---

## 2. Magic Constants

### MUST FIX (15 issues)

#### 4.1 Baud rate constants scattered throughout
**Issue**: Multiple files use `115200` and `1000000` as literals

**Good news**: Centralized config EXISTS at `link/src/shared/uart_config.rs`, but it's not consistently used.

**Locations using literals**:
- `handlers/mgmt.rs:27, 130`
- `flash.rs:722, 947, 1065, 1092`
- `main.rs:34` (CLI default)

**Proposed Fix**:
```rust
use link::uart_config::{STM32_BOOTLOADER, HIGH_SPEED};

// Replace:
core.port_mut().get_mut().set_baud_rate(115200)?;
// With:
core.port_mut().get_mut().set_baud_rate(STM32_BOOTLOADER.baudrate)?;

// Replace:
core.port_mut().get_mut().set_baud_rate(1000000)?;
// With:
core.port_mut().get_mut().set_baud_rate(HIGH_SPEED.baudrate)?;
```

**Priority**: Centralized constants exist but are not used consistently

---

#### 4.2 CLI default baud rate
**File**: `ctl/src/main.rs:34`

**Issue**:
```rust
#[arg(short, long, default_value = "1000000")]
baud: u32,
```

**Proposed Fix**:
```rust
use link::uart_config;

#[arg(short, long, default_value_t = uart_config::HIGH_SPEED.baudrate)]
baud: u32,
```

---

#### 4.3 Timeout values
**Issue**: Timeout values (`3000`, `500`, `200`, `100` milliseconds) are magic numbers scattered throughout.

**Proposed Fix**: Create timeout constants module:
```rust
// In link/src/shared/timeouts.rs or ctl/src/constants.rs
pub const NORMAL_OPERATION_TIMEOUT_MS: u64 = 3000;
pub const HELLO_TIMEOUT_MS: u64 = 500;
pub const BOOTLOADER_PROBE_TIMEOUT_MS: u64 = 200;
pub const MONITOR_POLL_TIMEOUT_MS: u64 = 100;
```

---

#### 4.4 Flash sector sizes
**File**: `link/src/ctl/flash.rs:66-79`

**Issue**:
```rust
const SECTOR_SIZES: [usize; 12] = [
    16 * 1024,  // Sector 0
    16 * 1024,  // Sector 1
    // ...
    128 * 1024, // Sector 10
    128 * 1024, // Sector 11
];
```

**Problem**: Mixed magic numbers in array. The KB multiplier pattern is inconsistent.

**Proposed Fix**:
```rust
const KB: usize = 1024;
const SECTOR_SIZES: [usize; 12] = [
    16 * KB,   // Sectors 0-3: 16 KB each
    16 * KB,
    16 * KB,
    16 * KB,
    64 * KB,   // Sector 4: 64 KB
    128 * KB,  // Sectors 5-11: 128 KB each
    128 * KB,
    128 * KB,
    128 * KB,
    128 * KB,
    128 * KB,
    128 * KB,
    128 * KB,
];
```

---

#### 4.5 STM32 chip-specific constants
**File**: `link/src/ctl/flash.rs:888`

**Issue**:
```rust
// Erase pages needed for firmware (STM32F072CB has 2KB pages)
const PAGE_SIZE: usize = 2048;
```

**Proposed Fix**: Create chip-specific module:
```rust
pub mod stm32f072 {
    pub const PAGE_SIZE: usize = 2 * 1024;
    pub const FLASH_BASE: u32 = 0x0800_0000;
    pub const SRAM_BASE: u32 = 0x2000_0000;
    pub const SRAM_END: u32 = 0x2002_0000;
}
```

---

#### 4.6 Write chunk sizes
**Files**: `flash.rs:906, 917, 1136, 1146`

**Issue**:
```rust
for chunk in firmware.chunks(256) {
```

**Proposed Fix**:
```rust
const STM32_WRITE_CHUNK_SIZE: usize = 256;
for chunk in firmware.chunks(STM32_WRITE_CHUNK_SIZE) {
```

---

#### 4.7 Buffer sizes with unexplained padding
**File**: `link/src/ctl/flash.rs:257`

**Issue**:
```rust
const RAW_BUFFER_SIZE: usize = SYNC_WORD.len() + TLV_HEADER_SIZE + MAX_VALUE_SIZE + 256;
```

**Problem**: The `+ 256` is unexplained.

**Proposed Fix**:
```rust
const TLV_PADDING_BYTES: usize = 256;  // Extra space for partial TLVs during parsing
const RAW_BUFFER_SIZE: usize = SYNC_WORD.len() + TLV_HEADER_SIZE + MAX_VALUE_SIZE + TLV_PADDING_BYTES;
```

---

#### 4.8 Retry attempt counts
**Locations**:
- `main.rs:421, 510, 530` - `wait_for_mgmt_ready(50)`
- `main.rs:429` - `max_attempts = 20`
- `core.rs:449` - `MAX_TLVS: usize = 1024`

**Proposed Fix**:
```rust
pub mod retries {
    pub const MGMT_READY_ATTEMPTS: usize = 50;
    pub const UI_PROBE_ATTEMPTS: usize = 20;
    pub const HELLO_MAX_TLVS: usize = 1024;
}
```

---

#### 4.9 STM32 memory addresses
**Files**: Multiple uses of `0x0800_0000` (STM32 flash base)

**Issue**:
```rust
bl.go(0x0800_0000).await?;
```

**Proposed Fix**:
```rust
pub mod stm32 {
    pub const FLASH_BASE: u32 = 0x0800_0000;
    pub const SRAM_BASE: u32 = 0x2000_0000;
    pub const SRAM_END: u32 = 0x2002_0000;
}

bl.go(stm32::FLASH_BASE).await?;
```

---

#### 4.10 Channel IDs
**File**: `ctl/src/handlers/net.rs:378`

**Issue**:
```rust
let channel_ids = [0u8, 1, 3]; // Ptt, PttAi, ChatAi
```

**Problem**: Magic numbers with comment explaining meaning.

**Proposed Fix**:
```rust
pub mod channels {
    pub const PTT: u8 = 0;
    pub const PTT_AI: u8 = 1;
    pub const CHAT_AI: u8 = 3;
}

let channel_ids = [channels::PTT, channels::PTT_AI, channels::CHAT_AI];
```

---

#### 4.11 ESP32 partition address
**File**: `ctl/src/handlers/net.rs:179`

**Issue**:
```rust
println!("Partition table: default (single app at 0x10000)");
```

**Proposed Fix**:
```rust
const DEFAULT_APP_ADDRESS: u32 = 0x10000;
println!("Partition table: default (single app at {:#x})", DEFAULT_APP_ADDRESS);
```

---

#### 4.12 Espflash initial baud rate
**File**: `link/src/ctl/flash.rs:1383, 1455`

**Issue**:
```rust
let serial_interface = TunnelSerialInterface::new(port, 115_200, delay);
```

**Proposed Fix**:
```rust
use crate::shared::uart_config::STM32_BOOTLOADER;
let serial_interface = TunnelSerialInterface::new(port, STM32_BOOTLOADER.baudrate, delay);
```

---

#### 4.13 ESP32 partition size limit
**File**: `link/src/ctl/espflash/image_format/idf.rs:37`

**Issue**:
```rust
const MAX_PARTITION_SIZE: u32 = 16 * 1000 * 1024;
```

**Problem**: Uses `1000` instead of `1024` for KB multiplier. Unclear if intentional (decimal MB vs binary MiB).

**Proposed Fix**:
```rust
const MB_DECIMAL: u32 = 1000 * 1024;
const MAX_PARTITION_SIZE: u32 = 16 * MB_DECIMAL;  // 16 MB (decimal)
// Or if it's actually binary:
const MAX_PARTITION_SIZE: u32 = 16 * 1024 * 1024;  // 16 MiB
```

---

#### 4.14 Port initialization delay
**File**: `link/src/ctl/core.rs:438`

**Issue**:
```rust
delay_ms(100).await;
```

**Proposed Fix**:
```rust
const PORT_INIT_STABILIZATION_MS: u64 = 100;
delay_ms(PORT_INIT_STABILIZATION_MS).await;
```

---

#### 4.15 Protocol-specific values
**Various locations**: SFrame key size (16 bytes), stack paint pattern (0xAA), etc.

**Proposed Fix**: Create protocol constants module for these values.

---

### SHOULD FIX (18 issues)

Documented uses of numbers that should ideally be constants but are less critical:
- Chip ID masks and ranges
- Version format calculations
- USB PID/VID values
- Test timing values with explanatory comments
- Buffer index calculations
- Loop iteration counts with context

**General approach**: Extract to module-level constants with descriptive names.

---

### OPTIONAL (12 issues)

Well-commented magic numbers in test code and algorithms where the number is already explained in comments. These are acceptable as-is but could be constants if needed for tuning.

---

## 3. Deep Nesting

**Total instances found: 809** across all analyzed files.

**Rust Convention**: Use `use` declarations for imports. Only use fully-qualified paths when disambiguating items with the same name from different modules. All instances below represent opportunities for cleaner code through proper imports.

---

### MUST FIX - High-frequency patterns (>20 occurrences, 137 total instances)

These patterns appear so frequently that adding `use` declarations would significantly improve readability.

#### 3.1 `js_sys::Reflect::set` - 45 occurrences
**Files**: Primarily `web-ctl/src/lib.rs` (WASM bindings)

**Current pattern**:
```rust
js_sys::Reflect::set(&obj, &JsValue::from_str("key"), &value)?;
```

**Proposed fix**:
```rust
use js_sys::Reflect;

Reflect::set(&obj, &JsValue::from_str("key"), &value)?;
```

---

#### 3.2 `tokio::time::sleep` - 40 occurrences
**Files**: `link/src/ctl/espflash/target/mod.rs`, `link/src/shared/mocks.rs`, `link/src/ctl/flash.rs`, handlers

**Current pattern**:
```rust
tokio::time::sleep(std::time::Duration::from_millis(100)).await;
```

**Proposed fix**:
```rust
use tokio::time::sleep;
use std::time::Duration;

sleep(Duration::from_millis(100)).await;
```

**Priority**: MUST FIX - Appears in critical timing code

---

#### 3.3 `std::io::Error` - 34 occurrences
**Files**: `link/src/ctl/flash.rs` (multiple), `link/src/ctl/core.rs`, error handling throughout

**Current pattern**:
```rust
impl<P: CtlPort<Error = std::io::Error>> CtlPort for TunnelPort<'_, P>

return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message"));
```

**Proposed fix**:
```rust
use std::io::{Error as IoError, ErrorKind};

impl<P: CtlPort<Error = IoError>> CtlPort for TunnelPort<'_, P>

return Err(IoError::new(ErrorKind::InvalidData, "message"));
```

**Priority**: MUST FIX - Appears in trait bounds and error handling

---

#### 3.4 `tokio::sync::mpsc` - 28 occurrences
**Files**: `link/src/shared/mocks.rs`, async channel usage

**Current pattern**:
```rust
let (tx, rx) = tokio::sync::mpsc::channel::<T>(10);
```

**Proposed fix**:
```rust
use tokio::sync::mpsc;

let (tx, rx) = mpsc::channel::<T>(10);
```

---

#### 3.5 `std::time::Duration` - 24 occurrences
**Files**: Throughout codebase for timeouts

**Current pattern**:
```rust
core.port_mut().set_timeout(std::time::Duration::from_millis(100))?;
```

**Proposed fix**:
```rust
use std::time::Duration;

core.port_mut().set_timeout(Duration::from_millis(100))?;
```

**Note**: This overlaps with the magic constants issue - timeout values should also be named constants.

---

### SHOULD FIX - Medium-frequency patterns (10-19 occurrences, 73 total instances)

#### 3.6 `super::connection::SerialInterface` - 17 occurrences
**File**: `link/src/ctl/espflash/target/mod.rs`

**Issue**: Repeated super:: references to sibling module

**Proposed fix**:
```rust
use super::connection::SerialInterface;
```

---

#### 3.7 `std::sync::Arc` - 16 occurrences
**Files**: Multiple files with shared state

**Proposed fix**:
```rust
use std::sync::Arc;
```

---

#### 3.8 `heapless::Vec::new` - 12 occurrences
**Files**: Embedded code (UI, MGMT, NET)

**Proposed fix**:
```rust
use heapless::Vec;
// Then use: Vec::new() or Vec::<T, N>::new()
```

---

#### 3.9 `core::str::from_utf8` - 12 occurrences
**Files**: no_std embedded code

**Proposed fix**:
```rust
use core::str::from_utf8;
```

---

#### 3.10 `std::vec::Vec` - 11 occurrences
**Proposed fix**: `use std::vec::Vec;` (though `Vec` is in prelude, explicit paths here)

---

#### 3.11 `crate::shared::PinValue` - 11 occurrences
**Files**: Handler code

**Proposed fix**:
```rust
use crate::shared::PinValue;
// Or for handlers in ctl/:
use link::PinValue;
```

**Priority**: High - Internal project type, no reason for full path

---

#### 3.12 `std::sync::atomic` - 10 occurrences
**Proposed fix**: `use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};`

---

#### 3.13 `std::error::Error` - 10 occurrences
**Proposed fix**: `use std::error::Error;` (especially for trait bounds)

---

#### 3.14 `embedded_hal::i2c::I2c` - 10 occurrences
**File**: `link/src/ui/mod.rs`

**Proposed fix**: `use embedded_hal::i2c::I2c;`

---

### Files Requiring Most Cleanup

#### Priority 1: Core Library Files

**1. `link/src/ctl/espflash/target/mod.rs` - 87 instances**
- Add `use` declarations for: `tokio::time`, `std::io`, `js_sys::Reflect`, `super::connection`
- **Impact**: This is vendored espflash code, changes might conflict with upstream

**2. `link/src/shared/mocks.rs` - 82 instances**
- Add `use` declarations for: `tokio::time`, `tokio::sync::mpsc`, `std::sync::Arc`
- **Impact**: Test mocks, high leverage for cleanup

**3. `link/src/ctl/flash.rs` - 63 instances**
- Add `use` declarations for: `std::io::{Error, ErrorKind}`, `tokio::time::sleep`
- **Impact**: Critical flashing code, readability matters

**4. `link/src/ui/mod.rs` - 61 instances**
- Add `use` declarations for: `embedded_hal::*`, `core::*`
- **Impact**: Embedded firmware, lots of HAL usage

#### Priority 2: Frontend Files

**5. `web-ctl/src/lib.rs` - 70 instances**
- Add `use` declarations for: `js_sys::Reflect`, `wasm_bindgen::*`
- **Impact**: WASM bindings, lots of JS interop

**6. `web-ctl/src/serial.rs` - 44 instances**
- Similar WASM cleanup needed

#### Priority 3: Application Files

**7. `net/src/main.rs` - 39 instances**
- Add `use` declarations for: `esp_idf_svc::{sys, wifi}`, `std::*`

**8. `ctl/src/main.rs` - 33 instances**
- Add `use` declarations for: `std::time::Duration`, `tokio::time::sleep`

**9. `ctl/src/handlers/net.rs` - 25 instances**
**10. `ctl/src/handlers/ui.rs` - 20 instances**
**11. `ctl/src/handlers/mgmt.rs` - 19 instances**
- Add `use` declarations for: `link::{Pin, PinValue}`, `link::ctl::{CtlError, flash}`

---

### Summary of Recommended Actions

1. **Create import cleanup tasks by file** - Top 15 files contain 623 of the 809 instances (77%)

2. **Prioritize by pattern frequency**:
   - `tokio::time::sleep` (40) → `use tokio::time::sleep;`
   - `std::io::Error` (34) → `use std::io::{Error as IoError, ErrorKind};`
   - `std::time::Duration` (24) → `use std::time::Duration;`

3. **Special attention to internal paths**:
   - `crate::shared::PinValue` (11) - No reason for full path
   - `super::super::*` (9) - Indicates module organization issues

4. **Note on espflash vendored code**:
   - `link/src/ctl/espflash/` has 142 instances across multiple files
   - Consider whether to modify vendored code or wait for upstream

5. **Disambiguation exceptions**:
   - When same name exists from different modules (rare in this codebase)
   - When full path aids understanding (also rare - usually imports are clearer)

**Total estimated cleanup**: ~809 instances across ~30 files
**Highest impact**: Top 15 files (623 instances)

---

## Implementation Recommendations

### Phase 1: Immediate (Critical Reliability)

1. **Create shared constants modules**:
   - `link/src/shared/timeouts.rs` - All timeout values
   - `link/src/shared/chip_config.rs` - Chip-specific constants (STM32, ESP32)
   - `ctl/src/constants.rs` - CTL-specific constants (retry counts, buffer sizes)

2. **Replace critical fixed delays with polling**:
   - MGMT bootloader entry (flash.rs:737)
   - UI bootloader entry (flash.rs:991, 1078)
   - Remove redundant initial delay before MGMT ready wait (flash.rs:957)

3. **Use existing uart_config consistently**:
   - Replace all baud rate literals with `uart_config::STM32_BOOTLOADER.baudrate` and `uart_config::HIGH_SPEED.baudrate`
   - Update CLI default value to reference the constant

4. **Document hardware timing**:
   - Create named constants for all ESP32 reset timings
   - Create named constants for STM32 reset timings
   - Add comments referencing datasheet sections

### Phase 2: Short-term (Code Quality)

1. **Consolidate timeout values** into timeouts module
2. **Create channel ID constants** for protocol
3. **Simplify use declarations** to reduce repetition
4. **Extract buffer size calculations** with explanatory comments
5. **Create memory address constants** for STM32 regions

### Phase 3: Long-term (Architecture)

1. **Configuration system** for tunable parameters (retry counts, timeouts)
2. **Compile-time validation** that timing values meet hardware requirements
3. **Flashing abstraction** that encapsulates chip-specific timing logic
4. **Hardware description module** that groups all chip-specific constants

---

## Priority Files for Implementation

Based on this audit, implement changes in this order:

1. **link/src/shared/timeouts.rs** (NEW) - Centralized timing constants
2. **link/src/shared/chip_config.rs** (NEW) - Hardware-specific constants
3. **link/src/ctl/flash.rs** - Core flashing logic (15+ timing issues, most critical)
4. **link/src/ctl/core.rs** - NET reset timing, hello handshake
5. **ctl/src/main.rs** - Connection logic, CLI defaults
6. **ctl/src/handlers/*.rs** - Replace remaining magic constants
7. **link/src/shared/uart_config.rs** - Already good, needs wider adoption

---

## Success Metrics

After implementing these changes:
- All baud rates reference centralized constants
- All timeouts are named constants with clear purposes
- Hardware timing requirements are documented with datasheet references
- Critical delays use active polling instead of fixed waits
- No unexplained numeric literals in flash/bootloader code
- Consistent retry and timeout behavior across operations

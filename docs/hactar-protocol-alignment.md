# Hactar Protocol Alignment Guide

This document specifies the changes required in the hactar firmware to align with Link's TLV protocol, enabling full compatibility with the Link CTL tool and MGMT firmware.

## Overview

Link uses a TLV (Type-Length-Value) protocol with:
- **Sync word**: `0x4C494E4B` ("LINK") in little-endian
- **Type**: 2-byte little-endian message type
- **Length**: 2-byte little-endian payload length
- **Value**: Variable-length payload

The key alignment requirement is that hactar's message type values must match Link's enum values exactly.

---

## UI Chip Protocol (MGMT <-> UI)

### Commands (MGMT to UI): `CtlToUi`

| Command | Link Value | Hactar Current | Payload |
|---------|------------|----------------|---------|
| Ping | `0x0020` | 0 | None |
| CircularPing | `0x0021` | 1 | None |
| GetVersion | `0x0022` | 2 | None |
| SetVersion | `0x0023` | 3 | 32-byte version string |
| GetSFrameKey | `0x0024` | 4 | None |
| SetSFrameKey | `0x0025` | 5 | 16-byte key |
| SetLoopback | `0x0026` | 6 | 1 byte: mode (0=Off, 1=Raw, 2=Alaw, 3=Sframe) |
| GetLoopback | `0x0027` | 7 | None |
| GetStackInfo | `0x0028` | 8 | None |
| RepaintStack | `0x0029` | 9 | None |
| GetLogsEnabled | `0x002A` | 10 | None |
| SetLogsEnabled | `0x002B` | 11 | 1 byte: 0=disabled, 1=enabled |
| ClearStorage | `0x002C` | 12 | None |

### Responses (UI to MGMT): `UiToCtl`

| Response | Link Value | Hactar Current | Payload |
|----------|------------|----------------|---------|
| Pong | `0x0030` | 0x8000 | None |
| CircularPing | `0x0031` | 0x8001 | None |
| Version | `0x0032` | 0x8002 | 32-byte version string |
| SFrameKey | `0x0033` | 0x8003 | 16-byte key |
| Ack | `0x0034` | 0x8004 | None |
| Error | `0x0035` | 0x8005 | Error string (UTF-8) |
| Loopback | `0x0036` | 0x8006 | 1 byte: mode |
| Log | `0x0037` | 0x8007 | UTF-8 log message |
| StackInfo | `0x0038` | 0x8008 | postcard-serialized StackInfo |
| LogsEnabled | `0x0039` | 0x8009 | 1 byte: 0=disabled, 1=enabled |

---

## NET Chip Protocol (MGMT <-> NET)

### Commands (MGMT to NET): `CtlToNet`

| Command | Link Value | Hactar Current | Payload |
|---------|------------|----------------|---------|
| Ping | `0x0040` | 0 | None |
| CircularPing | `0x0041` | 1 | None |
| AddWifiSsid | `0x0042` | 2 | postcard: `(ssid: String, pass: String)` |
| GetWifiSsids | `0x0043` | 3 | None |
| ClearWifiSsids | `0x0044` | 4 | None |
| GetRelayUrl | `0x0045` | 5 | None |
| SetRelayUrl | `0x0046` | 6 | UTF-8 URL string |
| SetLoopback | `0x0047` | 7 | 1 byte: mode (0=Off, 1=Raw, 2=Moq) |
| GetLoopback | `0x0048` | 8 | None |
| GetLogsEnabled | `0x0049` | 9 | None |
| SetLogsEnabled | `0x004A` | 10 | 1 byte: 0=disabled, 1=enabled |
| ClearStorage | `0x004B` | 11 | None |
| GetLanguage | `0x004C` | 12 | None |
| SetLanguage | `0x004D` | 13 | UTF-8 language code (e.g., "en-US") |
| GetChannel | `0x004E` | 14 | None |
| SetChannel | `0x004F` | 15 | JSON array: `["relay","org","channel","ptt"]` |
| GetAi | `0x0050` | 16 | None |
| SetAi | `0x0051` | 17 | JSON object: `{"query":[...],"audio":[...],"cmd":[...]}` |
| BurnJtagEfuse | `0x0052` | - | None |

### Responses (NET to MGMT): `NetToCtl`

| Response | Link Value | Hactar Current | Payload |
|----------|------------|----------------|---------|
| Pong | `0x0050` | 0x8000 | None |
| CircularPing | `0x0051` | 0x8001 | None |
| WifiSsids | `0x0052` | 0x8002 | postcard: `Vec<(String, String)>` |
| RelayUrl | `0x0053` | 0x8003 | UTF-8 URL string |
| Ack | `0x0054` | 0x8004 | None |
| Error | `0x0055` | 0x8005 | Error string (UTF-8) |
| Loopback | `0x0056` | 0x8006 | 1 byte: mode |
| LogsEnabled | `0x0057` | 0x8007 | 1 byte: 0=disabled, 1=enabled |
| Language | `0x0058` | 0x8008 | UTF-8 language code |
| Channel | `0x0059` | 0x8009 | JSON array |
| Ai | `0x005A` | 0x800A | JSON object |

---

## UI <-> NET Protocol (Direct Audio Link)

### UI to NET: `UiToNet`

| Message | Link Value | Payload |
|---------|------------|---------|
| CircularPing | `0x0060` | None |
| AudioFrame | `0x0061` | `[channel_id: u8][sframe_header][encrypted_chunk][auth_tag]` |

### NET to UI: `NetToUi`

| Message | Link Value | Payload |
|---------|------------|---------|
| CircularPing | `0x0070` | None |
| AudioFrame | `0x0071` | Audio frame data |

---

## Data Format Changes

### WiFi Credentials

**Current (hactar)**: JSON format
```json
{"ssid": "MyNetwork", "pass": "password123"}
```

**Required (Link)**: postcard serialization of Rust tuple `(String, String)`

The postcard format is a compact binary serialization. For a tuple of two strings:
1. First string length as varint
2. First string UTF-8 bytes
3. Second string length as varint
4. Second string UTF-8 bytes

Example for `("MyNetwork", "password123")`:
```
09 4D 79 4E 65 74 77 6F 72 6B 0B 70 61 73 73 77 6F 72 64 31 32 33
```

### WiFi SSID List Response

**Current (hactar)**: JSON array
```json
[{"ssid": "Net1", "pass": "pass1"}, {"ssid": "Net2", "pass": "pass2"}]
```

**Required (Link)**: postcard serialization of `Vec<(String, String)>`

### StackInfo (UI chip only)

**Current (hactar)**: JSON format
```json
{"stack_base": 536870912, "stack_top": 536854528, "stack_size": 16384, "stack_used": 2048}
```

**Required (Link)**: postcard serialization of:
```rust
struct StackInfo {
    stack_base: u32,
    stack_top: u32,
    stack_size: u32,
    stack_used: u32,
}
```

Postcard encodes u32 as variable-length integers (varints). For typical stack addresses, expect 5 bytes per field.

---

## Implementation Checklist

### UI Chip (`ui_mgmt_link.h` / `ui_mgmt_link.cc`)

- [ ] Update `UiMgmtCmd` enum values to start at `0x0020`
- [ ] Update `UiMgmtResp` enum values to start at `0x0030`
- [ ] Convert StackInfo response from JSON to postcard format
- [ ] Verify loopback mode enum values match (0=Off, 1=Raw, 2=Alaw, 3=Sframe)

### NET Chip (`net_mgmt_link.h` / `net_mgmt_link.cc`)

- [ ] Update `NetMgmtCmd` enum values to start at `0x0040`
- [ ] Update `NetMgmtResp` enum values to start at `0x0050`
- [ ] Convert WiFi credential handling from JSON to postcard format
- [ ] Add `BurnJtagEfuse` command handler (value `0x0052`)
- [ ] Verify loopback mode enum values match (0=Off, 1=Raw, 2=Moq)

### UI <-> NET Link (`ui_net_link.h`)

- [ ] Update `UiToNet` enum values to start at `0x0060`
- [ ] Update `NetToUi` enum values to start at `0x0070`

---

## Byte Order

Link uses **little-endian** for all multi-byte values:
- Sync word: `4B 4E 49 4C` (bytes as they appear on wire for "LINK")
- Type field: LSB first (e.g., `0x0020` = `20 00`)
- Length field: LSB first

---

## Testing Integration

Once aligned, test with:

```bash
# From the link repo
cd ctl

# Basic connectivity
cargo run -- mgmt ping
cargo run -- ui ping
cargo run -- net ping

# UI commands
cargo run -- ui info
cargo run -- ui loopback get
cargo run -- ui loopback set raw

# NET commands
cargo run -- net wifi list
cargo run -- net wifi add "TestSSID" "TestPassword"
cargo run -- net relay get
cargo run -- net loopback get
```

All commands should return meaningful responses without timeouts or parse errors.

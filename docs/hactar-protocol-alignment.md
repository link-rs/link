# Hactar Protocol Alignment Guide

This document tracks the protocol alignment between Link CTL/MGMT and hactar UI/NET firmware.

## Status: ALIGNED

All protocol values and data formats are now aligned between Link and hactar.

---

## UI Chip Protocol (MGMT <-> UI)

### Commands (MGMT to UI): `CtlToUi` / `UiMgmtCmd`

| Command | Value | Payload |
|---------|-------|---------|
| Ping | `0x0020` | Arbitrary data |
| CircularPing | `0x0021` | Arbitrary data |
| GetVersion | `0x0022` | None |
| SetVersion | `0x0023` | 4 bytes (u32 LE) |
| GetSFrameKey | `0x0024` | None |
| SetSFrameKey | `0x0025` | 16 bytes |
| SetLoopback | `0x0026` | 1 byte: mode (0=Off, 1=Raw, 2=Alaw, 3=Sframe) |
| GetLoopback | `0x0027` | None |
| GetStackInfo | `0x0028` | None |
| RepaintStack | `0x0029` | None |
| GetLogsEnabled | `0x002A` | None |
| SetLogsEnabled | `0x002B` | 1 byte: 0=disabled, 1=enabled |
| ClearStorage | `0x002C` | None |

### Responses (UI to MGMT): `UiToCtl` / `UiMgmtResp`

| Response | Value | Payload |
|----------|-------|---------|
| Pong | `0x0030` | Echo data |
| CircularPing | `0x0031` | Echo data |
| Version | `0x0032` | 4 bytes (u32 LE) |
| SFrameKey | `0x0033` | 16 bytes |
| Ack | `0x0034` | None |
| Error | `0x0035` | UTF-8 error string |
| Loopback | `0x0036` | 1 byte: mode |
| Log | `0x0037` | UTF-8 log message |
| StackInfo | `0x0038` | JSON: `{"stack_base":...,"stack_top":...,"stack_size":...,"stack_used":...}` |
| LogsEnabled | `0x0039` | 1 byte: 0=disabled, 1=enabled |

---

## NET Chip Protocol (MGMT <-> NET)

### Commands (MGMT to NET): `CtlToNet` / `NetMgmtCmd`

| Command | Value | Payload |
|---------|-------|---------|
| Ping | `0x0040` | Arbitrary data |
| CircularPing | `0x0041` | Arbitrary data |
| AddWifiSsid | `0x0042` | JSON: `{"ssid":"...","password":"..."}` |
| GetWifiSsids | `0x0043` | None |
| ClearWifiSsids | `0x0044` | None |
| GetRelayUrl | `0x0045` | None |
| SetRelayUrl | `0x0046` | UTF-8 URL string |
| SetLoopback | `0x0047` | 1 byte: mode (0=Off, 1=Raw, 2=Moq) |
| GetLoopback | `0x0048` | None |
| GetLogsEnabled | `0x0049` | None |
| SetLogsEnabled | `0x004A` | 1 byte: 0=disabled, 1=enabled |
| ClearStorage | `0x004B` | None |
| GetLanguage | `0x004C` | None |
| SetLanguage | `0x004D` | UTF-8 language code |
| GetChannel | `0x004E` | None |
| SetChannel | `0x004F` | JSON array |
| GetAi | `0x0050` | None |
| SetAi | `0x0051` | JSON object |
| BurnJtagEfuse | `0x0052` | None |

### Responses (NET to MGMT): `NetToCtl` / `NetMgmtResp`

| Response | Value | Payload |
|----------|-------|---------|
| Pong | `0x0050` | Echo data |
| CircularPing | `0x0051` | Echo data |
| WifiSsids | `0x0052` | JSON: `[{"ssid":"...","password":"..."}, ...]` |
| RelayUrl | `0x0053` | UTF-8 URL string |
| Ack | `0x0054` | None |
| Error | `0x0055` | UTF-8 error string |
| Loopback | `0x0056` | 1 byte: mode |
| LogsEnabled | `0x0057` | 1 byte: 0=disabled, 1=enabled |
| Language | `0x0058` | UTF-8 language code |
| Channel | `0x0059` | JSON array |
| Ai | `0x005A` | JSON object |

---

## UI <-> NET Protocol (Direct Audio Link)

### UI to NET: `UiToNet`

| Message | Value | Payload |
|---------|-------|---------|
| CircularPing | `0x0060` | Arbitrary data |
| AudioFrame | `0x0061` | `[channel_id: u8][chunk_header][audio_data]` |

### NET to UI: `NetToUi`

| Message | Value | Payload |
|---------|-------|---------|
| CircularPing | `0x0070` | Arbitrary data |
| AudioFrame | `0x0071` | `[channel_id: u8][chunk_header][audio_data]` |

---

## TLV Frame Format

All communication uses TLV (Type-Length-Value) framing:

```
[SYNC: 4 bytes "LINK"][TYPE: 2 bytes LE][LENGTH: 4 bytes LE][VALUE: LENGTH bytes]
```

- **Sync word**: `0x4C 0x49 0x4E 0x4B` ("LINK" in ASCII)
- **Type**: Little-endian u16
- **Length**: Little-endian u32
- **Value**: Variable-length payload

---

## Testing

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

# NET commands
cargo run -- net wifi list
cargo run -- net wifi add "SSID" "password"
cargo run -- net relay get
```

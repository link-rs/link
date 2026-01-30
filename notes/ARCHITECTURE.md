# Link Architecture

This document provides an overview of the Link project architecture, a multi-chip audio communication system designed for secure, real-time audio streaming over the internet.

## System Overview

Link is a hardware device containing three microcontrollers that work together to capture audio, transmit it over a WebSocket connection, and play back received audio. The system is designed for low-latency, encrypted voice communication.

```mermaid
graph TB
    subgraph "Link Device"
        MGMT[MGMT Chip<br/>STM32F072CB<br/>Cortex-M0]
        UI[UI Chip<br/>STM32F405RG<br/>Cortex-M4F]
        NET[NET Chip<br/>ESP32-S3]

        MGMT <-->|UART 115200| UI
        MGMT <-->|UART 115200| NET
        UI <-->|UART 460800| NET
        MGMT -->|6MHz MCO| UI
        MGMT -->|Reset/Boot GPIO| UI
        MGMT -->|Reset/Boot GPIO| NET
    end

    subgraph "Host Computer"
        CTL[ctl CLI Tool]
        WEBCTL[web-ctl<br/>Browser Interface]
    end

    subgraph "Cloud"
        RELAY[Relay Server<br/>WebSocket]
    end

    CTL <-->|USB Serial| MGMT
    WEBCTL <-->|WebSerial API| MGMT
    NET <-->|WiFi + TLS WebSocket| RELAY
```

## Hardware Architecture

### Chip Roles

| Chip | MCU | Role | Key Peripherals |
|------|-----|------|-----------------|
| **MGMT** | STM32F072CB (Cortex-M0) | Central orchestrator, message routing | 3x UART, GPIO, MCO clock output |
| **UI** | STM32F405RG (Cortex-M4F) | Audio capture/playback, user input | I2S, I2C (codec + EEPROM), buttons, LED |
| **NET** | ESP32-S3 | Network connectivity | WiFi, NVS flash storage, LED |

### Physical Connections

```mermaid
graph LR
    subgraph "MGMT Chip"
        M_UART1[UART1<br/>to CTL]
        M_UART2[UART2<br/>to UI]
        M_UART3[UART3<br/>to NET]
        M_MCO[MCO<br/>6MHz]
        M_GPIO[GPIO<br/>Reset/Boot]
        M_LED[RGB LEDs<br/>A & B]
    end

    subgraph "UI Chip"
        U_UART[UART<br/>to MGMT]
        U_UART2[UART2<br/>to NET]
        U_I2S[I2S<br/>Audio]
        U_I2C[I2C<br/>Codec+EEPROM]
        U_BTN[Buttons<br/>A/B/Mic]
        U_LED[RGB LED]
    end

    subgraph "NET Chip"
        N_UART1[UART1<br/>to MGMT]
        N_UART2[UART2<br/>to UI]
        N_WIFI[WiFi<br/>Radio]
        N_NVS[NVS<br/>Flash]
        N_LED[RGB LED]
    end

    subgraph "External"
        CODEC[WM8960<br/>Audio Codec]
        EEPROM[EEPROM<br/>24LC64]
        MIC[Microphone]
        SPK[Speaker]
        HOST[Host Computer]
    end

    M_UART1 <--> HOST
    M_UART2 <--> U_UART
    M_UART3 <--> N_UART1
    U_UART2 <--> N_UART2
    M_MCO --> U_I2S
    M_GPIO --> U_UART
    M_GPIO --> N_UART1

    U_I2C --> CODEC
    U_I2C --> EEPROM
    U_I2S --> CODEC
    CODEC --> MIC
    CODEC --> SPK
```

## Software Architecture

### Crate Structure

```mermaid
graph TB
    subgraph "Firmware Binaries"
        MGMT_BIN[mgmt/<br/>MGMT Firmware]
        UI_BIN[ui/<br/>UI Firmware]
        NET_BIN[net/<br/>NET Firmware]
    end

    subgraph "Core Library"
        LINK[link/<br/>Core Logic]
        LINK_SHARED[shared/<br/>Protocol, TLV, Jitter Buffer]
        LINK_UI[ui/<br/>Audio, EEPROM, SFrame]
        LINK_NET[net/<br/>Storage, WebSocket]
        LINK_MGMT[mgmt/<br/>Routing, Reset Control]
        LINK_CTL[ctl/<br/>Host Control Logic]
    end

    subgraph "Host Tools"
        CTL_BIN[ctl/<br/>CLI Tool]
        WEBCTL_BIN[web-ctl/<br/>WASM Browser UI]
    end

    subgraph "Support"
        BOOTLOADER[bootloader/<br/>STM32 + ESP32]
        VENDOR[vendor/embassy/<br/>Async Runtime]
    end

    MGMT_BIN --> LINK
    UI_BIN --> LINK
    NET_BIN --> LINK
    CTL_BIN --> LINK
    WEBCTL_BIN --> LINK

    LINK --> LINK_SHARED
    LINK --> LINK_UI
    LINK --> LINK_NET
    LINK --> LINK_MGMT
    LINK --> LINK_CTL

    LINK_CTL --> BOOTLOADER
    CTL_BIN --> BOOTLOADER
```

### Feature Flags

The `link` crate supports multiple configurations:

| Feature | Purpose |
|---------|---------|
| `defmt` | Logging/debugging output for embedded |
| `trace-tlv` | Verbose TLV message tracing |
| `std` | Enables host-side code (ctl module) |
| `audio-buffer` | Jitter buffering with embassy-time |

## Communication Protocol

### TLV Message Format

All inter-chip communication uses a Type-Length-Value (TLV) protocol with sync word synchronization:

```
┌──────────────┬──────────┬──────────┬─────────────────┐
│  Sync Word   │   Type   │  Length  │      Value      │
│   4 bytes    │  2 bytes │  4 bytes │   0-640 bytes   │
│  0x4C494E4B  │   BE u16 │   BE u32 │    payload      │
│   ("LINK")   │          │          │                 │
└──────────────┴──────────┴──────────┴─────────────────┘
```

The sync word allows receivers to resynchronize after noise or bootloader garbage.

### Message Types

```mermaid
graph LR
    subgraph "Control Path"
        CTL_HOST[Host CTL]
        CTL_MSG[CtlToMgmt<br/>MgmtToCtl]
    end

    subgraph "Management"
        MGMT_CHIP[MGMT]
        UI_MSG[MgmtToUi<br/>UiToMgmt]
        NET_MSG[MgmtToNet<br/>NetToMgmt]
    end

    subgraph "Audio Path"
        UI_CHIP[UI]
        NET_CHIP[NET]
        AUDIO_MSG[UiToNet<br/>NetToUi]
    end

    CTL_HOST -->|CTL_MSG| MGMT_CHIP
    MGMT_CHIP -->|UI_MSG| UI_CHIP
    MGMT_CHIP -->|NET_MSG| NET_CHIP
    UI_CHIP -->|AUDIO_MSG| NET_CHIP
```

Key message types:

| Direction | Examples |
|-----------|----------|
| CtlToMgmt | Ping, Hello, ResetUi, ResetNet, WsSpeedTest |
| MgmtToUi | Ping, GetVersion, SetSFrameKey, SetLoopback |
| MgmtToNet | AddWifiSsid, SetRelayUrl, WsSend, SetLoopback |
| UiToNet | AudioFrameA, AudioFrameB (640-byte A-law audio) |
| NetToUi | AudioFrame (playback from network) |

## Audio Data Flow

### Audio Format

- **I2S Format**: Stereo 16-bit samples, interleaved L/R
- **Encoded Format**: A-law mono, 640 bytes per frame
- **Frame Rate**: 50 fps (20ms per frame at 32kHz)
- **Codec**: WM8960 (I2C control, I2S data)

### Capture Path (Recording)

```mermaid
sequenceDiagram
    participant MIC as Microphone
    participant CODEC as WM8960 Codec
    participant UI as UI Chip
    participant NET as NET Chip
    participant WS as WebSocket Relay

    MIC->>CODEC: Analog audio
    CODEC->>UI: I2S stereo samples
    UI->>UI: Extract mono channel
    UI->>UI: A-law encode (640 bytes)
    UI->>NET: UiToNet::AudioFrameA
    NET->>NET: Queue in jitter buffer
    NET->>WS: Binary WebSocket frame
```

### Playback Path (Receiving)

```mermaid
sequenceDiagram
    participant WS as WebSocket Relay
    participant NET as NET Chip
    participant UI as UI Chip
    participant CODEC as WM8960 Codec
    participant SPK as Speaker

    WS->>NET: Binary WebSocket frame
    NET->>NET: Jitter buffer ingestion
    NET->>UI: NetToUi::AudioFrame
    UI->>UI: Jitter buffer (20ms output)
    UI->>UI: A-law decode to mono
    UI->>UI: Expand to stereo
    UI->>CODEC: I2S stereo samples
    CODEC->>SPK: Analog audio
```

### Jitter Buffer

The jitter buffer absorbs network timing variations to provide smooth playback:

```mermaid
stateDiagram-v2
    [*] --> Buffering: Start
    Buffering --> Playing: Level >= 10 frames
    Playing --> Buffering: Underrun (empty)
    Playing --> Playing: Normal output

    note right of Buffering
        Accumulating frames
        Output: silence
    end note

    note right of Playing
        Steady 20ms output
        Capacity: 32 frames (640ms)
    end note
```

- **Capacity**: 32 frames (~640ms at 20ms/frame)
- **Start Level**: 10 frames (200ms) before playback begins
- **Statistics**: Tracks received, output, underruns, overruns

## Loopback Modes

Both UI and NET chips support loopback modes for testing:

```mermaid
graph LR
    subgraph "UI Loopback"
        MIC1[Mic] --> UI1[UI Chip] --> SPK1[Speaker]
    end

    subgraph "NET Loopback"
        UI2[UI Chip] --> NET1[NET Chip] --> UI3[UI Chip]
    end

    subgraph "Normal Mode"
        MIC2[Mic] --> UI4[UI] --> NET2[NET] --> WS[Relay] --> NET3[NET] --> UI5[UI] --> SPK2[Speaker]
    end
```

| Mode | Description | Use Case |
|------|-------------|----------|
| UI Loopback | Mic → Speaker (bypasses network) | Test audio hardware |
| NET Loopback | UI → NET → UI (bypasses WebSocket) | Test inter-chip audio path |
| Normal | Full path through WebSocket relay | Production operation |

## Bootloader Architecture

Firmware updates are performed via chip-specific bootloader protocols:

```mermaid
sequenceDiagram
    participant CTL as Host CTL
    participant MGMT as MGMT Chip
    participant TARGET as UI/NET Chip

    CTL->>MGMT: Reset target to bootloader
    MGMT->>TARGET: Assert BOOT + toggle RST
    TARGET->>TARGET: Enter bootloader mode

    loop Firmware Transfer
        CTL->>MGMT: Bootloader packet (tunneled)
        MGMT->>TARGET: Forward packet
        TARGET->>MGMT: Response
        MGMT->>CTL: Forward response
    end

    CTL->>MGMT: Reset target to user mode
    MGMT->>TARGET: Deassert BOOT + toggle RST
    TARGET->>TARGET: Run user firmware
```

### STM32 Bootloader (MGMT, UI)

- Protocol: USART bootloader (AN3155)
- Commands: Get, Read, Write, Erase, Go
- Parity: Even (required by bootloader)

### ESP32 Bootloader (NET)

- Protocol: ROM bootloader with SLIP framing
- Features: Compression, MD5 verification
- Commands: Sync, Read, Write, Flash data

## Host Control Tools

### CLI Tool (`ctl`)

The `ctl` binary provides command-line access to all device functions:

```
ctl <chip> <command> [args]

Examples:
  ctl ui ping                    # Test UI communication
  ctl ui version                 # Get firmware version
  ctl ui version set 123         # Set firmware version
  ctl ui loopback set true       # Enable UI loopback
  ctl net wifi                   # List WiFi networks
  ctl net wifi add SSID pass     # Add WiFi network
  ctl net relay-url set wss://.. # Set relay URL
  ctl net flash firmware.bin     # Flash NET firmware
```

### Web Interface (`web-ctl`)

A browser-based interface using WebSerial API:

```mermaid
graph TB
    subgraph "Browser"
        JS[JavaScript UI]
        WASM[web-ctl.wasm]
        WEBSERIAL[WebSerial API]
    end

    subgraph "Device"
        USB[USB Serial]
        MGMT[MGMT Chip]
    end

    JS --> WASM
    WASM --> WEBSERIAL
    WEBSERIAL --> USB
    USB --> MGMT
```

Features:
- Connect/disconnect device
- Read/write all configuration
- Flash firmware with progress
- Auto-populate state on connect

## Persistent Storage

### UI Chip (EEPROM)

| Address | Size | Content |
|---------|------|---------|
| 0x00 | 4 bytes | Firmware version (u32 BE) |
| 0x04 | 16 bytes | SFrame encryption key |

### NET Chip (NVS Flash)

| Key | Content |
|-----|---------|
| `wifi_ssids` | Serialized WiFi credentials (up to 8) |
| `relay_url` | WebSocket relay server URL |

## Security

### SFrame Encryption

Audio frames can be encrypted using the SFrame protocol:
- **Algorithm**: AES-128-GCM
- **Key Derivation**: HKDF-SHA256
- **Key Storage**: UI chip EEPROM
- **Purpose**: End-to-end encryption of audio data

### WebSocket Transport

- **Protocol**: WSS (WebSocket over TLS)
- **Cipher Suite**: TLS 1.2+ with AES-128-GCM-SHA256
- **Authentication**: Server certificate validation

## LED Indicators

| LED | Color | Meaning |
|-----|-------|---------|
| MGMT LED A | Green | MGMT chip healthy |
| MGMT LED A | Red | MGMT chip error |
| MGMT LED B | Blue | NET WiFi connected |
| MGMT LED B | Red | NET WiFi disconnected |
| UI LED | Blue | Audio activity |
| NET LED | Blue | WiFi connected |
| NET LED | Red | WiFi disconnected |

## Build System

### Makefile Targets

```bash
# Flash firmware
make flash-mgmt    # Flash MGMT chip
make flash-ui      # Flash UI chip
make flash-net     # Flash NET chip

# Build web interfaces
make web-ctl       # Build browser control interface
make web-link      # Build virtual device simulator

# Development
make serve-web     # Serve web-ctl locally
make test          # Run all tests
```

### Dependencies

- **Embassy**: Async runtime for embedded Rust
- **esp-rs**: ESP32 Rust ecosystem
- **wasm-pack**: WebAssembly packaging
- **probe-rs**: Flash programming

## Testing

### Unit Tests

```bash
cd link && cargo test --features std
```

Tests cover:
- TLV encoding/decoding
- Jitter buffer behavior
- Audio codec algorithms
- Storage serialization
- GPIO sequences for reset

### Integration Tests

The `testing.rs` module provides end-to-end tests using mock channels:

```rust
#[tokio::test]
async fn ctl_ui_ping() {
    device_test(|mut ctl| async move {
        ctl.ui_ping(b"hello").await;
    }).await;
}
```

## Future Considerations

- **Multiple Relay Servers**: Failover and load balancing
- **Peer-to-Peer**: Direct device-to-device communication
- **Audio Processing**: Noise cancellation, echo suppression
- **Battery Management**: Power optimization for portable use

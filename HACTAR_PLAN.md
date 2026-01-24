# Implementation Plan: Hactar Protocol in Link Firmware

This document describes the changes needed to make the link firmware implement the hactar application-layer protocol as documented in [HACTAR_ARCH.md](HACTAR_ARCH.md).

## Current State Analysis

### What net-idf Already Has

| Feature | Location | Status |
|---------|----------|--------|
| MoQ transport via quicr | `net-idf/src/main.rs:27` | Working |
| ClientBuilder, FullTrackName, Subscription | quicr crate | Working |
| WiFi connectivity with NVS storage | `net-idf/src/main.rs:834-943` | Working |
| TLV inter-chip protocol | `net-idf/src/main.rs:1243-1314` | Working |
| MoQ loopback mode (publish + subscribe) | `net-idf/src/main.rs:279-342` | Working |
| Audio frame forwarding to MoQ | `net-idf/src/main.rs:334-343` | Working |
| Audio reception from MoQ | `net-idf/src/main.rs:439-467` | Working |
| Relay URL storage/connection | `net-idf/src/main.rs:155-198` | Working |

### What link Shared Library Has

| Feature | Location | Status |
|---------|----------|--------|
| Audio capture/playback | `link/src/ui/audio.rs`, `ui/src/main.rs` | Working |
| A-law encoding/decoding | `link/src/ui/audio.rs` | Working |
| SFrame encryption (RFC 9605) | `link/src/ui/sframe.rs` | Working |
| Button A/B handling | `link/src/ui/mod.rs` | Working |
| EEPROM SFrame key storage | `link/src/ui/eeprom.rs` | Working |

### What's Missing for Hactar Compatibility

| Feature | Hactar Location | Notes |
|---------|-----------------|-------|
| Channel ID routing (3 channels) | `ui_net_link.hh:25-32` | Replace AudioFrameA/B |
| Chunk message format | `ui_net_link.hh:96-135` | MessageType, Chunk, AIRequestChunk |
| Hierarchical track namespaces | `config_builder.hh` | moq://moq.ptt.arpa/v1/... |
| Per-channel readers/writers | `net/core/src/net.cc:468-528` | 3 tracks instead of 1 |
| JSON track change commands | `net/core/src/net.cc:213-238` | Dynamic track reconfiguration via ChatAi |

## Implementation Steps

### Phase 1: Protocol Updates (link/src/shared)

#### 1.1 Add Channel ID and Message Type Enums

**File:** `link/src/shared/protocol.rs`

```rust
/// Channel ID for routing messages (matches hactar ui_net_link.hh)
#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum ChannelId {
    /// Push-to-talk audio (human voice)
    Ptt = 0,
    /// AI audio channel (AI-generated voice)
    PttAi = 1,
    // Chat = 2 is reserved but not implemented
    /// AI text/JSON responses for track reconfiguration
    ChatAi = 3,
}

/// Message type within a chunk (matches hactar ui_net_link.hh)
#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum MessageType {
    Media = 1,
    AiRequest = 2,
    AiResponse = 3,
}
```

#### 1.2 Add Chunk Serialization Module

**File:** `link/src/shared/chunk.rs` (new file)

```rust
//! Chunk format for hactar-compatible message encoding.
//!
//! The chunk format wraps audio data with metadata before SFrame encryption.

use super::protocol::MessageType;

/// Audio chunk header size for Media type: type(1) + last_chunk(1) + chunk_length(4)
pub const MEDIA_HEADER_SIZE: usize = 6;

/// Audio chunk header size for AIRequest type: type(1) + request_id(4) + last_chunk(1) + chunk_length(4)
pub const AI_REQUEST_HEADER_SIZE: usize = 10;

/// Serialize a media chunk (for Ptt channel)
pub fn serialize_media_chunk(
    audio_data: &[u8],
    last_chunk: bool,
    out: &mut [u8],
) -> usize {
    let audio_len = audio_data.len();
    out[0] = MessageType::Media as u8;
    out[1] = last_chunk as u8;
    out[2..6].copy_from_slice(&(audio_len as u32).to_le_bytes());
    out[6..6 + audio_len].copy_from_slice(audio_data);
    6 + audio_len
}

/// Serialize an AI request chunk (for PttAi channel)
pub fn serialize_ai_request_chunk(
    audio_data: &[u8],
    request_id: u32,
    last_chunk: bool,
    out: &mut [u8],
) -> usize {
    let audio_len = audio_data.len();
    out[0] = MessageType::AiRequest as u8;
    out[1..5].copy_from_slice(&request_id.to_le_bytes());
    out[5] = last_chunk as u8;
    out[6..10].copy_from_slice(&(audio_len as u32).to_le_bytes());
    out[10..10 + audio_len].copy_from_slice(audio_data);
    10 + audio_len
}

/// Parse a received chunk, returning (message_type, audio_data_offset, audio_length)
pub fn parse_chunk(data: &[u8]) -> Option<(MessageType, usize, usize)> {
    if data.is_empty() {
        return None;
    }

    let msg_type = MessageType::try_from(data[0]).ok()?;

    match msg_type {
        MessageType::Media => {
            if data.len() < MEDIA_HEADER_SIZE {
                return None;
            }
            let chunk_len = u32::from_le_bytes([data[2], data[3], data[4], data[5]]) as usize;
            Some((msg_type, 6, chunk_len))
        }
        MessageType::AiRequest => {
            if data.len() < AI_REQUEST_HEADER_SIZE {
                return None;
            }
            let chunk_len = u32::from_le_bytes([data[6], data[7], data[8], data[9]]) as usize;
            Some((msg_type, 10, chunk_len))
        }
        MessageType::AiResponse => {
            // AI response: type(1) + request_id(4) + content_type(1) + last_chunk(1) + chunk_length(4)
            if data.len() < 11 {
                return None;
            }
            let chunk_len = u32::from_le_bytes([data[7], data[8], data[9], data[10]]) as usize;
            Some((msg_type, 11, chunk_len))
        }
    }
}
```

#### 1.3 Update UiToNet/NetToUi Messages

**File:** `link/src/shared/protocol.rs`

Replace `AudioFrameA`/`AudioFrameB` with channel-based messages:

```rust
#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum UiToNet {
    CircularPing = 0x60,
    /// Audio frame with channel_id prefix + encrypted chunk
    /// Format: [channel_id: u8][sframe_header][encrypted_chunk][auth_tag]
    AudioFrame = 0x61,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum NetToUi {
    CircularPing = 0x70,
    /// Audio frame to play (channel_id prefix + encrypted chunk)
    /// Format: [channel_id: u8][sframe_header][encrypted_chunk][auth_tag]
    AudioFrame = 0x71,
}
```

### Phase 2: UI Chip Message Format Changes (link/src/ui)

#### 2.1 Update Outbound Audio Processing

**File:** `link/src/ui/mod.rs`

Modify `Event::AudioFrame` handler to use chunk format:

```rust
Event::AudioFrame(frame) => 'audio: {
    let Some(button) = active_button else {
        break 'audio;
    };

    // Determine channel based on button
    let channel_id = match button {
        Button::A => ChannelId::Ptt,
        Button::B => ChannelId::PttAi,
    };

    // Build output buffer: channel_id + chunk (to be encrypted)
    let mut out_buf: heapless::Vec<u8, 256> = heapless::Vec::new();

    // Channel ID is NOT encrypted - stays in plaintext
    out_buf.push(channel_id as u8).ok();

    // Serialize chunk based on channel type
    let mut chunk_buf = [0u8; 200]; // Enough for header + 160 bytes audio
    let chunk_len = match channel_id {
        ChannelId::Ptt => {
            chunk::serialize_media_chunk(&frame.0, false, &mut chunk_buf)
        }
        ChannelId::PttAi => {
            chunk::serialize_ai_request_chunk(&frame.0, request_id, false, &mut chunk_buf)
        }
    };

    // Encrypt chunk data (skip channel_id at position 0)
    let mut encrypt_buf: heapless::Vec<u8, 256> = heapless::Vec::new();
    encrypt_buf.extend_from_slice(&chunk_buf[..chunk_len]).ok();
    if sframe_state.protect(&[], &mut encrypt_buf).is_err() {
        break 'audio;
    }

    // Append encrypted data after channel_id
    out_buf.extend_from_slice(&encrypt_buf).ok();

    // Send to NET chip
    to_net.must_write_tlv(UiToNet::AudioFrame, &out_buf).await;
}
```

#### 2.2 Update Inbound Audio Processing

Modify `handle_net` to decrypt and parse chunks:

```rust
async fn handle_net<M>(
    tlv: Tlv<NetToUi>,
    to_mgmt: &mut M,
    sframe_state: &SFrameState,
) -> Option<Frame>
where
    M: WriteTlv<UiToMgmt>,
{
    match tlv.tlv_type {
        NetToUi::AudioFrame => {
            if tlv.value.len() < 2 {
                return None;
            }

            // Extract channel_id (first byte, plaintext)
            let _channel_id = ChannelId::try_from(tlv.value[0]).ok()?;

            // Decrypt the rest (sframe header + encrypted chunk + auth tag)
            let mut buf: heapless::Vec<u8, 256> = heapless::Vec::new();
            buf.extend_from_slice(&tlv.value[1..]).ok();
            if sframe_state.unprotect(&[], &mut buf).is_err() {
                return None;
            }

            // Parse chunk
            let (msg_type, audio_offset, audio_len) = chunk::parse_chunk(&buf)?;

            match msg_type {
                MessageType::Media | MessageType::AiResponse => {
                    // Audio data
                    Frame::from_bytes(&buf[audio_offset..audio_offset + audio_len])
                }
                _ => None,
            }
        }
        _ => None,
    }
}
```

### Phase 3: NET Chip Changes (net-idf/src/main.rs)

#### 3.1 Add Channel-Based Track Management

Replace single loopback track with per-channel writers/readers:

```rust
use std::collections::HashMap;

/// Track namespace builder matching hactar's config_builder.hh
fn build_track_namespace(
    base: &[&str],          // ["moq://moq.ptt.arpa/v1", "org/acme", "store/1234"]
    channel_name: &str,     // "gardening"
    track_type: &str,       // "ptt"
) -> TrackNamespace {
    let mut parts: Vec<String> = base.iter().map(|s| s.to_string()).collect();
    parts.push(format!("channel/{}", channel_name));
    parts.push(track_type.to_string());
    TrackNamespace::from_strings(&parts.iter().map(|s| s.as_str()).collect::<Vec<_>>())
}

/// Build track name for audio (matches hactar format)
fn build_audio_track_name(lang: &str) -> String {
    format!("pcm_{}_8khz_mono_i16", lang)
}

/// MoQ state for channel-based routing
struct MoqChannelState {
    /// Writers indexed by ChannelId
    writers: HashMap<u8, std::sync::Arc<quicr::PublishTrack>>,
    /// Subscriptions indexed by ChannelId
    subscriptions: HashMap<u8, Subscription>,
    /// Group IDs per channel
    group_ids: HashMap<u8, u64>,
}

impl MoqChannelState {
    fn new() -> Self {
        Self {
            writers: HashMap::new(),
            subscriptions: HashMap::new(),
            group_ids: HashMap::new(),
        }
    }
}
```

#### 3.2 Update MoqCommand Enum

Add channel-aware commands:

```rust
enum MoqCommand {
    /// Set the relay URL (triggers reconnect if changed).
    SetRelayUrl(String),
    /// Configure channel settings (namespace, channel name, language)
    ConfigureChannel {
        base_namespace: Vec<String>,
        channel_name: String,
        language: String,
    },
    /// Start publishing/subscribing on configured channels
    StartChannels,
    /// Stop all channels
    StopChannels,
    /// Audio frame to publish with channel routing
    AudioFrame {
        channel_id: u8,
        data: Vec<u8>,
    },
    // ... keep existing commands for testing
    RunClock,
    RunBenchmark { fps: u32, payload_size: u32 },
    StopMode,
}
```

#### 3.3 Update MoqEvent Enum

```rust
enum MoqEvent {
    Connected,
    Disconnected,
    ChannelsStarted,
    ChannelsStopped,
    Error { message: String },
    /// Audio received on a specific channel
    AudioReceived { channel_id: u8, data: Vec<u8> },
}
```

#### 3.4 Update MoQ Task for Channel Routing

In `spawn_moq_task`, add channel state and handling:

```rust
// In the MoQ task loop
Ok(MoqCommand::ConfigureChannel { base_namespace, channel_name, language }) => {
    // Store configuration for later use
    channel_config = Some(ChannelConfig {
        base_namespace,
        channel_name,
        language,
    });
}

Ok(MoqCommand::StartChannels) => {
    if let (Some(ref c), Some(ref config)) = (&client, &channel_config) {
        // Create Ptt writer track
        let base: Vec<&str> = config.base_namespace.iter().map(|s| s.as_str()).collect();
        let ptt_ns = build_track_namespace(&base, &config.channel_name, "ptt");
        c.publish_namespace(&ptt_ns);

        let track_name = build_audio_track_name(&config.language);
        let ptt_track = FullTrackName::from_strings(
            &[&ptt_ns.to_string()],
            &track_name
        );

        match block_on(c.publish(ptt_track.clone())) {
            Ok(track) => {
                channel_state.writers.insert(0, track); // ChannelId::Ptt = 0
                channel_state.group_ids.insert(0, 0);
            }
            Err(e) => warn!("Failed to create Ptt writer: {:?}", e),
        }

        // Create Ptt subscription
        match block_on(c.subscribe(ptt_track)) {
            Ok(sub) => {
                channel_state.subscriptions.insert(0, sub);
            }
            Err(e) => warn!("Failed to subscribe to Ptt: {:?}", e),
        }

        // Optionally create PttAi tracks (channel_id = 1)
        // ... similar pattern

        let _ = event_tx.send(MoqEvent::ChannelsStarted);
    }
}

Ok(MoqCommand::AudioFrame { channel_id, data }) => {
    // Publish to the appropriate channel's track
    if let Some(track) = channel_state.writers.get(&channel_id) {
        let group_id = channel_state.group_ids.entry(channel_id).or_insert(0);
        let headers = ObjectHeaders::new(*group_id, 0);
        let _ = track.publish(&headers, &data);
        *group_id += 1;
    }
}
```

#### 3.5 Update UI Message Handler

**File:** `net-idf/src/main.rs`, `handle_ui_message` function:

```rust
fn handle_ui_message(
    msg_type: UiToNet,
    value: &[u8],
    mgmt_uart: &UartDriver,
    ui_uart: &UartDriver,
    loopback: bool,
    moq_cmd_tx: &Sender<MoqCommand>,
) {
    match msg_type {
        UiToNet::CircularPing => {
            write_tlv(mgmt_uart, NetToMgmt::CircularPing, value);
        }
        UiToNet::AudioFrame => {
            // New format: channel_id (1 byte) + encrypted payload
            if value.is_empty() {
                return;
            }

            let channel_id = value[0];
            let encrypted_payload = &value[1..];

            if loopback {
                // Local loopback - forward directly back to UI
                write_tlv(ui_uart, NetToUi::AudioFrame, value);
            }

            // Send to MoQ with channel routing
            let _ = moq_cmd_tx.send(MoqCommand::AudioFrame {
                channel_id,
                data: encrypted_payload.to_vec(),
            });
        }
        // Keep legacy handling for compatibility during migration
        UiToNet::AudioFrameA | UiToNet::AudioFrameB => {
            // Map old format to new: treat A as Ptt (0), B as PttAi (1)
            let channel_id = if msg_type == UiToNet::AudioFrameA { 0 } else { 1 };

            if loopback {
                write_tlv(ui_uart, NetToUi::AudioFrame, value);
            }
            let _ = moq_cmd_tx.send(MoqCommand::AudioFrame {
                channel_id,
                data: value.to_vec(),
            });
        }
    }
}
```

#### 3.6 Update MoQ Event Handling

In main loop, handle channel-based audio reception:

```rust
Ok(MoqEvent::AudioReceived { channel_id, data }) => {
    // Prepend channel_id and forward to UI chip
    let mut buf = Vec::with_capacity(1 + data.len());
    buf.push(channel_id);
    buf.extend_from_slice(&data);
    write_tlv(&ui_uart, NetToUi::AudioFrame, &buf);
}
```

#### 3.7 Poll Subscriptions for All Channels

In the MoQ task, poll all active subscriptions:

```rust
// In mode handling section
for (channel_id, subscription) in channel_state.subscriptions.iter_mut() {
    // Non-blocking poll for received objects
    let mut recv_future = subscription.recv();
    let pinned = unsafe { Pin::new_unchecked(&mut recv_future) };
    if let Poll::Ready(object) = pinned.poll(&mut cx) {
        let _ = event_tx.send(MoqEvent::AudioReceived {
            channel_id: *channel_id,
            data: object.payload().to_vec(),
        });
    }
}
```

#### 3.8 Handle ChatAi Channel for JSON Track Commands

Data received on the ChatAi channel (channel_id = 3) contains JSON commands for track reconfiguration. The NET chip parses these and updates its track subscriptions/publications:

```rust
Ok(MoqEvent::AudioReceived { channel_id, data }) => {
    if channel_id == 3 {
        // ChatAi channel - parse JSON track change command
        if let Ok(json_str) = std::str::from_utf8(&data) {
            if let Ok(config) = serde_json::from_str::<TrackChangeConfig>(json_str) {
                // Reconfigure tracks based on JSON
                let _ = moq_cmd_tx.send(MoqCommand::ConfigureChannel {
                    base_namespace: config.base_namespace,
                    channel_name: config.channel_name,
                    language: config.language,
                });
                let _ = moq_cmd_tx.send(MoqCommand::StartChannels);
            }
        }
    } else {
        // Audio channel - forward to UI chip
        let mut buf = Vec::with_capacity(1 + data.len());
        buf.push(channel_id);
        buf.extend_from_slice(&data);
        write_tlv(&ui_uart, NetToUi::AudioFrame, &buf);
    }
}
```

### Phase 4: Configuration and Storage

#### 4.1 Add Channel Configuration to NVS

**File:** `net-idf/src/main.rs`

```rust
const NVS_KEY_CHANNEL_NAME: &str = "channel_name";
const NVS_KEY_LANGUAGE: &str = "language";
const NVS_KEY_BASE_NS: &str = "base_ns";

impl NvsStorage {
    // Add fields
    channel_name: String,
    language: String,
    base_namespace: Vec<String>,

    // Add load/save methods
    fn load_channel_config(&mut self) { ... }
    fn save_channel_config(&mut self) { ... }
}
```

#### 4.2 Add MGMT Commands for Channel Configuration

**File:** `link/src/shared/protocol.rs`

```rust
#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToNet {
    // ... existing commands ...
    SetChannelName = 0x40,
    GetChannelName = 0x41,
    SetLanguage = 0x42,
    GetLanguage = 0x43,
    SetBaseNamespace = 0x44,
    GetBaseNamespace = 0x45,
    StartChannels = 0x46,
    StopChannels = 0x47,
}
```

## Implementation Order

1. **Phase 1**: Protocol updates in `link/src/shared/`
   - Add `ChannelId`, `MessageType` enums
   - Add `chunk.rs` module
   - Update `UiToNet`/`NetToUi` message types

2. **Phase 2**: UI chip updates in `link/src/ui/`
   - Update outbound audio to use chunk format
   - Update inbound parsing for chunk format

3. **Phase 3**: NET chip (net-idf) updates
   - Add channel state management
   - Update `MoqCommand`/`MoqEvent` for channels
   - Update UI message handling for channel routing
   - Add multi-subscription polling

4. **Phase 4**: Configuration and storage
   - Add NVS storage for channel config
   - Add MGMT commands for configuration

## Testing Strategy

### Unit Tests

1. **Chunk serialization**: Test `serialize_media_chunk`, `serialize_ai_request_chunk`, `parse_chunk`
2. **Channel ID mapping**: Verify button → channel → track routing

### Integration Tests

1. **UI loopback**: Verify chunk format survives encrypt→decrypt cycle
2. **NET channel routing**: Verify correct track selection based on channel_id
3. **MoQ publish/subscribe**: Test audio round-trip through relay

### End-to-End Tests

1. **Audio round-trip**: Record → encode → chunk → encrypt → MoQ → decrypt → parse → decode → playback
2. **Multi-channel**: Test Ptt and PttAi channels work independently
3. **Interop test**: Exchange audio between hactar device and link device

## Wire Format Compatibility

The encrypted packet format matches hactar exactly:

```
| channel_id (1 byte) | SFrame header (1-2 bytes) | encrypted chunk | auth tag (16 bytes) |
       PLAINTEXT                        ENCRYPTED
```

The chunk format inside the encrypted portion:

```
Media chunk (ChannelId::Ptt):
| type=1 (1) | last_chunk (1) | chunk_length (4) | audio_data (160) |

AIRequest chunk (ChannelId::PttAi):
| type=2 (1) | request_id (4) | last_chunk (1) | chunk_length (4) | audio_data (160) |
```

## Files to Modify/Create

| File | Action | Description |
|------|--------|-------------|
| `link/src/shared/protocol.rs` | Modify | Add `ChannelId`, `MessageType`, update message types |
| `link/src/shared/chunk.rs` | Create | Chunk serialization/deserialization |
| `link/src/shared/mod.rs` | Modify | Export chunk module |
| `link/src/ui/mod.rs` | Modify | Update audio frame handling for chunks |
| `net-idf/src/main.rs` | Modify | Add channel routing, update MoQ commands/events |

## Open Questions

1. **Track discovery**: How do devices discover each other's track names for subscriptions?
2. **SFrame key sync**: Current key is in EEPROM - how to synchronize across devices?
3. **AI channel support**: Is PttAi channel support needed immediately?

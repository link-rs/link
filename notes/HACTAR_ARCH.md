# Hactar Firmware Architecture

This document describes the application-layer architecture of the old hactar firmware, specifically how audio and messages are formatted, encrypted, and transmitted over MoQ (Media over QUIC).

## Overview

The hactar device has two chips with application-layer responsibilities:
- **UI chip**: Handles audio capture/playback, encryption/decryption, and message formatting
- **NET chip**: Handles MoQ connection management, track publishing/subscribing, and JSON command parsing

## Data Flow Summary

```
OUTBOUND (UI → NET → MoQ):
  Mic → Stereo PCM → Mono extraction → A-law encode → Chunk format → SFrame encrypt → UART → MoQ publish

INBOUND (MoQ → NET → UI):
  MoQ subscribe → UART → SFrame decrypt → Parse chunk → A-law decode → Stereo expand → Speaker
```

## Channel IDs

Both chips use a shared channel ID enum to route messages to the correct track:

```cpp
// ../hactar/firmware/shared_inc/ui_net_link.hh:25-32
enum class Channel_Id : uint8_t {
    Ptt,       // 0 - Push-to-talk audio (human voice)
    Ptt_Ai,    // 1 - AI audio channel (AI-generated voice)
    Chat,      // 2 - Text chat
    Chat_Ai,   // 3 - AI text/JSON responses
    Count      // 4
};
```

## UI Chip Processing

### Audio Capture and Encoding

1. **Stereo to Mono**: Raw I2S stereo samples are captured at 8kHz
2. **A-law Encoding**: Mono samples are compressed using A-law codec
   - Frame size: 160 samples = 160 bytes A-law
   - Frame duration: 20ms

```cpp
// ../hactar/firmware/ui/src/app_main.cc:700-718
void SendAudio(const ui_net_link::Channel_Id channel_id,
               const ui_net_link::Packet_Type packet_type,
               bool last)
{
    // Compress stereo PCM to mono A-law
    AudioCodec::ALawCompand(audio_chip.RxBuffer(), constants::Audio_Buffer_Sz,
                            talk_frame.data, constants::Audio_Phonic_Sz,
                            true, constants::Stereo);

    talk_frame.channel_id = channel_id;
    ui_net_link::Serialize(talk_frame, packet_type, last, message_packet);

    if (!TryProtect(&message_packet)) {
        return;
    }

    net_serial.Write(message_packet);
}
```

### Message Chunk Format

Messages are wrapped in a chunk structure before encryption. The format depends on the message type:

```cpp
// ../hactar/firmware/shared_inc/ui_net_link.hh:82-135
enum class MessageType : uint8_t {
    Media = 1,      // Regular audio
    AIRequest,      // Audio to AI
    AIResponse,     // Response from AI
    Chat,           // Text chat
};

// Media chunk (for Ptt channel):
struct __attribute__((packed)) Chunk {
    MessageType type;           // 1 byte: MessageType::Media (0x01)
    uint8_t last_chunk;         // 1 byte: 0 or 1
    uint32_t chunk_length;      // 4 bytes: 160 (little-endian)
    uint8_t chunk_data[160];    // Audio data
};
// Total: 166 bytes

// AI Request chunk (for Ptt_Ai channel):
struct __attribute__((packed)) AIRequestChunk {
    MessageType type;           // 1 byte: MessageType::AIRequest (0x02)
    uint32_t request_id;        // 4 bytes
    bool last_chunk;            // 1 byte
    uint32_t chunk_length;      // 4 bytes
    uint8_t chunk_data[160];    // Audio data
};
// Total: 170 bytes
```

### Serialization (UI → NET)

The `Serialize` function formats outgoing messages:

```cpp
// ../hactar/firmware/shared_inc/ui_net_link.hh:178-230
static void Serialize(const AudioObject& talk_frame,
                      Packet_Type packet_type,
                      bool is_last,
                      link_packet_t& packet)
{
    packet.type = (uint8_t)packet_type;  // Message = 0x07
    packet.payload[0] = (uint8_t)talk_frame.channel_id;

    uint32_t offset = 1;

    if (talk_frame.channel_id == Channel_Id::Ptt) {
        // Media chunk format
        packet.payload[offset++] = MessageType::Media;       // type
        packet.payload[offset++] = is_last;                  // last_chunk
        memcpy(packet.payload + offset, &audio_size, 4);     // chunk_length
        offset += 4;
    }
    else if (talk_frame.channel_id == Channel_Id::Ptt_Ai) {
        // AI Request chunk format
        packet.payload[offset++] = MessageType::AIRequest;   // type
        memcpy(packet.payload + offset, &request_id, 4);     // request_id
        offset += 4;
        packet.payload[offset++] = is_last;                  // last_chunk
        memcpy(packet.payload + offset, &audio_size, 4);     // chunk_length
        offset += 4;
    }

    // Append audio data
    memcpy(packet.payload + offset, talk_frame.data, 160);
    packet.length = offset + 160;
}
```

### Packet Structure Before Encryption

```
┌─────────────┬────────────────────────────────────────────────┐
│ Payload[0]  │ Channel_Id (1 byte): 0=Ptt, 1=Ptt_Ai, etc.     │
├─────────────┼────────────────────────────────────────────────┤
│ Payload[1]  │ MessageType (1 byte): Media=1, AIRequest=2...  │
├─────────────┼────────────────────────────────────────────────┤
│ Payload[2+] │ Chunk header fields (varies by MessageType)    │
├─────────────┼────────────────────────────────────────────────┤
│ Payload[N+] │ A-law audio data (160 bytes)                   │
└─────────────┴────────────────────────────────────────────────┘
```

### SFrame Encryption

The UI chip uses SFrame (RFC 9605) with AES-128-GCM-SHA256 for encryption:

```cpp
// ../hactar/firmware/ui/src/app_main.cc:822-836
bool TryProtect(link_packet_t* packet)
{
    uint8_t ct[link_packet_t::Payload_Size];
    // Skip first byte (channel_id) when encrypting
    auto payload = mls_ctx.protect(
        0, 0, ct,
        sframe::input_bytes{packet->payload, packet->length}.subspan(1),
        {});

    // Copy ciphertext back, preserving channel_id at position 0
    std::memcpy(packet->payload + 1, payload.data(), payload.size());
    packet->length = payload.size() + 1;
    return true;
}
```

**Key points:**
- Channel ID (first byte) is NOT encrypted - it stays in plaintext
- SFrame header + encrypted payload + auth tag replaces the rest
- Key is 16 bytes, stored in EEPROM

### Packet Structure After Encryption

```
┌─────────────┬────────────────────────────────────────────────┐
│ Payload[0]  │ Channel_Id (1 byte) - PLAINTEXT                │
├─────────────┼────────────────────────────────────────────────┤
│ Payload[1]  │ SFrame header (1-17 bytes, typically 1-2)      │
├─────────────┼────────────────────────────────────────────────┤
│ Payload[N]  │ Encrypted chunk data                           │
├─────────────┼────────────────────────────────────────────────┤
│ Payload[M]  │ AES-GCM auth tag (16 bytes)                    │
└─────────────┴────────────────────────────────────────────────┘
```

### Inbound Processing (NET → UI)

When the UI receives a packet from NET:

```cpp
// ../hactar/firmware/ui/src/app_main.cc:399-488
void HandleNetLinkPackets()
{
    while (link_packet = net_serial.Read()) {
        switch ((ui_net_link::Packet_Type)link_packet->type) {
        case ui_net_link::Packet_Type::Message:
        case ui_net_link::Packet_Type::AiResponse:
            // Decrypt first
            if (!TryUnprotect(link_packet)) {
                continue;
            }

            // Parse message type from payload[1]
            auto message_type = static_cast<MessageType>(link_packet->payload[1]);

            switch (message_type) {
            case MessageType::Media:
                HandleMedia(link_packet);  // Decode and play audio
                break;
            case MessageType::AIResponse:
                HandleAiResponse(link_packet);  // Handle AI audio or JSON
                break;
            case MessageType::Chat:
                HandleChat(link_packet);  // Display text
                break;
            }
        }
    }
}
```

### Audio Playback

```cpp
// ../hactar/firmware/ui/src/app_main.cc:721-726
void HandleMedia(link_packet_t* packet)
{
    ui_net_link::Deserialize(*link_packet, play_frame);
    // Expand A-law mono to stereo PCM
    AudioCodec::ALawExpand(play_frame.data, constants::Audio_Phonic_Sz,
                           audio_chip.TxBuffer(), constants::Audio_Buffer_Sz,
                           constants::Stereo, true);
}
```

## NET Chip Processing

### Track Setup and JSON Configuration

The NET chip uses JSON to configure MoQ tracks. Tracks are created at startup and can be dynamically updated via AI responses:

```cpp
// ../hactar/firmware/net/core/src/net.cc:673-700
ChannelBuilder channel_builder({"moq://moq.ptt.arpa/v1", "org/acme", "store/1234"}, device_id);

const std::string lang = language.Load();      // e.g., "en-US"
const std::string channel = default_channel.Load();  // e.g., "gardening"

// Create publication tracks
channel_builder.AddAIAudioPublicationChannel(lang);
channel_builder.AddPublicationChannel(channel, lang, "pcm");

// Create subscription tracks
channel_builder.AddSubscriptionChannel(channel, lang, "pcm");

json config_j = channel_builder.GetConfig();
// config_j contains "publications" and "subscriptions" arrays
```

### JSON Track Configuration Format

```json
{
  "publications": [
    {
      "tracknamespace": ["moq://moq.ptt.arpa/v1", "org/acme", "store/1234", "channel/gardening", "ptt"],
      "trackname": "pcm_en_8khz_mono_i16",
      "codec": "pcm",
      "channel_name": "gardening"
    }
  ],
  "subscriptions": [
    {
      "tracknamespace": ["moq://moq.ptt.arpa/v1", "org/acme", "store/1234", "channel/gardening", "ptt"],
      "trackname": "<device_id>",
      "codec": "pcm",
      "channel_name": "gardening"
    }
  ]
}
```

### Track Creation

```cpp
// ../hactar/firmware/net/core/src/net.cc:468-528
std::shared_ptr<moq::TrackReader> CreateReadTrack(const json& subscription, Serial& serial)
{
    std::vector<std::string> track_namespace = subscription.at("tracknamespace");
    std::string trackname = subscription.at("trackname");
    std::string codec = subscription.at("codec");
    std::string channel_name = subscription.at("channel_name");

    // Determine channel index based on codec and channel_name
    uint32_t offset = 0;
    if (codec == "pcm") {
        if (channel_name == "self_ai_audio") {
            offset = Channel_Id::Ptt_Ai;
        } else {
            offset = Channel_Id::Ptt;
        }
    } else if (codec == "ascii") {
        offset = Channel_Id::Chat;
    } else if (codec == "ai_cmd_response:json") {
        offset = Channel_Id::Chat_Ai;
    }

    // Create reader at the correct index
    readers[offset].reset(
        new moq::TrackReader(moq::MakeFullTrackName(track_namespace, trackname), serial, codec));
    return readers[offset];
}
```

### Receiving Audio from UI

When audio arrives from the UI chip:

```cpp
// ../hactar/firmware/net/core/src/net.cc:240-264
case ui_net_link::Packet_Type::Message:
{
    uint8_t channel_id = packet->payload[0];
    uint32_t ext_bytes = 1;
    uint32_t length = packet->length - ext_bytes;

    if (channel_id < (uint8_t)ui_net_link::Channel_Id::Count - 1) {
        if (auto& writer = writers[channel_id]) {
            // Push the encrypted payload (minus channel_id) to MoQ
            writers[channel_id]->PushObject(packet->payload + 1, length,
                                            curr_audio_isr_time);
        }
    }
    break;
}
```

**Key observation:** The NET chip does NOT decrypt or parse the payload. It forwards the SFrame-encrypted data directly to MoQ, using the plaintext channel_id (first byte) for routing.

### Publishing to MoQ

The TrackWriter publishes data as MoQ objects. The encrypted chunk (everything after channel_id) is sent as-is.

### Receiving from MoQ

When data arrives on a subscribed track, it's sent to the UI with the appropriate channel_id prepended.

### Dynamic Track Changes (AI Commands)

The AI can send JSON commands to change tracks:

```cpp
// ../hactar/firmware/net/core/src/net.cc:213-238
case ui_net_link::Packet_Type::AiResponse:
{
    uint8_t channel_id = packet->payload[0];
    auto* response = static_cast<ui_net_link::AIResponseChunk*>(
        static_cast<void*>(packet->payload + 1));

    if (!json::accept(response->chunk_data)) {
        break;  // Not valid JSON
    }

    json change_channel = json::parse(response->chunk_data);

    // Create new writer track
    if (auto writer = CreateWriteTrack(change_channel)) {
        writer->Start();
    }

    // Create new reader track
    if (auto reader = CreateReadTrack(change_channel, ui_layer)) {
        reader->Start();
    }
    break;
}
```

## MoQ Namespace Structure

The full track name follows this pattern:

```
moq://moq.ptt.arpa/v1 / org/acme / store/1234 / channel/<name> / ptt
└── Base namespace ──┘ └─ Org ──┘ └─ Store ──┘ └── Channel ───┘ └─┘
                                                                └── Track type
```

Track names use the format: `<codec>_<language>_<samplerate>_<channels>_<format>`
Example: `pcm_en_8khz_mono_i16`

For subscriptions, the trackname is often the remote device_id to receive audio from a specific sender.

## Link Packet Structure

Both chips use a shared packet format for UART communication:

```cpp
// ../hactar/firmware/shared_inc/link_packet_t.hh
struct link_packet_t {
    uint8_t type;                       // Packet_Type enum
    uint16_t length;                    // Payload length
    uint8_t payload[Max_Payload_Size];  // Variable payload
    bool is_ready;                      // Ready to transmit
};
// Max_Payload_Size = 640 bytes (constants::Audio_Phonic_Sz * 4)
```

## Key Constants

```cpp
// ../hactar/firmware/shared_inc/constants.hh
namespace constants {
    static constexpr uint16_t Audio_Sample_Rate_Hz = 8000;
    static constexpr uint16_t Audio_Frame_Length_ms = 20;
    static constexpr uint16_t Audio_Phonic_Sz = 160;  // A-law bytes per frame
    static constexpr uint16_t Audio_Buffer_Sz = 640;  // Stereo PCM samples
    static constexpr bool Stereo = true;
}
```

## Summary of Processing Steps

### Outbound Audio (UI → NET → MoQ)

1. **Capture**: I2S interrupt fires every 20ms, providing 640 bytes stereo PCM
2. **Encode**: `AudioCodec::ALawCompand()` compresses to 160 bytes mono A-law
3. **Format**: `ui_net_link::Serialize()` wraps in Chunk struct with metadata
4. **Encrypt**: `TryProtect()` applies SFrame encryption (preserves channel_id)
5. **Transmit**: `net_serial.Write()` sends to NET chip over UART
6. **Publish**: NET reads channel_id, forwards encrypted payload to MoQ track

### Inbound Audio (MoQ → NET → UI)

1. **Subscribe**: NET receives encrypted object from MoQ
2. **Route**: NET prepends channel_id, sends to UI over UART
3. **Decrypt**: `TryUnprotect()` decrypts SFrame payload
4. **Parse**: `Deserialize()` extracts audio from Chunk struct
5. **Decode**: `AudioCodec::ALawExpand()` expands to stereo PCM
6. **Play**: Audio buffer fed to I2S for speaker output

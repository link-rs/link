# Link TLV Protocol Specification

**Version:** 1.3
**Date:** 2026-02-26

## 1. Overview

The Link TLV (Type-Length-Value) protocol is a binary framing protocol for
inter-chip communication in the Link hardware system. It provides
self-synchronizing message framing over UART connections between four logical
participants:

- **CTL** -- Host controller (laptop/PC connected via USB-serial)
- **MGMT** -- Management MCU (STM32, routes messages between all other chips)
- **UI** -- User interface MCU (STM32, handles audio I/O and encryption)
- **NET** -- Network MCU (ESP32, handles WiFi and MoQ relay connectivity)

The physical links are:

``` 

                +---UART--> UI <--+
                |                 |
                V                 |
CTL <--UART--> MGMT              UART
                ^                 |
                |                 |
                +---UART--> NET <-+
```

MGMT acts as a central hub. CTL communicates with UI and NET by tunneling
messages through MGMT using ToUi/ToNet and FromUi/FromNet TLV types. MGMT
forwards raw UART data, so TLVs may be fragmented across multiple FromUi/FromNet
messages. CTL maintains per-chip stream buffers and parses complete TLVs using
sync word scanning.

## 2. Frame Format

Every TLV frame on the wire has the following structure:

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                     Sync Word (0x4C494E4B)                    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|         Type (u16 BE)         |         Length (u32 BE)       |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          (...Length)          |                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+                               |
|                         Value (0..640 bytes)                  |
|                             ...                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

| Field     | Size    | Encoding   | Description                              |
|-----------|---------|------------|------------------------------------------|
| Sync Word | 4 bytes | Big-endian | Magic value `0x4C494E4B` (ASCII "LINK")  |
| Type      | 2 bytes | Big-endian | Message type code (u16)                  |
| Length    | 4 bytes | Big-endian | Byte length of Value field (0..640)      |
| Value     | 0..640 bytes | Raw    | Message payload                          |

**Constants:**

| Name             | Value  |
|------------------|--------|
| `SYNC_WORD`      | `[0x4C, 0x49, 0x4E, 0x4B]` |
| `HEADER_SIZE`    | 6 (Type + Length)            |
| `MAX_VALUE_SIZE` | 640                          |
| Minimum frame    | 10 bytes (sync + header, empty value) |
| Maximum frame    | 650 bytes (sync + header + 640 value) |

## 3. Sync Word Scanning

The sync word enables receivers to recover framing after non-TLV data such as
bootloader output, as long as non-TLV data isn't interleaved with TLV data. The scanning algorithm is:

```
matched = 0
loop:
    byte = read_one_byte()
    if byte == SYNC_WORD[matched]:
        matched += 1
        if matched == 4:
            // Sync acquired -- read header next
            break
    else:
        matched = 0
        if byte == SYNC_WORD[0]:
            matched = 1
```

After sync is acquired, the receiver reads the 6-byte header, validates the
type and length, then reads `length` bytes of value. Any data before the sync
word is silently discarded.

**Error handling during read:**

| Condition                | Action                              |
|--------------------------|-------------------------------------|
| EOF during sync scan     | Return None (stream ended)          |
| EOF during header read   | Return None                         |
| EOF during value read    | Return None                         |
| Unknown type code        | Return `InvalidType` error          |
| Length > 640             | Return `TooLong` error              |
| I/O error                | Propagate                           |

## 4. Writing

Writers emit the complete frame atomically:

1. Write 4-byte sync word
2. Write 2-byte type (big-endian)
3. Write 4-byte length (big-endian)
4. Write value bytes
5. Flush

A `write_tlv_parts` variant allows writing the value from multiple
non-contiguous slices without intermediate concatenation; the length field is
the sum of all part lengths.

## 5. UART Configuration

| Link        | Baud Rate | Parity | Stop Bits | Flow Control |
|-------------|-----------|--------|-----------|--------------|
| CTL -- MGMT | 1000000   | Even   | 1         | None         |
| MGMT -- UI  | 1000000   | Even   | 1         | None         |
| MGMT -- NET | 1000000   | None   | 1         | None         |
| UI -- NET   | 1000000   | Even   | 1         | None         |

The MGMT--UI baud rate is configurable at runtime via `SetUiBaudRate`.
For STM32 bootloader flashing, the MGMT--UI link is temporarily switched
to 115200/8E1 (the STM32 bootloader's fixed configuration).

The CTL--MGMT and MGMT--UI links use even parity for compatibility with
the STM32 bootloader (115200/8E1), allowing the same physical connection
to be used for both TLV communication and bootloader flashing. The
MGMT--NET link uses no parity to match the ESP32 bootloader and user
firmware.

## 6. Message Types

Each communication direction has its own type enumeration. Types are assigned
as `u16` values using the base offsets below, with sequential numbering from
the base.

### 6.1 CtlToMgmt (0x0X)

Messages from CTL host to MGMT chip.

| Value | Name           | Payload                    | Expected Response (MgmtToCtl) |
|-------|----------------|----------------------------|-------------------------------|
| 0x00  | Ping           | Arbitrary echo data        | Pong                          |
| 0x01  | ToUi           | Tunneled TLV frame(s)      | (forwarded to UI)             |
| 0x02  | ToNet          | Tunneled TLV frame(s)      | (forwarded to NET)            |
| 0x03  | Hello          | 4 bytes (challenge)        | Hello                         |
| 0x04  | SetPin         | 2 bytes (pin, value)       | Ack                           |
| 0x05  | SetUiBaudRate  | 4 bytes (u32 BE baud rate) | Ack                           |
| 0x06  | GetStackInfo   | (empty)                    | StackInfo                     |
| 0x07  | RepaintStack   | (empty)                    | Ack                           |

### 6.2 MgmtToCtl (0x1X)

Messages from MGMT chip to CTL host.

| Value | Name     | Payload                          | Trigger                   |
|-------|----------|----------------------------------|---------------------------|
| 0x10  | Pong     | Echo of Ping data                | Response to Ping          |
| 0x11  | FromUi   | Tunneled byte stream from UI     | UI chip sent data         |
| 0x12  | FromNet  | Tunneled byte stream from NET    | NET chip sent data        |
| 0x13  | Ack      | (empty)                          | Acknowledgment of command |
| 0x14  | Hello    | 4 bytes (challenge XOR b"LINK")  | Response to Hello         |
| 0x15  | StackInfo| postcard-serialized StackInfo    | Response to GetStackInfo  |

### 6.3 CtlToUi (0x2X)

Messages from CTL to UI chip (tunneled through MGMT).

| Value | Name         | Payload                  | Expected Response (UiToCtl) |
|-------|--------------|--------------------------|------------------------------|
| 0x20  | Ping         | Arbitrary echo data      | Pong                         |
| 0x21  | CircularPing | Arbitrary echo data      | (forwarded to NET)           |
| 0x22  | GetVersion   | (empty)                  | Version                      |
| 0x23  | SetVersion   | 4 bytes (u32 BE version) | Ack                          |
| 0x24  | GetSFrameKey | (empty)                  | SFrameKey                    |
| 0x25  | SetSFrameKey | 16 bytes (AES-128 key)   | Ack                          |
| 0x26  | SetLoopback  | 1 byte (UiLoopbackMode)  | Ack                          |
| 0x27  | GetLoopback  | (empty)                  | Loopback                     |
| 0x28  | GetStackInfo | (empty)                  | StackInfo                    |
| 0x29  | RepaintStack | (empty)                  | Ack                          |

### 6.4 UiToCtl (0x3X)

Messages from UI chip to CTL (forwarded through MGMT via MgmtToCtl::FromUi).

| Value | Name         | Payload                       | Trigger                             |
|-------|--------------|-------------------------------|-------------------------------------|
| 0x30  | Pong         | Echo of Ping data             | Response to Ping                    |
| 0x31  | CircularPing | Echo data (forwarded)         | Forwarded from NET via direct link  |
| 0x32  | Version      | 4 bytes (u32 BE version)      | Response to GetVersion              |
| 0x33  | SFrameKey    | 16 bytes (AES-128 key)        | Response to GetSFrameKey            |
| 0x34  | Ack          | (empty)                       | Acknowledgment                      |
| 0x35  | Error        | UTF-8 error message           | Error response to any command       |
| 0x36  | Loopback     | 1 byte (UiLoopbackMode)       | Response to GetLoopback             |
| 0x37  | Log          | UTF-8 log text                | Unsolicited debug log               |
| 0x38  | StackInfo    | postcard-serialized StackInfo | Response to GetStackInfo            |

### 6.5 CtlToNet (0x4X)

Messages from CTL to NET chip (tunneled through MGMT).

| Value | Name               | Payload                  | Expected Response (NetToCtl) |
|-------|--------------------|--------------------------|------------------------------|
| 0x40  | Ping               | Arbitrary echo data      | Pong                         |
| 0x41  | CircularPing       | Arbitrary echo data      | (forwarded to UI)            |
| 0x42  | AddWifiSsid        | postcard(WifiSsid)       | Ack                          |
| 0x43  | GetWifiSsids       | (empty)                  | WifiSsids                    |
| 0x44  | ClearWifiSsids     | (empty)                  | Ack                          |
| 0x45  | GetRelayUrl        | (empty)                  | RelayUrl                     |
| 0x46  | SetRelayUrl        | UTF-8 URL string         | Ack                          |
| 0x47  | SetLoopback        | 1 byte (NetLoopbackMode) | Ack                          |
| 0x48  | GetLoopback        | (empty)                  | Loopback                     |
| 0x49  | GetJitterStats     | 1 byte (channel_id)      | JitterStats                  |
| 0x4A  | SetConfigUrl       | UTF-8 URL string         | Ack                          |
| 0x4B  | SetAccessToken     | UTF-8 token string       | Ack                          |
| 0x4C  | SetRefreshToken    | UTF-8 token string       | Ack                          |
| 0x4D  | GetLanguage        | (empty)                  | Language                     |
| 0x4E  | SetLanguage        | UTF-8 language code      | Ack                          |
| 0x4F  | GetChannel         | (empty)                  | ChannelInfo                  |
| 0x50  | SetChannel         | UTF-8 display name       | Ack                          |
| 0x51  | SetTokenUrl        | UTF-8 URL string         | Ack                          |

### 6.6 NetToCtl (0x5X)

Messages from NET chip to CTL (forwarded through MGMT via MgmtToCtl::FromNet).

| Value | Name          | Payload                             | Trigger                     |
|-------|---------------|-------------------------------------|-----------------------------|
| 0x50  | Pong          | Echo of Ping data                   | Response to Ping            |
| 0x51  | CircularPing  | Echo data (forwarded)               | Forwarded from UI           |
| 0x52  | WifiSsids     | postcard(Vec\<WifiSsid\>)           | Response to GetWifiSsids    |
| 0x53  | RelayUrl      | UTF-8 URL string                    | Response to GetRelayUrl     |
| 0x54  | Ack           | (empty)                             | Acknowledgment              |
| 0x55  | Error         | UTF-8 error message                 | Error response              |
| 0x56  | Loopback      | 1 byte (NetLoopbackMode)            | Response to GetLoopback     |
| 0x57  | JitterStats   | postcard-serialized JitterStatsInfo | Response to GetJitterStats  |
| 0x58  | Language      | UTF-8 language code                 | Response to GetLanguage     |
| 0x59  | ChannelInfo   | UTF-8 JSON {"id":"...","display_name":"..."} | Response to GetChannel |

### 6.7 UiToNet (0x6X)

Messages from UI to NET over the direct audio link.

| Value | Name         | Payload                            | Notes      |
|-------|--------------|------------------------------------|------------|
| 0x60  | CircularPing | Arbitrary echo data                | Test/debug |
| 0x61  | AudioFrame   | 1 byte channel_id + SFrame ciphertext | Audio   |

### 6.8 NetToUi (0x7X)

Messages from NET to UI over the direct audio link.

| Value | Name           | Payload                                         | Notes                    |
|-------|----------------|-------------------------------------------------|--------------------------|
| 0x70  | CircularPing   | Arbitrary echo data                              | Test/debug               |
| 0x71  | AudioFrame     | 1 byte channel_id + SFrame ciphertext            | Audio for playback       |

## 7. Payload Formats

### 7.1 Primitive Types

| Type        | Encoding      | Notes                       |
|-------------|---------------|-----------------------------|
| u8          | 1 byte        |                             |
| u16 BE      | 2 bytes       | Big-endian                  |
| u32 BE      | 4 bytes       | Big-endian                  |
| UTF-8 string| Variable      | No length prefix; length is the TLV Length field |

### 7.2 postcard Serialization

Structured payloads use the **postcard** serialization format (a compact
variable-length encoding for Serde-serializable Rust types). Postcard uses
varint encoding for lengths and enum discriminants.

The following types are postcard-serialized:

**WifiSsid:**
```rust
struct WifiSsid {
    ssid: String,      // max 32 bytes
    password: String,   // max 63 bytes
}
```


### 7.3 Hello Handshake

The Hello exchange verifies that a valid Link device is connected.

1. CTL sends `CtlToMgmt::Hello` with a 4-byte random challenge.
2. MGMT XORs each byte with the corresponding byte of `b"LINK"` and replies
   with `MgmtToCtl::Hello` containing the 4-byte result.
3. CTL verifies: `response[i] == challenge[i] ^ b"LINK"[i]` for i in 0..4.

During the Hello exchange, CTL may receive non-Hello TLVs (e.g., NET boot
spam forwarded as FromNet). CTL skips up to 1024 non-Hello TLVs before
giving up.

### 7.4 Ping/Pong

The Ping/Pong exchange verifies connectivity and data integrity on any link.

1. Sender sends Ping with arbitrary data.
2. Receiver echoes the identical data back as Pong.
3. Sender verifies `pong.value == ping.value`.

### 7.5 StackInfo

postcard-serialized `StackInfo` struct:

```rust
#[derive(Serialize, Deserialize)]
struct StackInfo {
    stack_base: u32,
    stack_top: u32,
    stack_size: u32,
    stack_used: u32,
}
```

`stack_free = stack_size - stack_used`

### 7.6 JitterStats

postcard-serialized `JitterStatsInfo` struct:

```rust
#[derive(Serialize, Deserialize)]
struct JitterStatsInfo {
    received: u32,
    output: u32,
    underruns: u32,
    overruns: u32,
    level: u16,
    state: u8,  // 0=Buffering, 1=Playing
}
```

### 7.7 Version

4 bytes, u32 big-endian. Stored in UI chip EEPROM. Semantics are
application-defined.

### 7.8 SFrame Key

16 bytes. The raw AES-128 epoch secret used for SFrame (RFC 9605) audio
encryption. Stored in UI chip EEPROM.

### 7.9 Baud Rate

4 bytes, u32 big-endian. The new baud rate in bits per second.

`SetUiBaudRate` changes the MGMT--UI link unilaterally (MGMT changes both
TX and RX). Used during UI chip flashing to switch to STM32 bootloader
speed (115200) and back.

### 7.10 Pin Control (SetPin)

2 bytes: `[pin_id: u8][value: u8]`

**Pin ID (byte 0):**
| Value | Pin        | Description                |
|-------|------------|----------------------------|
| 0     | UiBoot0    | UI chip BOOT0 pin          |
| 1     | UiBoot1    | UI chip BOOT1 pin          |
| 2     | UiRst      | UI chip RST pin            |
| 3     | NetBoot    | NET chip GPIO0/BOOT pin    |
| 4     | NetRst     | NET chip EN/RST pin        |

**Pin Value (byte 1):**
| Value | Name | Description                                        |
|-------|------|----------------------------------------------------|
| 0     | Low  | For BOOT: normal mode; for RST: hold in reset     |
| 1     | High | For BOOT: bootloader mode; for RST: run           |

Response: Ack

## 8. Enumerated Value Types

### 8.1 ChannelId (u8)

| Value | Name    | Description                              |
|-------|---------|------------------------------------------|
| 0     | Ptt     | Push-to-talk human voice (button A)      |
| 1     | PttAi   | AI audio channel (button B)              |
| 2     | (reserved) | Chat (not implemented)                |
| 3     | ChatAi  | AI text/JSON responses                   |

### 8.2 MessageType (u8) -- Chunk Layer

| Value | Name       | Description               |
|-------|------------|---------------------------|
| 1     | Media      | Regular audio data        |
| 2     | AiRequest  | Audio to AI               |
| 3     | AiResponse | Response from AI          |

### 8.3 UiLoopbackMode (u8) -- UI Chip

| Value | Name   | Description                                               |
|-------|--------|-----------------------------------------------------------|
| 0     | Off    | Normal operation; audio sent to NET                       |
| 1     | Raw    | Loopback before A-law encoding (stereo PCM to speaker)    |
| 2     | Alaw   | Loopback after A-law encode/decode (no encryption)        |
| 3     | Sframe | Full round-trip: encode, encrypt, decrypt, decode         |

### 8.4 NetLoopbackMode (u8) -- NET Chip

| Value | Name | Description                                                 |
|-------|------|-------------------------------------------------------------|
| 0     | Off  | Normal operation; audio to MoQ relay, filter self-echo      |
| 1     | Raw  | Local bypass; audio from UI sent directly back to UI        |
| 2     | Moq  | Audio to MoQ relay, do NOT filter self-echo (hear own voice)|

### 8.5 Pin (u8) -- Pin Identifiers

| Value | Name     | Description             |
|-------|----------|-------------------------|
| 0     | UiBoot0  | UI chip BOOT0 pin       |
| 1     | UiBoot1  | UI chip BOOT1 pin       |
| 2     | UiRst    | UI chip RST pin         |
| 3     | NetBoot  | NET chip GPIO0/BOOT pin |
| 4     | NetRst   | NET chip EN/RST pin     |

### 8.6 PinValue (u8) -- Pin Levels

| Value | Name | Description                                        |
|-------|------|----------------------------------------------------|
| 0     | Low  | For BOOT: normal mode; for RST: hold in reset     |
| 1     | High | For BOOT: bootloader mode; for RST: run           |

## 9. Audio Frame Format

Audio frames travel over the direct UI--NET link using TLV types
`UiToNet::AudioFrame` (0x61) and `NetToUi::AudioFrame` (0x71).

### 9.1 TLV Value Structure

```
+------------+---------------------------------------------------+
| channel_id | SFrame ciphertext                                 |
| (1 byte)   | (variable)                                        |
+------------+---------------------------------------------------+
```

The SFrame ciphertext is an RFC 9605 frame using the AES_128_GCM_SHA256_128
cipher suite (ID 0x0004). It consists of:

```
+----------------+--------------------+----------+
| SFrame header  | Encrypted payload  | Auth tag |
| (1..17 bytes)  | (variable)         | (16 bytes)|
+----------------+--------------------+----------+
```

The SFrame header encodes a Key ID (KID) and Counter (CTR) using a compact
variable-length encoding defined in RFC 9605 Section 4.3.

The encrypted payload, before encryption, contains a **chunk** with a
message-type-specific header:

**Media chunk** (MessageType::Media = 1):

| Offset | Size | Field        | Encoding | Description         |
|--------|------|--------------|----------|---------------------|
| 0      | 1    | type         | u8       | 1 (Media)           |
| 1      | 1    | last_chunk   | u8       | 0=more, 1=last      |
| 2      | 4    | chunk_length | u32 LE   | Audio data length    |
| 6      | N    | audio_data   | raw      | A-law encoded audio  |

**AiRequest chunk** (MessageType::AiRequest = 2):

| Offset | Size | Field        | Encoding | Description         |
|--------|------|--------------|----------|---------------------|
| 0      | 1    | type         | u8       | 2 (AiRequest)       |
| 1      | 4    | request_id   | u32 LE   | Request identifier   |
| 5      | 1    | last_chunk   | u8       | 0=more, 1=last      |
| 6      | 4    | chunk_length | u32 LE   | Audio data length    |
| 10     | N    | audio_data   | raw      | A-law encoded audio  |

**AiResponse chunk** (MessageType::AiResponse = 3):

| Offset | Size | Field        | Encoding | Description         |
|--------|------|--------------|----------|---------------------|
| 0      | 1    | type         | u8       | 3 (AiResponse)      |
| 1      | 4    | request_id   | u32 LE   | Request identifier   |
| 5      | 1    | content_type | u8       | Content type code    |
| 6      | 1    | last_chunk   | u8       | 0=more, 1=last      |
| 7      | 4    | chunk_length | u32 LE   | Audio data length    |
| 11     | N    | audio_data   | raw      | Audio/content data   |

## 10. Circular Ping

The circular ping tests the full ring of connections: CTL -> MGMT -> chip A ->
chip B -> MGMT -> CTL.

**UI-first path:** CTL sends `CtlToUi::CircularPing` (tunneled via ToUi).
UI forwards to NET via the direct UI--NET link. NET sends
`NetToCtl::CircularPing` to MGMT, which forwards to CTL as FromNet.

**NET-first path:** CTL sends `CtlToNet::CircularPing` (tunneled via ToNet).
NET forwards to UI via the direct NET--UI link. UI sends
`UiToCtl::CircularPing` to MGMT, which forwards to CTL as FromUi.

CTL verifies the payload data matches what was originally sent.

## 11. Reset Sequences

### 11.1 Reset to Bootloader (UI)

1. CTL sends `SetPin(UiBoot0, High)` -- assert BOOT0 high
2. CTL sends `SetPin(UiRst, Low)` -- hold RST low
3. CTL waits ~10ms
4. CTL sends `SetPin(UiRst, High)` -- release RST
5. UI enters STM32 system bootloader
6. CTL can now flash firmware using the STM32 bootloader protocol over the
   same UART (MGMT--UI link, 115200/8E1)
7. After flashing, CTL sends `SetPin(UiBoot0, Low)` then `SetPin(UiRst, Low)`, waits ~10ms,
   then `SetPin(UiRst, High)` to boot into user mode

### 11.2 Reset to Bootloader (NET)

1. CTL sends `SetPin(NetBoot, High)` -- assert GPIO0 high
2. CTL sends `SetPin(NetRst, Low)` -- hold EN/RST low
3. CTL waits ~10ms
4. CTL sends `SetPin(NetRst, High)` -- release EN/RST
5. NET (ESP32) enters ROM bootloader
6. CTL can now flash firmware using espflash/esptool protocol over the
   MGMT--NET UART
7. After flashing, CTL sends `SetPin(NetBoot, Low)` then `SetPin(NetRst, Low)`, waits ~10ms,
   then `SetPin(NetRst, High)` to boot into user firmware

### 11.3 Direct Pin Control

For advanced flashing scenarios, CTL can control pins individually using `SetPin`:

- `SetPin(UiBoot0, Low/High)` -- directly set UI BOOT0 pin level
- `SetPin(UiBoot1, Low/High)` -- directly set UI BOOT1 pin level
- `SetPin(UiRst, Low/High)` -- directly set UI RST pin level
- `SetPin(NetBoot, Low/High)` -- directly set NET GPIO0/BOOT pin level
- `SetPin(NetRst, Low/High)` -- directly set NET EN/RST pin level

SetPin commands receive an Ack response from MGMT.

## 12. Unsolicited Messages

Some messages are sent by chips without a preceding request:

| Message        | Source | Description                |
|----------------|--------|----------------------------|
| UiToCtl::Log   | UI     | Debug log output (UTF-8)   |

CTL must be prepared to receive these at any time. When waiting for a specific
response, CTL should:
- Buffer FromUi/FromNet data for later parsing
- Skip Log messages when waiting for UI command responses

## 13. Error Responses

Both UI and NET can respond with an Error TLV instead of the expected response.
The value is a UTF-8 error message describing the failure. CTL should treat
this as a device error and may display or log the message.

| Direction | Type Value | Name  |
|-----------|-----------|-------|
| UiToCtl   | 0x35      | Error |
| NetToCtl  | 0x55      | Error |

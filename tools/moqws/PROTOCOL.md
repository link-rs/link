# MOQWS Protocol Specification

MOQWS is a WebSocket-based protocol that provides access to MOQT (Media over QUIC Transport) relay servers. Each WebSocket connection creates an independent MOQT client connection, allowing web browsers and other WebSocket clients to publish and subscribe to MOQT tracks.

## Overview

```
┌─────────────────┐         ┌─────────────────┐         ┌─────────────────┐
│  WebSocket      │  MOQWS  │   MOQWS Bridge  │  MOQT   │   MOQT Relay    │
│  Client (JS)    │◀───────▶│   Server        │◀───────▶│   Server        │
└─────────────────┘         └─────────────────┘         └─────────────────┘
```

### Design Principles

1. **One WS connection = One MOQT client**: Each WebSocket connection maintains its own MOQT client connection to a relay
2. **JSON control + binary data**: Control messages are JSON text frames; payloads are binary frames
3. **ID-based multiplexing**: Multiple subscriptions and publish tracks per connection, identified by numeric IDs
4. **Minimal translation**: Direct mapping from MOQT concepts with minimal abstraction

## Message Format

### Frame Types

- **Text frames**: JSON control messages
- **Binary frames**: Object payloads (always immediately follow a control message that sets `payload_follows: true`)

### Message Structure

All control messages have a `type` field identifying the message type:

```typescript
interface Message {
  type: string;
  // ... type-specific fields
}
```

## Client → Server Messages

### `connect`

Establish a MOQT connection to a relay server.

```json
{
  "type": "connect",
  "relay_url": "moqt://relay.example.com:4433",
  "endpoint_id": "my-client-123"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `relay_url` | string | yes | MOQT relay URL (e.g., `moqt://host:port`) |
| `endpoint_id` | string | no | Client identifier for the MOQT session (default: auto-generated) |

**MOQT Mapping**: Creates a `quicr::Client` and calls `client.connect()`.

---

### `disconnect`

Disconnect from the MOQT relay.

```json
{
  "type": "disconnect"
}
```

**MOQT Mapping**: Calls `client.disconnect()`.

---

### `subscribe`

Subscribe to a MOQT track to receive objects.

```json
{
  "type": "subscribe",
  "id": 1,
  "namespace": ["moq://moq.ptt.arpa/v1", "org/acme", "store/1234", "channel/gardening", "ptt"],
  "track": "pcm_en_8khz_mono_i16"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | number | yes | Client-assigned subscription ID (for multiplexing) |
| `namespace` | string[] | yes | Track namespace as array of segments |
| `track` | string | yes | Track name |

**MOQT Mapping**:
- Creates a `FullTrackName` from `(namespace, track)`
- Calls `client.subscribe(track_name)` to create a `Subscription`
- Stores subscription keyed by `id`

---

### `unsubscribe`

Cancel an active subscription.

```json
{
  "type": "unsubscribe",
  "id": 1
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | number | yes | Subscription ID to cancel |

**MOQT Mapping**: Drops the `Subscription` associated with the ID.

---

### `publish_announce`

Announce intent to publish to a track. Must be called before publishing objects.

```json
{
  "type": "publish_announce",
  "id": 1,
  "namespace": ["moq://moq.ptt.arpa/v1", "org/acme", "store/1234", "channel/gardening", "ptt"],
  "track": "pcm_en_8khz_mono_i16",
  "track_mode": "datagram",
  "priority": 0,
  "ttl": 1000
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | number | yes | Client-assigned publish track ID |
| `namespace` | string[] | yes | Track namespace as array of segments |
| `track` | string | yes | Track name |
| `track_mode` | string | no | `"datagram"` (default, low-latency) or `"stream"` (reliable) |
| `priority` | number | no | Default priority for objects (0-255, default: 0) |
| `ttl` | number | no | Default TTL in milliseconds (default: 1000) |

**MOQT Mapping**:
- Creates a `FullTrackName` from `(namespace, track)`
- Creates a `PublishTrack` via `PublishTrackBuilder`
- Calls `client.publish_track(track)` to register with relay
- Stores publish track keyed by `id`

---

### `publish`

Publish an object to an announced track. Must be immediately followed by a binary frame containing the payload.

```json
{
  "type": "publish",
  "id": 1,
  "group_id": 12345678,
  "object_id": 42
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | number | yes | Publish track ID (from `publish_announce`) |
| `group_id` | number | yes | Object group ID (e.g., source device ID) |
| `object_id` | number | yes | Object sequence number within group |
| `priority` | number | no | Override priority for this object |
| `ttl` | number | no | Override TTL for this object |

**Binary Frame**: The next WebSocket frame MUST be a binary frame containing the object payload.

**MOQT Mapping**:
- Creates `ObjectHeaders` with `(group_id, object_id)`
- Calls `track.publish(&headers, payload)` with the binary frame data

---

### `publish_end`

Stop publishing to a track.

```json
{
  "type": "publish_end",
  "id": 1
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | number | yes | Publish track ID to stop |

**MOQT Mapping**: Calls `client.unpublish_track(track)` and removes the track.

---

## Server → Client Messages

### `connected`

Confirms successful MOQT relay connection.

```json
{
  "type": "connected",
  "moqt_version": 1,
  "server_id": "moq-relay-1.0"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `moqt_version` | number | MOQT protocol version negotiated with relay |
| `server_id` | string | Relay server identifier |

---

### `disconnected`

Indicates the MOQT connection was closed.

```json
{
  "type": "disconnected",
  "reason": "idle_timeout"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `reason` | string | Reason for disconnection |

---

### `error`

Reports an error.

```json
{
  "type": "error",
  "code": "not_connected",
  "message": "Must connect before subscribing"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `code` | string | Error code (see Error Codes section) |
| `message` | string | Human-readable error description |
| `id` | number | Optional: subscription/publish ID if relevant |

---

### `subscribed`

Confirms a subscription is active and receiving objects.

```json
{
  "type": "subscribed",
  "id": 1
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | number | Subscription ID that is now active |

---

### `subscribe_error`

Reports a subscription failure.

```json
{
  "type": "subscribe_error",
  "id": 1,
  "code": "not_authorized",
  "message": "Track subscription not authorized"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | number | Subscription ID that failed |
| `code` | string | Error code |
| `message` | string | Human-readable error description |

---

### `subscription_ended`

Indicates a subscription has ended (track finished or error).

```json
{
  "type": "subscription_ended",
  "id": 1,
  "reason": "done_by_fin"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | number | Subscription ID that ended |
| `reason` | string | Reason: `done_by_fin`, `done_by_reset`, `cancelled`, `error` |

---

### `published`

Confirms a publish track is ready to accept objects.

```json
{
  "type": "published",
  "id": 1
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | number | Publish track ID that is ready |

---

### `publish_error`

Reports a publish track failure.

```json
{
  "type": "publish_error",
  "id": 1,
  "code": "announce_not_authorized",
  "message": "Track announce not authorized"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | number | Publish track ID that failed |
| `code` | string | Error code |
| `message` | string | Human-readable error description |

---

### `object`

Delivers a received object from a subscription. Immediately followed by a binary frame containing the payload.

```json
{
  "type": "object",
  "id": 1,
  "group_id": 12345678,
  "object_id": 42,
  "payload_length": 640
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | number | Subscription ID this object belongs to |
| `group_id` | number | Object group ID |
| `object_id` | number | Object sequence number within group |
| `payload_length` | number | Length of the following binary frame |

**Binary Frame**: The next WebSocket frame will be a binary frame containing the object payload.

---

## Error Codes

| Code | Description |
|------|-------------|
| `not_connected` | Operation requires an active MOQT connection |
| `already_connected` | Already connected to a relay |
| `connection_failed` | Failed to connect to MOQT relay |
| `invalid_message` | Malformed or invalid message |
| `unknown_id` | Unknown subscription or publish track ID |
| `duplicate_id` | ID already in use |
| `not_authorized` | Operation not authorized |
| `announce_not_authorized` | Track announce not authorized |
| `internal_error` | Internal server error |

---

## MOQT Concepts Mapping

### Track Naming

MOQT tracks are identified by a **namespace** (an N-tuple of byte strings) and a **track name** (a single byte string).

In MOQWS, these are represented as:
- `namespace`: JSON array of strings (each element is one namespace tuple entry)
- `track`: Single string

Example:
```json
{
  "namespace": ["moq://moq.ptt.arpa/v1", "org/acme", "store/1234", "channel/gardening", "ptt"],
  "track": "pcm_en_8khz_mono_i16"
}
```

This maps to:
```rust
let ns = TrackNamespace::from_strings(&[
    "moq://moq.ptt.arpa/v1",
    "org/acme",
    "store/1234",
    "channel/gardening",
    "ptt"
]);
let track_name = FullTrackName::new(ns, "pcm_en_8khz_mono_i16".as_bytes());
```

### Object Model

MOQT objects are identified within a track by:
- **group_id**: Identifies the source or generation (e.g., device MAC address)
- **object_id**: Sequence number within the group

In PTT audio applications:
- `group_id` = device identifier (allows filtering self-echo)
- `object_id` = incrementing frame counter

### Track Modes

| Mode | MOQT Behavior | Use Case |
|------|---------------|----------|
| `datagram` | Unreliable, low-latency QUIC datagrams | Real-time audio/video |
| `stream` | Reliable, ordered QUIC streams | Chat messages, metadata |

---

## Example Session

```
Client                                Server
  │                                      │
  │──── connect ─────────────────────────▶│  (creates MOQT client)
  │◀──── connected ──────────────────────│
  │                                      │
  │──── subscribe {id:1, ...} ───────────▶│  (creates subscription)
  │◀──── subscribed {id:1} ──────────────│
  │                                      │
  │◀──── object {id:1, group:X, obj:0} ──│  (received from MOQT)
  │◀──── [binary: payload] ──────────────│
  │                                      │
  │◀──── object {id:1, group:X, obj:1} ──│
  │◀──── [binary: payload] ──────────────│
  │                                      │
  │──── publish_announce {id:2, ...} ────▶│  (creates publish track)
  │◀──── published {id:2} ───────────────│
  │                                      │
  │──── publish {id:2, group:Y, obj:0} ──▶│
  │──── [binary: payload] ───────────────▶│  (publishes to MOQT)
  │                                      │
  │──── unsubscribe {id:1} ──────────────▶│
  │──── publish_end {id:2} ──────────────▶│
  │──── disconnect ──────────────────────▶│
  │◀──── disconnected ───────────────────│
  │                                      │
```

---

## Wire Format Examples

### Subscribing to a Track

1. Client sends text frame:
```json
{"type":"subscribe","id":1,"namespace":["audio","room1"],"track":"mic"}
```

2. Server sends text frame (on success):
```json
{"type":"subscribed","id":1}
```

### Receiving an Object

1. Server sends text frame:
```json
{"type":"object","id":1,"group_id":12345,"object_id":7,"payload_length":640}
```

2. Server sends binary frame:
```
<640 bytes of audio data>
```

### Publishing an Object

1. Client sends text frame:
```json
{"type":"publish","id":2,"group_id":54321,"object_id":0}
```

2. Client sends binary frame:
```
<audio payload bytes>
```

---

## Security Considerations

1. **TLS**: The underlying MOQT connection uses QUIC with TLS. The WebSocket connection should also use WSS (WebSocket Secure) in production.

2. **Authentication**: MOQWS itself does not add authentication. Use:
   - WebSocket authentication (bearer tokens, cookies)
   - MOQT relay authentication (configured in relay URL or relay-side)

3. **Origin Validation**: The server should validate WebSocket origins in production.

4. **Rate Limiting**: Consider rate limiting connections and messages per connection.

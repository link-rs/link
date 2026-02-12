# moqws - WebSocket-to-MOQT Bridge

A WebSocket server that bridges web clients to MOQT (Media over QUIC Transport) relay servers.

## Overview

`moqws` allows web browsers and other WebSocket clients to access MOQT functionality. Each WebSocket connection creates an independent MOQT client, enabling web applications to publish and subscribe to MOQT tracks.

```
┌─────────────────┐         ┌─────────────────┐         ┌─────────────────┐
│  Web Browser    │  WS     │   moqws         │  MOQT   │   MOQT Relay    │
│  (JavaScript)   │◀───────▶│   Bridge        │◀───────▶│   Server        │
└─────────────────┘         └─────────────────┘         └─────────────────┘
```

## Usage

### Running the Server

```bash
# Default: listen on 127.0.0.1:8765
cargo run --release

# Custom bind address
cargo run --release -- --bind 0.0.0.0:8080

# With debug logging
RUST_LOG=debug cargo run -- --log-level debug
```

### JavaScript Client

Include the client library and connect:

```javascript
// Use MoqwsSimpleClient for callback-based API
const client = new MoqwsSimpleClient('ws://localhost:8765');

// Connect to WebSocket server
await client.connect();

// Connect to MOQT relay
await client.connectToRelay('moqt://relay.example.com:4433');

// Subscribe to a track
client.on('object', ({ id, group_id, object_id, payload }) => {
    console.log(`Received: group=${group_id}, obj=${object_id}, size=${payload.byteLength}`);
});
await client.subscribe(1, ['audio', 'room1'], 'mic');

// Publish to a track
await client.publishAnnounce(2, ['audio', 'room1'], 'mic');
client.publish(2, myGroupId, objectId++, audioBuffer);
```

Or use `MoqwsClient` directly with `addEventListener`:

```javascript
const client = new MoqwsClient('ws://localhost:8765');
client.addEventListener('object', (e) => {
    const { group_id, object_id, payload } = e.detail;
    // ...
});
```

### Example HTML

Open `client/example.html` in a browser for an interactive demo.

## Protocol

See [PROTOCOL.md](PROTOCOL.md) for the complete protocol specification.

### Quick Reference

**Client → Server:**
- `connect` - Connect to MOQT relay
- `disconnect` - Disconnect from relay
- `subscribe` - Subscribe to a track
- `unsubscribe` - Cancel subscription
- `publish_announce` - Announce publish track
- `publish` - Publish an object (+ binary frame)
- `publish_end` - Stop publishing

**Server → Client:**
- `connected` - Relay connection established
- `disconnected` - Relay disconnected
- `error` - Error occurred
- `subscribed` - Subscription active
- `subscribe_error` - Subscription failed
- `subscription_ended` - Track ended
- `published` - Publish track ready
- `publish_error` - Publish failed
- `object` - Received object (+ binary frame)

## Files

```
moqws/
├── Cargo.toml          # Rust dependencies
├── README.md           # This file
├── PROTOCOL.md         # Protocol specification
├── src/
│   └── main.rs         # Server implementation
└── client/
    ├── moqws.js        # JavaScript client library
    ├── moqws.d.ts      # TypeScript type definitions
    └── example.html    # Interactive demo
```

## Architecture

The server uses two async runtimes:
- **Tokio** - Handles WebSocket connections
- **Embassy** - Runs the MOQT client (via quicr FFI)

Each WebSocket connection spawns a dedicated thread running an Embassy executor for its MOQT client. Communication between the WebSocket handler and MOQT thread uses channels.

## Building

```bash
cd tools/moqws
cargo build --release
```

Requires the `quicr` crate which depends on libquicr (C++ library with QUIC/MOQT implementation).

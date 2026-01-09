# quicr.rs Examples

This directory contains example applications demonstrating how to use the quicr.rs library.

## Prerequisites

Before running any example, you need to start a relay server using libquicr's qServer:

```bash
cd vendor/libquicr
make
./build/cmd/examples/qServer -p 4433
```

## Examples

### Clock Example (`pubsub`)

Demonstrates basic publish/subscribe functionality where a publisher sends clock timestamps and subscribers receive them.

**Publisher** (sends timestamps every second):
```bash
cargo run --example pubsub -- --mode publish
```

**Subscriber** (receives and displays timestamps):
```bash
cargo run --example pubsub -- --mode subscribe
```

#### Options

| Option | Short | Description | Default |
|--------|-------|-------------|---------|
| `--relay` | `-r` | Relay URI | `moqt://localhost:4433` |
| `--namespace` | `-n` | Track namespace | `clock/demo` |
| `--track-name` | `-t` | Track name | `timestamps` |
| `--mode` | `-m` | `publish` or `subscribe` | `publish` |
| `--endpoint-id` | `-e` | Client identifier | `pubsub-example` |
| `--interval` | `-i` | Publish interval (ms) | `1000` |

#### Custom Usage

```bash
# Publish every 500ms to a custom track
cargo run --example pubsub -- -m pub -n "myapp/data" -t sensor1 -i 500

# Subscribe to that custom track
cargo run --example pubsub -- -m sub -n "myapp/data" -t sensor1
```

---

### Chat Example (`chat`)

A simple pub/sub chat application where multiple users can send and receive messages in a room.

**Subscribe to messages:**
```bash
cargo run --example chat -- --mode subscribe --room myroom
```

**Publish messages:**
```bash
cargo run --example chat -- --mode publish --room myroom --user alice
```

**Both mode (send and receive):**
```bash
cargo run --example chat -- --mode both --room myroom --user alice
```

---

### Relay Example (`relay`)

Demonstrates running a simple relay server.

```bash
cargo run --example relay -- --help
```

---

## Logging

All examples support logging via the `RUST_LOG` environment variable. The library uses the standard Rust `log` crate.

### Log Levels

| Level | Description |
|-------|-------------|
| `error` | Critical failures (connection failures, FFI errors) |
| `warn` | Non-critical issues (buffer full, null callbacks) |
| `info` | Important events (connection established, tracks registered) |
| `debug` | Detailed operational info (status changes, config details) |
| `trace` | Very detailed info (FFI calls, individual object pub/sub) |

### Usage Examples

```bash
# Show only errors
RUST_LOG=error cargo run --example pubsub

# Show info level and above (recommended for normal operation)
RUST_LOG=info cargo run --example pubsub

# Show debug level for the quicr crate only
RUST_LOG=quicr=debug cargo run --example pubsub

# Show all trace-level logs (very verbose)
RUST_LOG=trace cargo run --example pubsub

# Combine multiple filters
RUST_LOG=quicr=debug,tokio=warn cargo run --example chat

# Debug a specific module
RUST_LOG=quicr::client=debug cargo run --example pubsub
```

### Example Output

With `RUST_LOG=quicr=info`:

```
[INFO  quicr::client] Creating new client: endpoint_id=pubsub-example, uri=moqt://localhost:4433
[INFO  quicr::client] Client created successfully
[INFO  quicr::client] Connecting to relay (timeout: 30s)
[INFO  quicr::client] Connection established successfully
[INFO  quicr::client] Publish track registered: FullTrackName { ... }
```

---

## Stopping Examples

Press **Ctrl+C** to gracefully stop any running example.

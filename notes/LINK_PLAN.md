# Refactoring Plan: Extract Testable Logic from NET Firmware

This document describes how to move device-independent logic from `net/src/main.rs` into `link::net` for better testability and code reuse.

## Goals

1. Extract ~500 lines of duplicated/device-independent logic from `net` firmware
2. Enable unit testing of MoQ state machine (currently untested)
3. Unify storage interfaces between flash-based and NVS-based implementations
4. Reduce maintenance burden of parallel implementations

## Phase 1: Extract MoQ Types to `link::shared::moq`

**File:** `link/src/shared/moq.rs`

The MoQ types are pure data structures with no device dependency. Move them from `net/src/main.rs` to the shared library.

### Types to add:

```rust
/// Commands sent to the MoQ task.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum MoqCommand {
    /// Set the relay URL (triggers reconnect if changed).
    SetRelayUrl(String),
    /// Run clock mode - publish timestamps every second.
    RunClock,
    /// Run benchmark mode - publish at target FPS.
    RunBenchmark { fps: u32, payload_size: u32 },
    /// Send a chat message.
    SendChat { message: String },
    /// Stop the current mode.
    StopMode,
    /// Run MoQ loopback mode - publish audio to MoQ and subscribe to same track.
    RunMoqLoopback,
    /// Run MoQ publish mode - publish audio to MoQ without subscribing.
    RunPublish,
    /// Audio frame to publish (used in MoQ loopback and publish modes).
    AudioFrame { data: Vec<u8> },
}

/// Events sent from the MoQ task back to the main loop.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum MoqEvent {
    /// Connected to relay.
    Connected,
    /// Disconnected from relay.
    Disconnected,
    /// Mode started.
    ModeStarted,
    /// Mode stopped.
    ModeStopped,
    /// Error occurred.
    Error { message: String },
    /// Chat message sent successfully.
    ChatSent,
    /// Chat message received.
    ChatReceived { message: String },
    /// Audio frame received from MoQ subscription (for loopback mode).
    AudioReceived { data: Vec<u8> },
}

/// Current mode the MoQ task is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum MoqMode {
    #[default]
    Idle,
    Clock,
    Benchmark { fps: u32, payload_size: u32 },
    MoqLoopback,
    Publish,
}

/// Runtime MoQ configuration.
#[derive(Clone, Debug)]
pub struct MoqConfig {
    /// Target FPS for benchmark mode (0 = burst mode).
    pub benchmark_fps: u32,
    /// Payload size for benchmark mode.
    pub benchmark_payload_size: u32,
}

impl Default for MoqConfig {
    fn default() -> Self {
        Self {
            benchmark_fps: 50,
            benchmark_payload_size: 640,
        }
    }
}
```

### Changes needed:

1. Add types to `link/src/shared/moq.rs`
2. Feature-gate with `#[cfg(feature = "std")]` for `String` and `Vec<u8>` (or use `alloc`)
3. Re-export from `link/src/lib.rs`
4. Update `net/src/main.rs` to import from `link::MoqCommand` etc.

---

## Phase 2: Create Storage Trait

**File:** `link/src/net/storage.rs`

Create a trait that abstracts over both flash-based (`NetStorage`) and NVS-based (`NvsStorage`) storage.

### Trait definition:

```rust
/// Trait for NET chip persistent storage.
///
/// Implemented by both flash-based storage (for bare-metal) and
/// NVS-based storage (for ESP-IDF).
pub trait NetStorageTrait {
    type Error;

    // WiFi SSIDs
    fn add_wifi_ssid(&mut self, ssid: &str, password: &str) -> Result<(), Self::Error>;
    fn get_wifi_ssids(&self) -> &[WifiSsid];
    fn clear_wifi_ssids(&mut self);

    // Relay URL
    fn get_relay_url(&self) -> &str;
    fn set_relay_url(&mut self, url: &str) -> Result<(), Self::Error>;

    // Channel configuration
    fn get_channel_config(&self, channel_id: u8) -> Option<&ChannelConfig>;
    fn set_channel_config(&mut self, config: ChannelConfig) -> Result<(), Self::Error>;
    fn get_all_channel_configs(&self) -> &[ChannelConfig];
    fn clear_channel_configs(&mut self);

    // Persistence
    fn save(&mut self) -> Result<(), Self::Error>;
}
```

### Changes needed:

1. Add trait to `link/src/net/storage.rs`
2. Implement trait for existing `NetStorage<F>`
3. In `net/src/main.rs`, implement trait for `NvsStorage`
4. Update message handlers to be generic over `impl NetStorageTrait`

---

## Phase 3: Extract MoQ State Machine

**File:** `link/src/net/moq.rs` (new file)

The MoQ state machine is ~450 lines of complex logic with zero test coverage. Extract it into a testable module.

### Design approach:

Use a callback-based design to abstract over actual I/O operations:

```rust
/// Callbacks for MoQ client operations.
///
/// This trait abstracts over the actual quicr client, allowing the
/// state machine to be tested without real network I/O.
pub trait MoqClient {
    type Error: core::fmt::Debug;
    type PublishTrack;
    type Subscription;

    /// Connect to relay at given URL.
    fn connect(&mut self, url: &str) -> Result<(), Self::Error>;

    /// Disconnect from relay.
    fn disconnect(&mut self);

    /// Check if connected.
    fn is_connected(&self) -> bool;

    /// Create a publish track.
    fn create_publish_track(
        &mut self,
        namespace: &[&str],
        track_name: &str,
    ) -> Result<Self::PublishTrack, Self::Error>;

    /// Create a subscription.
    fn create_subscription(
        &mut self,
        namespace: &[&str],
        track_name: &str,
    ) -> Result<Self::Subscription, Self::Error>;

    /// Publish data to a track.
    fn publish(
        &self,
        track: &Self::PublishTrack,
        group_id: u64,
        object_id: u64,
        data: &[u8],
    ) -> Result<(), Self::Error>;

    /// Try to receive from subscription (non-blocking).
    fn try_recv(&mut self, subscription: &mut Self::Subscription) -> Option<Vec<u8>>;
}

/// MoQ state machine.
///
/// Handles mode transitions, reconnection logic, and periodic operations.
/// Device-independent and fully testable.
pub struct MoqStateMachine<C: MoqClient> {
    client: C,
    relay_url: Option<String>,
    mode: MoqMode,
    config: MoqConfig,

    // Clock mode state
    clock_track: Option<C::PublishTrack>,
    clock_group_id: u64,
    last_clock_publish: Option<Instant>,

    // Benchmark mode state
    benchmark_track: Option<C::PublishTrack>,
    benchmark_group_id: u64,
    benchmark_packets_sent: u64,
    last_benchmark_publish: Option<Instant>,
    last_benchmark_report: Option<Instant>,

    // Loopback/Publish mode state
    loopback_track: Option<C::PublishTrack>,
    loopback_subscription: Option<C::Subscription>,
    loopback_group_id: u64,
    loopback_object_id: u64,
    loopback_recv_count: u64,
    last_loopback_stats: Instant,

    // Reconnection state
    last_reconnect_attempt: Option<Instant>,
}

impl<C: MoqClient> MoqStateMachine<C> {
    pub fn new(client: C) -> Self;

    /// Process a command. Returns events to emit.
    pub fn handle_command(&mut self, cmd: MoqCommand) -> Vec<MoqEvent>;

    /// Run periodic tick. Returns events to emit.
    /// Call this regularly (e.g., every 1ms).
    pub fn tick(&mut self, now: Instant) -> Vec<MoqEvent>;

    /// Get current mode.
    pub fn mode(&self) -> MoqMode;

    /// Check if connected.
    pub fn is_connected(&self) -> bool;
}
```

### Implementation details:

The state machine methods would contain the logic currently in `spawn_moq_task`:

1. `handle_command()` - Handles `MoqCommand` variants, updates state, returns events
2. `tick()` - Handles periodic operations:
   - Clock mode: publish timestamp every second
   - Benchmark mode: publish at target FPS, report stats
   - Loopback mode: poll subscription, log stats
   - Reconnection: attempt reconnect every 5 seconds if disconnected

### Time abstraction:

For testability, use a `Clock` trait or pass `Instant` to `tick()`:

```rust
pub trait Clock {
    fn now(&self) -> Instant;
}

// Real implementation
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant { Instant::now() }
}

// Test implementation
pub struct MockClock { pub now: Instant }
impl Clock for MockClock {
    fn now(&self) -> Instant { self.now }
}
```

### Changes needed:

1. Create `link/src/net/moq.rs` with trait and state machine
2. Add feature flag `moq` to `link/Cargo.toml` (requires `std` or `alloc`)
3. Create mock `MoqClient` implementation for tests
4. Write unit tests for all mode transitions and edge cases
5. In `net/src/main.rs`:
   - Implement `MoqClient` trait for real quicr client
   - Replace `spawn_moq_task` loop body with `MoqStateMachine`

---

## Phase 4: Unify Message Handlers

**File:** `link/src/net/handlers.rs` (new file)

The message handlers in `net/src/main.rs` (`handle_mgmt_message`, `handle_ui_message`) are nearly identical to those in `link/src/net/mod.rs` but use different I/O.

### Design approach:

Create generic handlers that work with any I/O implementation:

```rust
/// Trait for writing TLV responses.
pub trait TlvWriter<T> {
    fn write_tlv(&mut self, msg_type: T, value: &[u8]);
}

/// Trait for sending MoQ commands.
pub trait MoqCommandSender {
    fn send(&self, cmd: MoqCommand);
}

/// Handle MGMT message.
///
/// Generic over storage, I/O, and MoQ command channel.
pub fn handle_mgmt_message<S, M, U, C>(
    msg_type: MgmtToNet,
    value: &[u8],
    storage: &mut S,
    to_mgmt: &mut M,
    to_ui: &mut U,
    moq_cmd: &C,
    loopback: &mut bool,
    moq_config: &mut MoqConfig,
    ptt_stats: Option<&JitterStats>,
    ptt_ai_stats: Option<&JitterStats>,
) where
    S: NetStorageTrait,
    M: TlvWriter<NetToMgmt>,
    U: TlvWriter<NetToUi>,
    C: MoqCommandSender,
{
    // ... handler logic ...
}

/// Handle UI message.
pub fn handle_ui_message<M, U, C>(
    msg_type: UiToNet,
    value: &[u8],
    to_mgmt: &mut M,
    to_ui: &mut U,
    moq_cmd: &C,
    loopback: bool,
) where
    M: TlvWriter<NetToMgmt>,
    U: TlvWriter<NetToUi>,
    C: MoqCommandSender,
{
    // ... handler logic ...
}
```

### Changes needed:

1. Create `link/src/net/handlers.rs` with generic handlers
2. Implement `TlvWriter` for:
   - Async writers (existing `WriteTlv` trait in `link::net`)
   - Sync writers (`UartDriver` wrapper in `net`)
3. Implement `MoqCommandSender` for:
   - `mpsc::Sender<MoqCommand>` (used in `net`)
   - `embassy_sync::channel::Sender` (used in `link::net`)
4. Update both `link/src/net/mod.rs` and `net/src/main.rs` to use shared handlers

---

## Phase 5: Audio Routing Logic

**File:** `link/src/net/audio.rs` (new file)

The audio routing logic (channel ID extraction, buffer selection) is duplicated.

### Extract shared logic:

```rust
/// Route received audio data to appropriate jitter buffer.
///
/// Returns the channel ID and payload slice, or None if data is invalid.
pub fn parse_audio_frame(data: &[u8]) -> Option<(ChannelId, &[u8])> {
    if data.len() < 2 {
        return None;
    }
    let channel_id = ChannelId::try_from(data[0]).ok()?;
    Some((channel_id, &data[1..]))
}

/// Build audio frame with channel ID prefix.
pub fn build_audio_frame(channel_id: ChannelId, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + payload.len());
    frame.push(channel_id as u8);
    frame.extend_from_slice(payload);
    frame
}
```

---

## Implementation Order

1. **Phase 1: MoQ Types** (smallest change, immediate benefit)
   - ~30 minutes
   - No risk, pure addition

2. **Phase 2: Storage Trait** (enables Phase 4)
   - ~1 hour
   - Low risk, trait addition + impl

3. **Phase 5: Audio Routing** (small, independent)
   - ~15 minutes
   - No risk, pure addition

4. **Phase 4: Message Handlers** (depends on Phase 2)
   - ~2 hours
   - Medium risk, refactoring existing code

5. **Phase 3: MoQ State Machine** (largest change, biggest benefit)
   - ~4 hours
   - Higher risk, but enables comprehensive testing

---

## File Changes Summary

| File | Action | Lines |
|------|--------|-------|
| `link/src/shared/moq.rs` | Modify | +80 |
| `link/src/net/storage.rs` | Modify | +40 |
| `link/src/net/moq.rs` | Create | +400 |
| `link/src/net/handlers.rs` | Create | +250 |
| `link/src/net/audio.rs` | Create | +30 |
| `link/src/net/mod.rs` | Modify | -200, +50 |
| `net/src/main.rs` | Modify | -500, +100 |

**Net result:** ~200 fewer lines in `net/src/main.rs`, ~400 more lines in `link` (but testable)

---

## Testing Strategy

### Unit tests to add:

1. **MoQ State Machine** (`link/src/net/moq.rs`)
   - Mode transitions (Idle → Clock → Idle, etc.)
   - Command handling for each command type
   - Reconnection logic (5-second backoff)
   - Clock mode timing (1 publish per second)
   - Benchmark mode timing and stats
   - Loopback receive polling
   - Error handling and recovery

2. **Message Handlers** (`link/src/net/handlers.rs`)
   - Each `MgmtToNet` command
   - Each `UiToNet` command
   - Storage persistence
   - Error responses

3. **Audio Routing** (`link/src/net/audio.rs`)
   - Valid frame parsing
   - Invalid frame handling
   - Frame building

### Integration tests:

After refactoring, the existing tests in `link/src/net/mod.rs` should continue to pass with minimal changes.

---

## Risks and Mitigations

1. **Paradigm mismatch (async vs sync)**
   - Mitigation: Use traits that abstract over I/O, not async/await
   - The handlers are fundamentally synchronous (process message, emit response)

2. **Breaking existing functionality**
   - Mitigation: Implement incrementally, test after each phase
   - Keep old code until new code is proven

3. **Performance impact**
   - Mitigation: Use `#[inline]` for hot paths, avoid allocations in tight loops
   - Profile before and after

4. **Feature flag complexity**
   - Mitigation: Use minimal feature flags (`std` for heap types)
   - Document required features clearly

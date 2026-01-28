# Audio Playout Plan for moq-sub

This document outlines the plan to add SFrame decryption, A-law decoding, and audio playout to the `moq-sub` tool.

## Data Flow

```
MoQ Object → [channel_id][SFrame ciphertext]
           → SFrame decrypt → [chunk header][A-law audio]
           → parse_chunk() → A-law audio (160 bytes)
           → decode_alaw() → PCM i16 samples (160 samples, 20ms @ 8kHz)
           → ring buffer → audio output thread
           → sample rate convert → speaker
```

## Components

### 1. SFrame Decryption (~1 hour)

- Copy `link/src/ui/sframe.rs` and adapt for std (replace `heapless::Vec` with `std::Vec`)
- Add dependencies: `aes-gcm`, `hkdf`, `sha2`
- Add CLI argument: `--sframe-key <32-char hex>` (16 bytes)
- Key ID is assumed to be 0 (or add `--sframe-kid` if needed)

### 2. Chunk Parsing (~15 minutes)

- Copy `link/src/shared/chunk.rs` parsing logic
- Use `parse_chunk()` to extract audio from decrypted payload
- Handle `MessageType::Media` chunks (6-byte header + audio)

### 3. A-law Decoding (~15 minutes)

- Add dependency: `audio-codec-algorithms`
- Call `decode_alaw(byte)` for each of the 160 bytes
- Output: 160 i16 PCM samples (20ms @ 8kHz mono)

### 4. Audio Playout (~3-5 hours)

This is the bulk of the work.

#### Dependencies
- `cpal` - Cross-platform audio I/O
- `ringbuf` or `rtrb` - Lock-free ring buffer for audio thread
- Optionally `rubato` for sample rate conversion

#### Architecture
```
┌─────────────────┐     ┌──────────────┐     ┌─────────────────┐
│  Network Thread │────▶│  Ring Buffer │────▶│  Audio Callback │
│  (async recv)   │     │  (lock-free) │     │  (real-time)    │
└─────────────────┘     └──────────────┘     └─────────────────┘
```

#### Challenges
1. **Sample rate conversion**: 8kHz input → 44.1/48kHz output
   - Option A: Use `cpal`'s resampling if available
   - Option B: Use `rubato` crate for high-quality resampling
   - Option C: Simple linear interpolation (lower quality)

2. **Buffering**:
   - Target ~100-200ms buffer (5-10 frames) to absorb jitter
   - Ring buffer sized for worst-case burst

3. **Underrun handling**:
   - Play silence (0 samples) or comfort noise (0xD5 A-law = silence)
   - Log underrun events for debugging

4. **Thread synchronization**:
   - Lock-free ring buffer between async network and audio callback
   - Audio callback runs in real-time context (no blocking!)

#### Implementation Steps
1. Set up cpal output stream at 48kHz stereo
2. Create ring buffer (e.g., 500ms capacity)
3. In network receive loop: decode → push to ring buffer
4. In audio callback: pop from ring buffer → resample → output
5. Handle underruns gracefully

### CLI Arguments (Proposed)

```
moq-sub --relay moqt://... --namespace ... --track ...
        --sframe-key <hex>    # 32-char hex (16 bytes)
        --play                # Enable audio playout
        --buffer-ms 100       # Playout buffer size
```

## Estimated Total Time: 4-6 hours

| Component | Estimate |
|-----------|----------|
| SFrame decryption | 1 hour |
| Chunk parsing | 15 min |
| A-law decoding | 15 min |
| Audio playout | 3-5 hours |

## Source Files to Reference

- `link/src/ui/sframe.rs` - SFrame encryption/decryption
- `link/src/shared/chunk.rs` - Chunk format parsing
- `link/src/ui/audio.rs` - A-law encode/decode, frame types
- `link/src/shared/protocol.rs` - MessageType enum

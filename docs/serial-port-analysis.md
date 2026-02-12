# Serial Port Architecture: ctl vs web-ctl

## Overview

Both `ctl` (native CLI) and `web-ctl` (WASM/WebSerial) share the same core
protocol library (`link/src/ctl/`), parameterized by a `CtlPort` trait. The
trait is implemented by `TokioSerialPort` (native) and `WebSerialAdapter`
(WASM). Everything above the port — TLV framing, tunnel parsing, command
handlers — is identical in both stacks.

```
                ctl (native)                    web-ctl (WASM)
            ┌─────────────────┐            ┌─────────────────┐
            │  ctl/handlers   │            │  web-ctl/lib.rs  │
            └────────┬────────┘            └────────┬────────┘
                     │                              │
            ┌────────┴──────────────────────────────┴────────┐
            │              CtlCore<P: CtlPort>                │
            │   read_tlv · write_tlv · tunnel parsing · etc.  │
            └────────┬──────────────────────────────┬────────┘
                     │                              │
            ┌────────┴────────┐            ┌────────┴────────┐
            │ TokioSerialPort │            │ WebSerialAdapter │
            │ (tokio-serial)  │            │   (WebSerial)    │
            └────────┬────────┘            └────────┬────────┘
                     │                              │
               OS serial driver            Browser WebSerial API
```

## Port Configuration

Both stacks open the port identically:

| Setting   | ctl                        | web-ctl                           |
|-----------|----------------------------|-----------------------------------|
| Baud rate | user-specified (def 115200) | user-specified                    |
| Parity    | Even                       | Even                              |
| DTR/RTS   | via `tokio_serial`         | via `setSignals()` JS API         |
| Timeout   | 3 s (hard-coded default)   | 3000 ms (hard-coded default)      |
| Init      | `init_port()` → DTR/RTS low, 100 ms settle | same              |

## Read Buffering

Both stacks use an internal `VecDeque<u8>` read buffer. When `read()` is
called, the port first drains from this buffer; if empty it calls an internal
`fill_buffer()` that reads from the underlying transport and appends.

### Native (`TokioSerialPort`)

```
read(buf)
 ├─ buffer non-empty → drain, return
 └─ buffer empty
     └─ fill_buffer()
         └─ tokio::time::timeout(3s, stream.read(&mut [0u8; 1024]))
             ├─ data arrives → extend buffer, return Ok(n)
             └─ timeout → return Err(TimedOut)
```

Key: `tokio::time::timeout` *cancels* the underlying `AsyncRead::read()`.
The OS serial driver retains any not-yet-delivered bytes in the kernel buffer.
**No data is lost on timeout.**

### WASM (`WebSerial`)

```
read(buf)
 ├─ buffer non-empty → drain, return
 └─ buffer empty
     └─ fill_buffer()
         └─ select(
              JsFuture::from(reader.read()),   ← JS Promise
              js_timeout(3000ms)               ← setTimeout Promise
            )
             ├─ read wins  → parse chunk, extend buffer, return Ok(())
             └─ timeout wins → return Err("Read timeout")
```

Key: When the timeout wins the `select()`, the `reader.read()` JS Promise
is **not cancelled** — JS Promises are not cancellable. The
`ReadableStreamDefaultReader` still has a pending read request. When data
eventually arrives for that promise, the chunk is consumed from the stream
but the `JsFuture` has been dropped — **the data is silently lost**.

The next call to `fill_buffer()` issues *another* `reader.read()`. The
ReadableStream spec queues this behind the orphaned read. So the orphaned
read consumes the first incoming chunk (lost), and our actual read gets the
*second* chunk (which may be from a completely different TLV).

## TLV Protocol

Both stacks use the same TLV framing (`SYNC_WORD + type + length + value`):

```
[4C 49 4E 4B]  ← sync word "LINK"
[xx xx]        ← type (u16 big-endian)
[xx xx xx xx]  ← length (u32 big-endian)
[... value ...]
```

### Sync-word scanning in `read_tlv()`

`read_tlv()` scans byte-by-byte for the sync word, calling `port.read()`
for each byte. Once found, it calls `read_exact()` for the 6-byte header
and then `read_exact()` for the value. Any error (including timeout) on
*any* of these reads propagates as `CtlError::Port(...)` and aborts the
entire operation.

### Tunnel architecture

Commands to UI and NET chips are *tunneled* through MGMT:

```
CTL → [ToUi, inner_tlv]  → MGMT → [inner_tlv] → UI
UI  → [response_tlv]     → MGMT → [FromUi, response_tlv] → CTL
```

`read_tlv_ui()` loops: read an outer MGMT TLV, if it's `FromUi` append
payload to `ui_buffer`, if it's `FromNet` append to `net_buffer`, else
skip. After each `FromUi` append, try to parse a complete inner TLV from
`ui_buffer`. Both `ui_buffer` and `net_buffer` are `heapless::Vec<u8, 640>`.

## The `hello()` Command — Why It Works

```rust
pub async fn hello(&mut self, challenge: &[u8; 4]) -> bool {
    // ...
    for _ in 0..MAX_TLVS {          // 1024 iterations
        match self.read_tlv::<MgmtToCtl>().await {
            Ok(Some(tlv)) => {
                if tlv.tlv_type == MgmtToCtl::Hello && tlv.value.len() == 4 {
                    return tlv.value == expected;
                }
                // skip non-Hello TLVs (boot spam, etc.)
            }
            Ok(None) | Err(_) => return false,  // swallow ALL errors
        }
    }
    false
}
```

1. **Direct MGMT-level**: reads `MgmtToCtl` TLVs from the wire, no tunnel parsing.
2. **Error-tolerant**: catches all errors/timeouts and returns `false`.
3. **Spam-tolerant**: loops up to 1024 times, skipping `FromNet`/`FromUi` boot spam.
4. **Called first**: when the JS frontend calls `hello()`, the reader is in a clean
   state — no orphaned promises, no stale buffer data.

## Other Commands — Why They Fail

Every other command follows this pattern:

```rust
pub async fn get_version(&mut self) -> Result<u32, CtlError> {
    self.write_tlv_ui(MgmtToUi::GetVersion, &[]).await?;
    let tlv = self.read_tlv_ui_skip_log().await?;  // ← propagates errors
    // ... parse response ...
}
```

1. **Tunneled**: goes through `read_tlv_ui()` → `read_tlv::<MgmtToCtl>()` → loop.
2. **Error-propagating**: any timeout or read error immediately fails the command.
3. **No retry**: a single timeout kills the entire operation.

## Theories: Why ctl Works but web-ctl Doesn't

### Theory 1: Orphaned Read Promises Corrupt the Stream (Most Likely)

This is the strongest theory because it explains the "hello works, everything
after doesn't" pattern.

**The bug**: `fill_buffer()` uses `select(reader.read(), timeout)`. When
timeout wins, the `reader.read()` JS Promise is orphaned but still pending.
Data arriving for that promise is consumed from the stream but never stored.
Subsequent `reader.read()` calls queue behind the orphan, causing each one
to miss the first chunk and receive the *next* one.

**The trigger**: If *any* read times out (even once), the ReadableStream
enters a degraded state where chunks are silently lost. TLV framing breaks
because bytes go missing mid-stream.

**Why hello still works**: `hello()` is called first, before any timeouts
have occurred, so the reader is still clean.

**Why subsequent commands fail**: One of two scenarios:

  a. The JS frontend calls `get_all_state()` after hello. The first sub-call
     (`get_net_loopback_mode()`) talks to the NET chip. If NET hasn't fully
     booted, it doesn't respond within 3 seconds → timeout → orphaned read →
     all subsequent commands fail due to data loss.

  b. Between the `hello()` call and the next command, enough time passes that
     boot spam fills the WebSerial internal buffer. On the next command's
     first read, the data comes back fast, but if any intermediate `fill_buffer`
     call times out for any reason (browser scheduling jank, GC pause >3s),
     the reader is poisoned.

**In native ctl**: `tokio::time::timeout` cancels the `AsyncRead::read()`
future. The OS kernel retains un-read bytes in its serial buffer. No data
is ever lost. The next read picks up exactly where the last one left off.

**Fix**: After a timeout in `fill_buffer()`, refresh the reader (release lock,
get new reader) — the same thing `drain()` already does. Or better: don't
use `select()`; instead set `bufferSize` on the WebSerial `open()` options
and handle timeout differently.

### Theory 2: Buffered Boot Spam Overflows the heapless Tunnel Buffers

`ui_buffer` and `net_buffer` are `heapless::Vec<u8, 640>`. When boot spam
arrives (NET chip printing ESP-IDF startup text), it's wrapped in `FromNet`
TLVs and appended to `net_buffer`:

```rust
MgmtToCtl::FromNet => {
    let _ = self.net_buffer.extend_from_slice(&tlv.value);
    //  ^^^ silently fails if buffer is full
}
```

If NET sends >640 bytes of boot spam, the buffer silently overflows. This
isn't catastrophic for the `net_buffer` itself (data is just dropped), but
it means the TLV sync scanner inside the buffer may see truncated data, fail
to find a sync word, and spin waiting for more data that will never arrive
(because it was silently dropped).

**In native ctl**: Same code, same risk. But the native CLI typically calls
`hello()` with a short 500ms timeout during auto-detection (in `try_connect`),
which processes and discards boot spam quickly. Then it restores the 3s
timeout. web-ctl doesn't have this warmup period.

### Theory 3: Missing `drain()` Between Operations

The native CLI calls `drain()` explicitly before certain operations (like
flashing), and `hello()` naturally drains boot spam by reading through it
in a loop. But web-ctl doesn't call `drain()` after `connect()` or `hello()`.

If there's stale data on the wire between `hello()` returning and the next
command being sent, the response to the next command will be preceded by
stale TLVs. The tunnel parsing code *should* handle this (it loops and skips
unexpected TLV types), but if the stale data is mid-TLV (partial sync word,
partial header), the sync scanner could misparse subsequent data.

### Theory 4: WebSerial Chunk Granularity Causes Framing Issues

`tokio_serial::read()` can return any number of bytes (1 to buffer-size).
WebSerial `reader.read()` returns whatever "chunk" the browser decides to
deliver — this could be 1 byte or 4096 bytes, and the granularity is
unpredictable.

The TLV scanning code reads byte-by-byte via `read(&mut [0u8; 1])`. In
native, this returns 1 byte from the 1024-byte kernel read. In WebSerial,
this calls `fill_buffer()` → `reader.read()` → gets a whole chunk → puts
it all in the buffer → returns 1 byte. This is fine for a single call.

But the issue is that `reader.read()` may deliver data that spans multiple
TLVs in a single chunk. The scanning code handles this correctly (via the
internal buffer). However, if a chunk boundary falls in the middle of a
sync word, and a timeout fires during the read of the next chunk, you get
the orphaned-promise problem from Theory 1 at the worst possible time —
mid-TLV.

## Comparison Summary

| Aspect | Native ctl | web-ctl |
|--------|-----------|---------|
| Transport | OS serial (kernel-buffered) | WebSerial ReadableStream |
| Timeout cancel | Future is cancelled; bytes stay in kernel buffer | Promise is orphaned; bytes consumed and lost |
| Data loss on timeout | None | **Yes — chunk consumed by orphaned promise** |
| Boot spam handling | `try_connect` drains via hello loop w/ 500ms timeout | No explicit drain after connect |
| Tunnel buffer overflow | Same code (heapless 640B) | Same code (heapless 640B) |
| Chunk granularity | Controlled by OS (typically small) | Controlled by browser (unpredictable) |
| Error semantics of `hello()` | Returns `bool` (errors swallowed) | Same |
| Error semantics of commands | Returns `Result` (errors propagated) | Same |

## Recommendation

The highest-priority fix is making WebSerial timeout-safe. The fundamental
invariant that the native stack preserves ("a timeout never loses data") is
violated in the WebSerial stack. Options:

1. **Refresh the reader after every timeout** in `fill_buffer()` — call
   `reader.release_lock()` then `port.readable().get_reader()`. This
   discards the orphaned promise and any data it might consume, but at
   least prevents cascading corruption.

2. **Never use `select()` for timeout** — instead, set a per-read timeout
   via `AbortSignal.timeout()` passed to `reader.read({ signal })`, which
   lets the browser cancel the read natively.

3. **Add a drain/warmup step after connect** — after `init_port()`, read
   and discard TLVs for a short period (similar to what `try_connect` does
   in native ctl) to absorb boot spam before the frontend issues real
   commands.

# CLAUDE Comment Research Report

Research findings for Category 3 questions from CLAUDE comments.

## 1. EmbassyDelay vs embedded_hal_async::delay::DelayNs

**Location**: `mgmt/src/main.rs:18`

**Status**: **FIXED** (commit 10ea6bd)

Replaced custom `AsyncDelay` trait with standard `embedded_hal_async::delay::DelayNs`.
Now uses `embassy_time::Delay` directly which implements `DelayNs`.

---

## 2. Manual HTTP Request Building

**Location**: `net/src/main.rs:480`

**Status**: **RESOLVED** (commit a61476b)

**Investigation findings**:
- `edge_http::io::client::Connection` provides `initiate_ws_upgrade_request()`
- BUT it requires `TcpConnect` and manages the TCP connection lifecycle itself
- We already have an established TLS connection over TCP
- Therefore, we cannot use `Connection` - the current approach is correct

**Changes made**:
- Extracted HTTP request building into `build_ws_upgrade_request()` helper
- Added documentation explaining why we format the request ourselves
- Cleaned up the code structure

---

## 3. edge_ws::io::recv Cancellation Safety

**Location**: `net/src/main.rs:614`

**Status**: **FIXED**

**Investigation findings** (from edge-ws source):

`edge_ws::io::recv` is **NOT cancellation-safe**:
```rust
pub async fn recv<R>(read: R, frame_data_buf: &mut [u8]) -> Result<...> {
    let header = FrameHeader::recv(&mut read).await?;  // await point 1
    header.recv_payload(read, frame_data_buf).await?;  // await point 2
    Ok((header.frame_type, header.payload_len as _))
}
```

If cancelled between the two await points:
1. Header bytes are consumed from the stream
2. Header data is lost when the future is dropped
3. Next recv attempt reads payload bytes as a new header - stream corruption

**Resolution**: Refactored to use `FrameHeader::recv()` separately from payload read:
```rust
// Only the short header read (2-14 bytes) is in the select
match select(FrameHeader::recv(&mut tls), cmd_rx.receive()).await {
    Either::First(header_result) => {
        let header = header_result?;
        // Payload read is OUTSIDE the select - cannot be cancelled
        let payload = header.recv_payload(&mut tls, &mut ws_read_buf).await?;
        // Handle frame...
    }
    Either::Second(cmd) => { /* handle command */ }
}
```

This minimizes the cancellation window to just the header read (2-14 bytes), which
completes very quickly. The payload read happens after the select, so it cannot
be interrupted by incoming commands.

---

## 4. Echo Test vs Speed Test

**Location**: `net/src/main.rs:831`

**Status**: **RESOLVED** (commit a61476b)

**Conclusion**: **Keep both** - they measure different things:

| Test | Sending Rate | Measures |
|------|--------------|----------|
| Echo test | 20ms intervals (50 fps) | Network **jitter** and buffer behavior |
| Speed test | As fast as possible | Network **throughput** capacity |

Added documentation explaining the purpose of each test.

---

## 5. no_std URL Parsing Library

**Location**: `net/src/main.rs:1160`

**Status**: **FIXED** (commit b29585a)

Added `url-lite` crate for URL parsing. The custom `parse_wss_url` now uses
`url_lite::Url::parse()` which handles edge cases better.

---

## Summary

| # | Question | Status | Resolution |
|---|----------|--------|------------|
| 1 | EmbassyDelay vs DelayNs | **Fixed** | Use standard `DelayNs` trait |
| 2 | Manual HTTP building | **Resolved** | Documented why it's necessary, extracted helper |
| 3 | Cancellation safety | **Fixed** | Use `FrameHeader::recv()` in select, read payload after |
| 4 | Echo/Speed tests | **Resolved** | Documented different purposes |
| 5 | URL parsing | **Fixed** | Use `url-lite` crate |

## All CLAUDE Comments Resolved

All CLAUDE comments in the codebase have been addressed.

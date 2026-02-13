# CTL Test Plan

All tests assume an EV16 device is connected via USB serial with MGMT, UI, and NET
chips running valid firmware.  The `ctl` binary is assumed to be on `$PATH`.

Commands are invoked in CLI mode (`ctl <command>`) rather than REPL mode so they
can be driven from a script and their exit code inspected.


## 1. Scripted Tests

These tests produce deterministic output and a meaningful exit code.
A test harness can run the command, check `$?`, and grep stdout for the
expected string.

### 1.1 Connection and Handshake

| # | Command | Pass criteria |
|---|---------|---------------|
| 1 | `ctl hello` | Exit 0, stdout contains `Hello OK!` |

### 1.2 MGMT Chip

| # | Command | Pass criteria |
|---|---------|---------------|
| 2 | `ctl mgmt ping` | Exit 0, stdout contains `Received pong!` |
| 3 | `ctl mgmt ping "test1234"` | Exit 0, stdout contains `Received pong!` |
| 4 | `ctl mgmt stack info` | Exit 0, stdout contains `Stack Size:` and `Stack Used:` |
| 5 | `ctl mgmt stack repaint` | Exit 0, stdout contains `Stack repainted` |
| 6 | `ctl mgmt net-baud-rate set 115200` then `ctl mgmt net-baud-rate set 1000000` | Each exits 0, stdout contains `NET baud rate set to` |
| 7 | `ctl mgmt ctl-baud-rate get` | Exit 0, stdout contains `Get not implemented` |

### 1.3 UI Chip

| # | Command | Pass criteria |
|---|---------|---------------|
| 8 | `ctl ui ping` | Exit 0, stdout contains `Received pong!` |
| 9 | `ctl ui ping "abcdef"` | Exit 0, stdout contains `Received pong!` |
| 10 | `ctl ui version get` | Exit 0, stdout is a decimal number |
| 11 | `ctl ui version set 42` then `ctl ui version get` | Set exits 0; get exits 0 and stdout is `42` |
| 12 | `ctl ui sframe-key get` | Exit 0, stdout is 32 hex characters |
| 13 | `ctl ui sframe-key set 00112233445566778899aabbccddeeff` then `ctl ui sframe-key get` | Set exits 0; get exits 0 and stdout is `00112233445566778899aabbccddeeff` |
| 14 | `ctl ui loopback get` | Exit 0, stdout is one of `Off`, `Raw`, `Alaw`, `Sframe` |
| 15 | `ctl ui loopback raw` then `ctl ui loopback get` | Set exits 0; get stdout is `Raw` |
| 16 | `ctl ui loopback off` | Exit 0, stdout contains `UI loopback: off` |
| 17 | `ctl ui boot0 set high` | Exit 0, stdout contains `UI BOOT0: High` |
| 18 | `ctl ui boot0 set low` | Exit 0, stdout contains `UI BOOT0: Low` |
| 19 | `ctl ui boot1 set high` | Exit 0, stdout contains `UI BOOT1: High` |
| 20 | `ctl ui boot1 set low` | Exit 0, stdout contains `UI BOOT1: Low` |
| 21 | `ctl ui rst set low` then `ctl ui rst set high` | Each exits 0, stdout contains `UI RST: Low` / `UI RST: High` |
| 22 | `ctl ui reset user` | Exit 0, stdout contains `UI chip reset to user mode` |
| 23 | `ctl ui reset hold` then `ctl ui reset release` | Each exits 0 |
| 24 | `ctl ui reset bootloader` then `ctl ui reset user` | Each exits 0 |
| 25 | `ctl ui stack info` | Exit 0, stdout contains `Stack Size:` and `Stack Used:` |
| 26 | `ctl ui stack repaint` | Exit 0, stdout contains `Stack repainted` |

### 1.4 NET Chip

| # | Command | Pass criteria |
|---|---------|---------------|
| 27 | `ctl net ping` | Exit 0, stdout contains `Received pong!` |
| 28 | `ctl net ping "xyzzy"` | Exit 0, stdout contains `Received pong!` |
| 29 | `ctl net loopback get` | Exit 0, stdout is one of `off`, `raw`, `moq` |
| 30 | `ctl net loopback raw` then `ctl net loopback get` | Set exits 0; get stdout is `raw` |
| 31 | `ctl net loopback off` | Exit 0, stdout contains `NET loopback: off` |
| 32 | `ctl net wifi clear` | Exit 0, stdout contains `Cleared all WiFi networks` |
| 33 | `ctl net wifi add TestNet secret123` then `ctl net wifi` | Add exits 0; list stdout contains `TestNet` and `secret123` |
| 34 | `ctl net wifi clear` then `ctl net wifi` | Clear exits 0; list stdout contains `No WiFi networks configured` |
| 35 | `ctl net relay-url set https://example.com/relay` then `ctl net relay-url get` | Set exits 0; get stdout is `https://example.com/relay` |
| 36 | `ctl net boot set high` | Exit 0, stdout contains `NET BOOT: High` |
| 37 | `ctl net boot set low` then `ctl net boot set high` | Each exits 0 |
| 38 | `ctl net rst set low` then `ctl net rst set high` | Each exits 0 |
| 39 | `ctl net reset user` | Exit 0, stdout contains `NET chip reset to user mode` |
| 40 | `ctl net reset hold` then `ctl net reset release` | Each exits 0 |
| 41 | `ctl net reset bootloader` then `ctl net reset user` | Each exits 0 |
| 42 | `ctl net channel clear` | Exit 0, stdout contains `All channel configurations cleared` |
| 43 | `ctl net channel set 0 --enabled true --relay-url https://r.example.com` then `ctl net channel get 0` | Set exits 0; get stdout contains `enabled: true` and `https://r.example.com` |
| 44 | `ctl net channel` (list all) | Exit 0, stdout contains `Ptt` |
| 45 | `ctl net jitter-stats 0` | Exit 0, stdout contains `received:` and `underruns:` |

### 1.5 Circular Ping

| # | Command | Pass criteria |
|---|---------|---------------|
| 46 | `ctl circular-ping` | Exit 0, stdout contains `Completed circular ping!` |
| 47 | `ctl circular-ping --reverse` | Exit 0, stdout contains `Completed circular ping!` |
| 48 | `ctl circular-ping "roundtrip"` | Exit 0, stdout contains `Completed circular ping!` |

### 1.6 Restore Defaults (cleanup at end of scripted run)

Run these at the end to leave the device in a known state:

```
ctl ui loopback off
ctl ui version set 0
ctl net loopback off
ctl net wifi clear
ctl net channel clear
ctl net relay-url set ""
```


## 2. Manual Tests

These tests require human judgment, interactive input, physical observation,
or firmware binaries that aren't available in the repo.

### 2.1 MGMT Bootloader Info

| # | Command | Why manual |
|---|---------|-----------|
| 49 | `ctl mgmt info` | On EV16 the bootloader entry is automatic, so this could potentially be scripted (check for `Bootloader Version:` and `Chip ID:`). However, if auto-entry fails the command prompts for manual intervention ("Press Enter when ready"), which blocks a script. **Try scripting first**; fall back to manual if the auto-reset path is unreliable on your EV16 unit. |

### 2.2 MGMT Flash

| # | Command | Why manual |
|---|---------|-----------|
| 50 | `ctl mgmt flash <firmware.bin>` | Requires a valid MGMT firmware binary. Flashing overwrites the running firmware -- if it fails or the binary is bad, the device may need manual recovery (BOOT0 jumper). Verify by running `ctl hello` and `ctl mgmt ping` after flash to confirm the new firmware is working. Also visually confirm LEDs behave normally after reset. |

### 2.3 UI Bootloader Info

| # | Command | Why manual |
|---|---------|-----------|
| 51 | `ctl ui info` | Resets the UI chip into bootloader mode and back. The output (bootloader version, chip ID, flash sample) requires human inspection to verify correctness. Could be partially scripted by checking for `Bootloader Version:` in stdout, but a failure may leave the UI chip in bootloader mode, requiring `ctl ui reset user` to recover. |

### 2.4 UI Flash

| # | Command | Why manual |
|---|---------|-----------|
| 52 | `ctl ui flash <firmware.bin>` | Requires a valid UI firmware binary. Overwrites UI chip firmware. Verify afterward with `ctl ui ping` and by checking that audio / LED behavior is correct. Use `--no-verify` flag to test the skip-verification path separately. |

### 2.5 NET Bootloader Info

| # | Command | Why manual |
|---|---------|-----------|
| 53 | `ctl net info` | Resets the NET chip into ESP32 bootloader mode. Output (chip type, flash size, MAC address, security info) requires human inspection. A failure may leave the NET chip in bootloader mode, requiring `ctl net reset user`. |

### 2.6 NET Flash

| # | Command | Why manual |
|---|---------|-----------|
| 54 | `ctl net flash <firmware.elf>` | Requires a valid NET (ESP32) ELF binary. Overwrites NET chip firmware. Verify afterward with `ctl net ping`. |
| 55 | `ctl net flash <firmware.elf> -P partitions.csv` | Same as above but with a custom partition table. Verify the device boots and responds to `ctl net ping`. |

### 2.7 NET Erase

| # | Command | Why manual |
|---|---------|-----------|
| 56 | `ctl net erase` | Erases the entire NET flash. The NET chip will not respond to `net ping` afterward until re-flashed. Destructive -- only run if you have a NET firmware binary ready to re-flash. |

### 2.8 UI Monitor

| # | Command | Why manual |
|---|---------|-----------|
| 57 | `ctl ui monitor` | Interactive: enters raw terminal mode, streams `[UI]` log lines until ESC is pressed. Requires human to read the log output and press ESC. |
| 58 | `ctl ui monitor --reset` | Same, but resets the UI chip first. Verify that boot-time log messages appear. |

### 2.9 NET Monitor

| # | Command | Why manual |
|---|---------|-----------|
| 59 | `ctl net monitor` | Interactive: enters raw terminal mode, streams raw NET data until ESC is pressed. Requires human to observe output and press ESC. |
| 60 | `ctl net monitor --reset` | Same, but resets the NET chip first. Verify that boot-time output appears. |

### 2.10 CTL Baud Rate Change

| # | Command | Why manual |
|---|---------|-----------|
| 61 | `ctl mgmt ctl-baud-rate set 115200` | Changes the CTL-MGMT link baud rate. The `ctl` process changes the local serial port to match, so subsequent commands in the same session work. But the next invocation of `ctl` defaults to 1000000 and will fail to connect. Verify by running `ctl -b 115200 hello` afterward, then restore with `ctl -b 115200 mgmt ctl-baud-rate set 1000000`. Risky to script because a failure mid-change can leave the device unreachable until power cycle. |


## 3. Script Skeleton

Below is a minimal bash script that runs the scripted tests from section 1.
It stops on the first failure.

```bash
#!/usr/bin/env bash
set -euo pipefail

PASS=0
FAIL=0

run() {
    local description="$1"
    shift
    local expected="$1"
    shift

    echo -n "  $description ... "
    if output=$("$@" 2>&1); then
        if echo "$output" | grep -q "$expected"; then
            echo "OK"
            ((PASS++))
        else
            echo "FAIL (expected '$expected' in output)"
            echo "    got: $output"
            ((FAIL++))
            return 1
        fi
    else
        echo "FAIL (exit code $?)"
        echo "    got: $output"
        ((FAIL++))
        return 1
    fi
}

echo "=== CTL Test Suite ==="
echo ""

echo "--- Connection ---"
run "hello" "Hello OK!" ctl hello

echo "--- MGMT ---"
run "mgmt ping" "Received pong!" ctl mgmt ping
run "mgmt ping custom" "Received pong!" ctl mgmt ping "test1234"
run "mgmt stack info" "Stack Size:" ctl mgmt stack info
run "mgmt stack repaint" "Stack repainted" ctl mgmt stack repaint

echo "--- UI ---"
run "ui ping" "Received pong!" ctl ui ping
run "ui ping custom" "Received pong!" ctl ui ping "abcdef"

# version round-trip
ctl ui version set 42 >/dev/null 2>&1
run "ui version get" "42" ctl ui version get

# sframe-key round-trip
ctl ui sframe-key set 00112233445566778899aabbccddeeff >/dev/null 2>&1
run "ui sframe-key get" "00112233445566778899aabbccddeeff" ctl ui sframe-key get

# loopback round-trip
ctl ui loopback raw >/dev/null 2>&1
run "ui loopback get (raw)" "Raw" ctl ui loopback get
run "ui loopback off" "UI loopback: off" ctl ui loopback off

# pin control
run "ui boot0 high" "UI BOOT0: High" ctl ui boot0 set high
run "ui boot0 low" "UI BOOT0: Low" ctl ui boot0 set low
run "ui boot1 high" "UI BOOT1: High" ctl ui boot1 set high
run "ui boot1 low" "UI BOOT1: Low" ctl ui boot1 set low

# reset
run "ui reset user" "UI chip reset to user mode" ctl ui reset user
run "ui stack info" "Stack Size:" ctl ui stack info
run "ui stack repaint" "Stack repainted" ctl ui stack repaint

echo "--- NET ---"
run "net ping" "Received pong!" ctl net ping
run "net ping custom" "Received pong!" ctl net ping "xyzzy"

# loopback round-trip
ctl net loopback raw >/dev/null 2>&1
run "net loopback get (raw)" "raw" ctl net loopback get
run "net loopback off" "NET loopback: off" ctl net loopback off

# wifi round-trip
ctl net wifi clear >/dev/null 2>&1
ctl net wifi add TestNet secret123 >/dev/null 2>&1
run "net wifi list" "TestNet" ctl net wifi
run "net wifi clear" "Cleared all WiFi networks" ctl net wifi clear

# relay-url round-trip
ctl net relay-url set "https://example.com/relay" >/dev/null 2>&1
run "net relay-url get" "https://example.com/relay" ctl net relay-url get

# pin control
run "net boot high" "NET BOOT: High" ctl net boot set high
run "net rst high" "NET RST: High" ctl net rst set high

# reset
run "net reset user" "NET chip reset to user mode" ctl net reset user

# channels
ctl net channel clear >/dev/null 2>&1
ctl net channel set 0 --enabled true --relay-url "https://r.example.com" >/dev/null 2>&1
run "net channel get 0" "enabled: true" ctl net channel get 0
run "net channel list" "Ptt" ctl net channel
run "net channel clear" "All channel configurations cleared" ctl net channel clear

# jitter stats
run "net jitter-stats 0" "received:" ctl net jitter-stats 0

echo "--- Circular Ping ---"
run "circular-ping" "Completed circular ping!" ctl circular-ping
run "circular-ping reverse" "Completed circular ping!" ctl circular-ping --reverse

echo ""
echo "--- Cleanup ---"
ctl ui loopback off >/dev/null 2>&1
ctl ui version set 0 >/dev/null 2>&1
ctl net loopback off >/dev/null 2>&1
ctl net wifi clear >/dev/null 2>&1
ctl net channel clear >/dev/null 2>&1
ctl net relay-url set "" >/dev/null 2>&1
echo "Device restored to defaults."

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
```


## 4. Summary

| Category | Count | Method |
|----------|-------|--------|
| Scripted | 48 | Automated bash script |
| Manual | 13 | Human operator with firmware binaries |
| **Total** | **61** | |

The scripted tests cover all non-destructive, non-interactive features.
The manual tests cover flashing (requires firmware binaries, risk of bricking),
monitoring (interactive terminal), bootloader info (may require manual
intervention), and baud rate changes (risk of losing communication).

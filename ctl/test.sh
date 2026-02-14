#!/usr/bin/env bash
# Hardware integration tests for the CTL tool.
# Requires an EV16 device connected via USB serial with all chips running firmware.
# Set CTL to override the binary path, e.g.: CTL=./target/release/ctl ./test.sh
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CTL="${CTL:-$SCRIPT_DIR/target/debug/ctl}"

if [ ! -x "$CTL" ]; then
    echo "ctl binary not found at $CTL"
    echo "Run 'cargo build' in the ctl directory first, or set CTL=/path/to/ctl"
    exit 1
fi

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
            ((PASS++)) || true
        else
            echo "FAIL (expected '$expected' in output)"
            echo "    got: $output"
            ((FAIL++)) || true
        fi
    else
        echo "FAIL (exit code $?)"
        echo "    got: $output"
        ((FAIL++)) || true
    fi
}

echo "=== CTL Test Suite ==="
echo ""

# ── 1.1 Connection and Handshake ──

echo "--- Connection ---"
run "hello" "Hello OK!" "$CTL" hello

# ── 1.2 MGMT Chip ──

echo "--- MGMT ---"
run "mgmt ping" "Received pong!" "$CTL" mgmt ping
run "mgmt ping custom" "Received pong!" "$CTL" mgmt ping "test1234"
run "mgmt stack info" "Stack Size:" "$CTL" mgmt stack info
run "mgmt stack repaint" "Stack repainted" "$CTL" mgmt stack repaint

# net-baud-rate round-trip (set to 115200, then restore to 1000000)
run "mgmt net-baud-rate set 115200" "NET baud rate set to" "$CTL" mgmt net-baud-rate set 115200
run "mgmt net-baud-rate set 1000000" "NET baud rate set to" "$CTL" mgmt net-baud-rate set 1000000

# ctl-baud-rate get (not implemented)
run "mgmt ctl-baud-rate get" "Get not implemented" "$CTL" mgmt ctl-baud-rate get

# ── 1.3 UI Chip ──

echo "--- UI ---"
run "ui ping" "Received pong!" "$CTL" ui ping
run "ui ping custom" "Received pong!" "$CTL" ui ping "abcdef"

# version round-trip
"$CTL" ui version set 42 >/dev/null 2>&1
run "ui version get" "42" "$CTL" ui version get

# sframe-key round-trip
"$CTL" ui sframe-key set 00112233445566778899aabbccddeeff >/dev/null 2>&1
run "ui sframe-key get" "00112233445566778899aabbccddeeff" "$CTL" ui sframe-key get

# loopback (state is runtime-only; DTR reset clears it between CLI invocations)
run "ui loopback get" "Off" "$CTL" ui loopback get
run "ui loopback off" "UI loopback: off" "$CTL" ui loopback off

# pin control
run "ui boot0 high" "UI BOOT0: High" "$CTL" ui boot0 set high
run "ui boot0 low" "UI BOOT0: Low" "$CTL" ui boot0 set low
run "ui boot1 high" "UI BOOT1: High" "$CTL" ui boot1 set high
run "ui boot1 low" "UI BOOT1: Low" "$CTL" ui boot1 set low

# rst pin round-trip
run "ui rst low" "UI RST: Low" "$CTL" ui rst set low
run "ui rst high" "UI RST: High" "$CTL" ui rst set high

# reset sequences
run "ui reset user" "UI chip reset to user mode" "$CTL" ui reset user
run "ui reset hold" "UI chip held in reset" "$CTL" ui reset hold
run "ui reset release" "UI chip released from reset" "$CTL" ui reset release
run "ui reset bootloader" "UI chip reset to bootloader mode" "$CTL" ui reset bootloader
run "ui reset user (after bootloader)" "UI chip reset to user mode" "$CTL" ui reset user

run "ui stack info" "Stack Size:" "$CTL" ui stack info
run "ui stack repaint" "Stack repainted" "$CTL" ui stack repaint

# ── 1.4 NET Chip ──

echo "--- NET ---"
run "net ping" "Received pong!" "$CTL" net ping
run "net ping custom" "Received pong!" "$CTL" net ping "xyzzy"

# loopback (state is runtime-only; DTR reset clears it between CLI invocations)
run "net loopback get" "off" "$CTL" net loopback get
run "net loopback off" "NET loopback: off" "$CTL" net loopback off

# wifi round-trip
"$CTL" net wifi clear >/dev/null 2>&1
run "net wifi clear" "Cleared all WiFi networks" "$CTL" net wifi clear
"$CTL" net wifi add TestNet secret123 >/dev/null 2>&1
run "net wifi list" "TestNet" "$CTL" net wifi
run "net wifi clear (after add)" "Cleared all WiFi networks" "$CTL" net wifi clear
run "net wifi list (empty)" "No WiFi networks configured" "$CTL" net wifi

# relay-url round-trip
"$CTL" net relay-url set "https://example.com/relay" >/dev/null 2>&1
run "net relay-url get" "https://example.com/relay" "$CTL" net relay-url get

# pin control
run "net boot low" "NET BOOT: Low" "$CTL" net boot set low
run "net boot high" "NET BOOT: High" "$CTL" net boot set high
run "net rst low" "NET RST: Low" "$CTL" net rst set low
run "net rst high" "NET RST: High" "$CTL" net rst set high

# reset sequences
run "net reset user" "NET chip reset to user mode" "$CTL" net reset user
run "net reset hold" "NET chip held in reset" "$CTL" net reset hold
run "net reset release" "NET chip released from reset" "$CTL" net reset release
run "net reset bootloader" "NET chip reset to bootloader mode" "$CTL" net reset bootloader
run "net reset user (after bootloader)" "NET chip reset to user mode" "$CTL" net reset user

# channels
"$CTL" net channel clear >/dev/null 2>&1
run "net channel clear" "All channel configurations cleared" "$CTL" net channel clear
run "net channel set 0" "updated" "$CTL" net channel set 0 --enabled --relay-url "https://r.example.com"
run "net channel get 0" "enabled: true" "$CTL" net channel get 0
run "net channel list" "Ptt" "$CTL" net channel
run "net channel clear (after set)" "All channel configurations cleared" "$CTL" net channel clear

# jitter stats
run "net jitter-stats 0" "received:" "$CTL" net jitter-stats 0

# ── 1.5 Circular Ping ──

echo "--- Circular Ping ---"
run "circular-ping" "Completed circular ping!" "$CTL" circular-ping
run "circular-ping reverse" "Completed circular ping!" "$CTL" circular-ping --reverse
run "circular-ping custom" "Completed circular ping!" "$CTL" circular-ping "roundtrip"

# ── 1.6 Cleanup ──

echo ""
echo "--- Cleanup ---"
"$CTL" ui loopback off >/dev/null 2>&1
"$CTL" ui version set 0 >/dev/null 2>&1
"$CTL" net loopback off >/dev/null 2>&1
"$CTL" net wifi clear >/dev/null 2>&1
"$CTL" net channel clear >/dev/null 2>&1
"$CTL" net relay-url set "" >/dev/null 2>&1
echo "Device restored to defaults."

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]

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

echo "--- Connection ---"
run "hello" "Hello OK!" "$CTL" hello

echo "--- MGMT ---"
run "mgmt ping" "Received pong!" "$CTL" mgmt ping
run "mgmt ping custom" "Received pong!" "$CTL" mgmt ping "test1234"
run "mgmt stack info" "Stack Size:" "$CTL" mgmt stack info
run "mgmt stack repaint" "Stack repainted" "$CTL" mgmt stack repaint

echo "--- UI ---"
run "ui ping" "Received pong!" "$CTL" ui ping
run "ui ping custom" "Received pong!" "$CTL" ui ping "abcdef"

# version round-trip
"$CTL" ui version set 42 >/dev/null 2>&1
run "ui version get" "42" "$CTL" ui version get

# sframe-key round-trip
"$CTL" ui sframe-key set 00112233445566778899aabbccddeeff >/dev/null 2>&1
run "ui sframe-key get" "00112233445566778899aabbccddeeff" "$CTL" ui sframe-key get

# loopback (just verify get works; state doesn't persist across CLI invocations)
run "ui loopback off" "UI loopback: off" "$CTL" ui loopback off

# pin control
run "ui boot0 high" "UI BOOT0: High" "$CTL" ui boot0 set high
run "ui boot0 low" "UI BOOT0: Low" "$CTL" ui boot0 set low
run "ui boot1 high" "UI BOOT1: High" "$CTL" ui boot1 set high
run "ui boot1 low" "UI BOOT1: Low" "$CTL" ui boot1 set low

# reset
run "ui reset user" "UI chip reset to user mode" "$CTL" ui reset user
run "ui stack info" "Stack Size:" "$CTL" ui stack info
run "ui stack repaint" "Stack repainted" "$CTL" ui stack repaint

echo "--- NET ---"
run "net ping" "Received pong!" "$CTL" net ping
run "net ping custom" "Received pong!" "$CTL" net ping "xyzzy"

# loopback (just verify the command works)
run "net loopback off" "NET loopback: off" "$CTL" net loopback off

# pin control
run "net boot high" "NET BOOT: High" "$CTL" net boot set high
run "net rst high" "NET RST: High" "$CTL" net rst set high

# reset
run "net reset user" "NET chip reset to user mode" "$CTL" net reset user

echo ""
echo "--- Cleanup ---"
"$CTL" ui version set 0 >/dev/null 2>&1
echo "Device restored to defaults."

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]

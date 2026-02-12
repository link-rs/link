#!/bin/bash
set -e

# Auto-detect the serial port (find first /dev/cu.usbserial* device)
SERIAL_PORT=$(ls /dev/cu.usbserial* 2>/dev/null | head -1)

# This script is called by cargo run for the mgmt-echo tool
# It converts the ELF to binary and flashes it using the ctl program

ELF_FILE="$1"
BIN_FILE="${ELF_FILE%.elf}.bin"

# Convert to absolute paths
ELF_FILE="$(cd "$(dirname "$ELF_FILE")" && pwd)/$(basename "$ELF_FILE")"
BIN_FILE="$(dirname "$ELF_FILE")/$(basename "$BIN_FILE")"

# Get baud rate from environment or default to 460800
BAUD_RATE="${BAUD_RATE:-460800}"

echo "Converting $ELF_FILE to $BIN_FILE..."
rust-objcopy -O binary "$ELF_FILE" "$BIN_FILE"

echo "Flashing to MGMT chip..."
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/../../ctl"

# Flash the firmware at 115200 (STM32 bootloader requirement)
# The firmware will run at $BAUD_RATE after flashing
cargo run --quiet -- --port="$SERIAL_PORT" mgmt flash "$BIN_FILE"

if [ -z "$SERIAL_PORT" ]; then
    echo "Warning: No USB serial port found. Screen not launched."
    echo "Available ports:"
    ls /dev/cu.* 2>/dev/null || echo "  None"
    exit 0
fi

echo ""
echo "Launching echo-client at $BAUD_RATE baud on $SERIAL_PORT..."
echo "Press Ctrl+C to exit"
sleep 1

# Launch echo-client with the detected port and baud rate
cd "$SCRIPT_DIR/../echo-client"
exec cargo run --quiet -- --port "$SERIAL_PORT" --baud "$BAUD_RATE"

#!/bin/bash
# Runner script that resets the chip via OpenOCD before running probe-rs

set -e

ELF_FILE="$1"

if [ -z "$ELF_FILE" ]; then
    echo "Usage: $0 <elf-file>"
    exit 1
fi

# Reset the chip via OpenOCD
openocd -f board/esp32s3-ftdi.cfg -c "init; reset halt; exit" 2>/dev/null

# Run with probe-rs
exec probe-rs run --chip=esp32s3 --probe=0403:6010 --preverify --always-print-stacktrace --no-location --catch-hardfault "$ELF_FILE"

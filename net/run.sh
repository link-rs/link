#!/bin/bash
# Runner script that flashes and runs the chip via OpenOCD

set -e

ELF_FILE="$1"

if [ -z "$ELF_FILE" ]; then
    echo "Usage: $0 <elf-file>"
    exit 1
fi

# Convert ELF to binary
BIN_FILE="${ELF_FILE}.bin"
esptool.py --chip esp32s3 elf2image --flash_mode dio --flash_size 8MB -o "$BIN_FILE" "$ELF_FILE"

# Flash at app address (0x10000) via OpenOCD
openocd -f board/esp32s3-ftdi.cfg -c "program_esp $BIN_FILE 0x10000 verify reset exit"

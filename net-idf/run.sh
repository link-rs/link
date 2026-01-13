#!/bin/bash
# Runner script that flashes and runs the chip via OpenOCD

set -e

ELF_FILE="$1"

if [ -z "$ELF_FILE" ]; then
    echo "Usage: $0 <elf-file>"
    exit 1
fi

# Derive paths from ELF file location
# ELF is at target/<target>/<profile>/<name>
ELF_DIR=$(dirname "$ELF_FILE")
BOOTLOADER="${ELF_DIR}/bootloader.bin"
PARTITION_TABLE="${ELF_DIR}/partition-table.bin"
BINARY="${ELF_DIR}/net-idf.bin"

# Flash bootloader at 0x0, partition table at 0x8000, and app at 0x10000
openocd -f board/esp32s3-ftdi.cfg \
    -c "program_esp $BOOTLOADER 0x0 verify" \
    -c "program_esp $PARTITION_TABLE 0x8000 verify" \
    -c "program_esp $BINARY 0x10000 verify reset exit"

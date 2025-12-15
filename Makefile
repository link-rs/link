.PHONY: flash-ui flash-mgmt flash-net clean

# UI chip (STM32F405RG - Cortex-M4F)
UI_TARGET = thumbv7em-none-eabihf
UI_ELF = ui/target/$(UI_TARGET)/release/ui
UI_BIN = ui/target/$(UI_TARGET)/release/ui.bin

# MGMT chip (STM32F072CB - Cortex-M0)
MGMT_TARGET = thumbv6m-none-eabi
MGMT_ELF = mgmt/target/$(MGMT_TARGET)/release/mgmt
MGMT_BIN = mgmt/target/$(MGMT_TARGET)/release/mgmt.bin

# NET chip (ESP32-S3) - firmware built separately via ESP-IDF
# Default path to NET firmware build directory
NET_BUILD_DIR ?= ../hactar/firmware/net/build
NET_BIN = $(NET_BUILD_DIR)/net.bin

# ELF files (always rebuild via cargo)
$(UI_ELF): FORCE
	cd ui && cargo build --release --target $(UI_TARGET)

$(MGMT_ELF): FORCE
	cd mgmt && cargo build --release --target $(MGMT_TARGET)

# Binary files
$(UI_BIN): $(UI_ELF)
	cd ui && cargo objcopy --release --target $(UI_TARGET) -- -O binary ../$(UI_BIN)

$(MGMT_BIN): $(MGMT_ELF)
	cd mgmt && cargo objcopy --release --target $(MGMT_TARGET) -- -O binary ../$(MGMT_BIN)

$(NET_BIN): FORCE
	cd net && cargo build --release

# Flash targets
flash-ui: $(UI_BIN)
	cd ctl && cargo run -- ui flash ../ui.bin

flash-mgmt: $(MGMT_BIN)
	cd ctl && cargo run -- mgmt flash ../mgmt.bin

# Flash NET chip (ESP32-S3)
# WARNING: Uses default app address 0x10000. Override NET_BUILD_DIR if needed.
# Example: make flash-net NET_BUILD_DIR=/path/to/build
flash-net: $(NET_BIN)
	@test -f $(NET_BIN) || (echo "Error: $(NET_BIN) not found. Set NET_BUILD_DIR to your ESP-IDF build directory." && exit 1)
	cd ctl && cargo run -- net flash $(abspath $(NET_BIN)) -c --no-verify

FORCE:

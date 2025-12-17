.PHONY: flash-ui flash-mgmt flash-net clean web-ctl serve-web

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
	cd ctl && cargo run -- ui flash ../$(UI_BIN)

flash-mgmt: $(MGMT_BIN)
	cd ctl && cargo run -- mgmt flash ../$(MGMT_BIN)

# Flash NET chip (ESP32-S3)
# WARNING: Uses default app address 0x10000. Override NET_BUILD_DIR if needed.
# Example: make flash-net NET_BUILD_DIR=/path/to/build
flash-net: $(NET_BIN)
	@test -f $(NET_BIN) || (echo "Error: $(NET_BIN) not found. Set NET_BUILD_DIR to your ESP-IDF build directory." && exit 1)
	cd ctl && cargo run -- net flash $(abspath $(NET_BIN)) -c --no-verify

# Web CTL (WASM)
web-ctl: $(UI_BIN) $(MGMT_BIN) $(NET_BIN)
	cd web-ctl && wasm-pack build --target web --out-dir www/pkg
	mkdir -p web-ctl/www/firmware
	cp $(UI_BIN) web-ctl/www/firmware/
	cp $(MGMT_BIN) web-ctl/www/firmware/
	@if [ -f $(NET_BIN) ]; then cp $(NET_BIN) web-ctl/www/firmware/; fi

# Build web-ctl without firmware (for quick iteration)
web-ctl-quick:
	cd web-ctl && wasm-pack build --target web --out-dir www/pkg

serve-web: web-ctl
	@echo "Serving at http://localhost:8080"
	cd web-ctl/www && python3 -m http.server 8080

clean:
	cd ui && cargo clean
	cd mgmt && cargo clean
	cd net && cargo clean
	cd ctl && cargo clean
	cd web-ctl && cargo clean
	rm -rf web-ctl/www/pkg
	rm -rf web-ctl/www/firmware

FORCE:

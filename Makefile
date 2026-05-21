.PHONY: all preflight format flash-ui flash-mgmt flash-mgmt-dfu flash-net flash-all clean web-ctl serve-web web-link serve-link export-web-ctl export-web-link export ctl test-ctl mgmt-dfu

CRATES = ui mgmt net ctl link web-ctl web-link echo-server mgmt-dfu

# Output paths (targets configured in each crate's .cargo/config.toml)
UI_BIN = ui/target/thumbv7em-none-eabihf/debug/ui.bin
MGMT_BIN = mgmt/target/thumbv6m-none-eabi/debug/mgmt.bin
MGMT_DFU_BIN = mgmt-dfu/target/thumbv6m-none-eabi/release/mgmt-dfu.bin
NET_BIN = net/target/xtensa-esp32s3-espidf/debug/net
NET_PARTITIONS = net/partitions.csv

# Build all firmwares and control program
all: $(UI_BIN) $(MGMT_BIN) $(NET_BIN) ctl web-ctl
	@echo "Build complete."

# Preflight checks: build everything, run tests, check formatting
preflight: all
	cd link && cargo test --features std
	@for crate in $(CRATES); do \
		echo "Checking format: $$crate"; \
		(cd $$crate && cargo fmt --check) || exit 1; \
	done
	@echo "Preflight checks passed."

# Format all crates
format:
	@for crate in $(CRATES); do \
		echo "Formatting: $$crate"; \
		(cd $$crate && cargo fmt); \
	done
	@echo "Formatting complete."

# Firmware binaries
$(UI_BIN): FORCE
	cd ui && cargo objcopy -- -O binary target/thumbv7em-none-eabihf/debug/ui.bin

$(MGMT_BIN): FORCE
	cd mgmt && cargo objcopy -- -O binary target/thumbv6m-none-eabi/debug/mgmt.bin

$(MGMT_DFU_BIN): FORCE
	cd mgmt-dfu && cargo objcopy --release -- -O binary target/thumbv6m-none-eabi/release/mgmt-dfu.bin

$(NET_BIN): FORCE
	cd net && cargo build

# CTL program
ctl: FORCE
	cd ctl && cargo build

# Flash targets
flash-ui: $(UI_BIN)
	cd ctl && cargo run -- ui flash ../$(UI_BIN)

flash-mgmt: $(MGMT_BIN)
	cd ctl && cargo run -- mgmt flash ../$(MGMT_BIN)

# Self-update: ask the currently-running mgmt-dfu firmware to detach into
# the ROM bootloader, then write a new image and tell the ROM to jump to
# it. No BOOT0 toggling. Requires the device to already be running
# mgmt-dfu (enumerated as c0de:cafb).
flash-mgmt-dfu: $(MGMT_DFU_BIN)
	dfu-util -d c0de:cafb -e
	@echo "Waiting for ROM bootloader to enumerate..."
	@sleep 2
	dfu-util -d 0483:df11 --alt 0 --dfuse-address 0x08000000:mass-erase:force:leave -D $(MGMT_DFU_BIN)

# Flash NET chip (ESP32-S3) - uses ESP-IDF based firmware
# Uses partition table for proper 8MB flash layout with 4MB app partition
flash-net: $(NET_BIN)
	cd ctl && cargo run -- net flash $(abspath $(NET_BIN)) --partition-table $(abspath $(NET_PARTITIONS))

# Flash all chips in sequence: MGMT, UI, NET
flash-all: $(MGMT_BIN) $(UI_BIN) $(NET_BIN)
	@echo "Flashing MGMT..."
	cd ctl && cargo run -- mgmt flash ../$(MGMT_BIN)
	@echo "Flashing UI..."
	cd ctl && cargo run -- ui flash ../$(UI_BIN)
	@echo "Flashing NET..."
	cd ctl && cargo run -- net flash $(abspath $(NET_BIN)) --partition-table $(abspath $(NET_PARTITIONS))
	@echo "All chips flashed."

# Web CTL (WASM)
web-ctl: $(UI_BIN) $(MGMT_BIN) $(NET_BIN)
	cd web-ctl && wasm-pack build --target web --out-dir www/pkg
	mkdir -p web-ctl/www/firmware
	cp $(UI_BIN) web-ctl/www/firmware/
	cp $(MGMT_BIN) web-ctl/www/firmware/
	@if [ -f $(NET_BIN) ]; then cp $(NET_BIN) web-ctl/www/firmware/; fi
	cp $(NET_PARTITIONS) web-ctl/www/firmware/

# Build web-ctl without firmware (for quick iteration)
web-ctl-quick:
	cd web-ctl && wasm-pack build --target web --out-dir www/pkg

serve-ctl: web-ctl
	@echo "Serving at http://localhost:8000"
	cd web-ctl/www && python3 -m http.server 8000

# Web Link (Virtual device in WASM)
web-link:
	cd web-link && wasm-pack build --target web --out-dir www/pkg

serve-link: web-link
	@echo "Serving at http://localhost:8081"
	cd web-link/www && python3 -m http.server 8081

# Hardware integration tests (requires connected EV16 device)
test-ctl: ctl
	./ctl/test.sh

clean:
	cd link && cargo clean
	cd ui && cargo clean
	cd mgmt && cargo clean
	cd mgmt-dfu && cargo clean
	cd net && cargo clean
	cd ctl && cargo clean
	cd web-ctl && cargo clean
	-cd web-link && cargo clean
	rm -rf web-ctl/www/pkg
	rm -rf web-ctl/www/firmware
	rm -rf web-link/www/pkg
	rm -f web-ctl.tar.gz web-link.tar.gz

# Export targets - create distributable .tar.gz archives for static hosting
export-web-ctl: web-ctl
	@echo "Creating web-ctl.tar.gz..."
	cd web-ctl/www && tar -czf ../../web-ctl.tar.gz --exclude='./.*' *
	@ls -lh web-ctl.tar.gz

export-web-link: web-link
	@echo "Creating web-link.tar.gz..."
	cd web-link/www && tar -czf ../../web-link.tar.gz --exclude='./.*' *
	@ls -lh web-link.tar.gz

export: export-web-ctl export-web-link
	@echo "Export complete."

FORCE:

.PHONY: all preflight format flash-ui flash-mgmt flash-net flash-net-idf clean web-ctl serve-web web-link serve-link export-web-ctl export-web-link export ctl

CRATES = ui mgmt net ctl link web-ctl web-link bootloader echo-server

# Output paths (targets configured in each crate's .cargo/config.toml)
UI_BIN = ui/target/thumbv7em-none-eabihf/debug/ui.bin
MGMT_BIN = mgmt/target/thumbv6m-none-eabi/debug/mgmt.bin
NET_BIN = net/target/xtensa-esp32s3-none-elf/debug/net
NET_IDF_BIN = net-idf/target/xtensa-esp32s3-espidf/debug/net-idf
NET_IDF_PARTITIONS = net-idf/partitions.csv

# Build everything: all firmwares, ctl, and web apps
all: $(UI_BIN) $(MGMT_BIN) $(NET_BIN) ctl web-ctl web-link
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

# Flash NET chip (ESP32-S3) - uses net/ crate (bare-metal)
flash-net: $(NET_BIN)
	cd ctl && cargo run -- net flash $(abspath $(NET_BIN))

# Flash NET-IDF chip (ESP32-S3) - uses net-idf/ crate (ESP-IDF based)
# Uses partition table for proper 8MB flash layout with 4MB app partition
$(NET_IDF_BIN): FORCE
	cd net-idf && cargo build

flash-net-idf: $(NET_IDF_BIN)
	cd ctl && cargo run -- net flash $(abspath $(NET_IDF_BIN)) --partition-table $(abspath $(NET_IDF_PARTITIONS))

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

serve-ctl: web-ctl
	@echo "Serving at http://localhost:8080"
	cd web-ctl/www && python3 -m http.server 8080

# Web Link (Virtual device in WASM)
web-link:
	cd web-link && wasm-pack build --target web --out-dir www/pkg

serve-link: web-link
	@echo "Serving at http://localhost:8081"
	cd web-link/www && python3 -m http.server 8081

clean:
	cd ui && cargo clean
	cd mgmt && cargo clean
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

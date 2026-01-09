# Build Instructions for quicr-rs

## Default Build (macOS/Linux with libquicr from source)

```bash
# Clone with submodules
git clone --recursive https://github.com/suhasHere/quicr-rs.git
cd quicr-rs

# Or if already cloned:
git submodule update --init --recursive

# Build (default features: std + mbedtls)
# Note: requires mbedtls installed (brew install mbedtls on macOS)
cargo build
```

## Feature Flags

| Feature | Description |
|---------|-------------|
| `ffi-stub` | Mock implementations (no C++ toolchain needed) |
| `prebuilt-esp32s3` | Use prebuilt libs from `vendor/prebuilt/esp32s3/` |
| `prebuilt-esp32s3-std` | Prebuilt libs with ESP-IDF std support |
| `espidf-build` | Build libquicr via Docker for ESP32 bare-metal |
| `espidf-std` | Build via Docker with ESP-IDF std |
| `esp-idf-native` | Build using host ESP-IDF toolchain |
| `esp32s3`, `esp32c3`, etc. | ESP32 chip variants |
| `mbedtls` | MbedTLS for crypto (default) |

## ESP32 Builds

```bash
# Using Docker (builds libquicr for ESP32-S3)
./scripts/docker-build-esp32s3.sh

# Build Rust with prebuilt libs
cargo build --no-default-features --features "prebuilt-esp32s3,esp32s3,mbedtls"

# Or with native ESP-IDF toolchain
cargo build --no-default-features --features "esp-idf-native,mbedtls"
```

## Development (stub mode, no C++ deps)

```bash
cargo build --no-default-features --features "std,ffi-stub"
```

## Running Examples

Examples require a relay server or stub mode:

```bash
# With stub mode (mock FFI, no relay needed)
cargo run --example pubsub --no-default-features --features "std,ffi-stub" -- --mode publish

# With real FFI (requires mbedtls and relay server running)
# First start relay: cd vendor/libquicr && make && ./build/cmd/examples/qServer -p 4433
cargo run --example pubsub -- --mode publish
```

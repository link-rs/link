# Build Instructions for quicr

## Desktop Build (macOS/Linux)

Builds libquicr from source using cmake.

```bash
# Clone with submodules
git clone --recursive <repo-url>
cd quicr

# Or if already cloned:
git submodule update --init --recursive

# Build (default features: std + mbedtls)
cargo build
```

## Feature Flags

| Feature | Description |
|---------|-------------|
| `std` | Standard library support (default) |
| `mbedtls` | Use MbedTLS for crypto (default) |
| `openssl` | Use OpenSSL for crypto |
| `boringssl` | Use BoringSSL for crypto |
| `esp-idf` | ESP-IDF build via component system |

## ESP-IDF Build

For ESP32 with ESP-IDF framework, use the `esp-idf` feature. This integrates with ESP-IDF's build system - libquicr is built as an ESP-IDF component, not by this crate's build.rs.

### Setup

1. Add the ESP-IDF component to your project:
   ```
   # In your project's build configuration
   EXTRA_COMPONENT_DIRS=/path/to/quicr/esp-component
   ```

2. Enable the feature in Cargo.toml:
   ```toml
   [dependencies]
   quicr = { path = "../vendor/quicr", features = ["esp-idf"] }
   ```

3. Build with cargo as normal - esp-idf-sys will build libquicr as a component.

### How it works

```
┌─────────────────────────────────────────────────────┐
│  Your ESP-IDF Rust Project                          │
│    Cargo.toml: quicr = { features = ["esp-idf"] }
└─────────────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────┐
│  esp-idf-sys builds all components including:       │
│    - freertos, lwip, mbedtls, pthread (ESP-IDF)     │
│    - libquicr (via esp-component/CMakeLists.txt)    │
└─────────────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────┐
│  quicr crate build.rs                               │
│    - Only generates Rust bindings (bindgen)         │
│    - No C++ compilation                             │
│    - No link directives (handled by esp-idf-sys)    │
└─────────────────────────────────────────────────────┘
```

## Running Examples

Examples require a relay server:

```bash
# First start relay server (from libquicr):
cd libquicr && mkdir -p build && cd build
cmake .. && make
./cmd/examples/qServer -p 4433

# Run example:
cargo run --example pubsub -- --mode publish
```

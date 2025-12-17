#!/bin/bash
set -e

echo "Building web-ctl WASM module..."

# Check for wasm-pack
if ! command -v wasm-pack &> /dev/null; then
    echo "wasm-pack not found. Installing..."
    cargo install wasm-pack
fi

# Build the WASM module
wasm-pack build --target web --out-dir www/pkg

echo ""
echo "Build complete! Output in www/pkg/"
echo ""
echo "To test locally, run a web server in the www directory:"
echo "  cd www && python3 -m http.server 8080"
echo ""
echo "Then open http://localhost:8080 in Chrome/Edge"
echo "(WebSerial requires HTTPS or localhost)"

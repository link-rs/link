//! Simple Relay Server Example
//!
//! This example shows how the relay server concept works with quicr.
//! Note: The actual relay functionality is provided by libquicr's server component.
//! This is a placeholder showing the intended usage pattern.
//!
//! For a production relay, use the qServer binary from libquicr directly.

use std::env;

fn main() {
    println!("quicr.rs Relay Server");
    println!("=====================");
    println!();
    println!("Note: This crate provides client-side bindings only.");
    println!("For a relay server, use the qServer binary from libquicr:");
    println!();
    println!("  cd libquicr");
    println!("  make");
    println!("  ./build/cmd/examples/qServer -p 4433");
    println!();
    println!("Or run via cargo in the libquicr source:");
    println!();
    println!("  cd libquicr/cmd/examples");
    println!("  ./server --bind 0.0.0.0 --port 4433");
    println!();

    // Print help for the chat example
    println!("To test with the chat example:");
    println!();
    println!("  1. Start the relay server (see above)");
    println!("  2. In terminal 1: cargo run --example chat -- --mode subscribe --room test");
    println!(
        "  3. In terminal 2: cargo run --example chat -- --mode publish --room test --user alice"
    );
    println!();

    // Check for --build flag to actually build the server
    let args: Vec<String> = env::args().collect();
    if args.contains(&"--build".to_string()) {
        println!("Building libquicr...");
        let status = std::process::Command::new("make")
            .current_dir("libquicr")
            .status()
            .expect("Failed to run make");

        if status.success() {
            println!("Build successful!");
            println!("Run: ./libquicr/build/cmd/examples/qServer -p 4433");
        } else {
            println!("Build failed!");
        }
    }
}

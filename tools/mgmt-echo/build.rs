use std::env;

fn main() {
    // Read BAUD_RATE environment variable, default to 460800
    let baud_rate = env::var("BAUD_RATE").unwrap_or_else(|_| "460800".to_string());

    println!("cargo:rustc-env=BAUD_RATE={}", baud_rate);
    println!("cargo:rerun-if-env-changed=BAUD_RATE");

    built::write_built_file().expect("Failed to acquire build-time information");
}

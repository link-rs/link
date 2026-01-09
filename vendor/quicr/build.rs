// SPDX-FileCopyrightText: Copyright (c) 2024 QuicR Contributors
// SPDX-License-Identifier: BSD-2-Clause

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if cfg!(feature = "esp-idf-component") {
        build_esp_idf_component();
    } else {
        build_native();
    }
}

/// Build libquicr from source using cmake and generate bindings.
/// Used for desktop platforms (macOS, Linux, etc.)
fn build_native() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let libquicr_dir = manifest_dir.join("libquicr");

    println!("cargo:rerun-if-changed=libquicr/src");
    println!("cargo:rerun-if-changed=libquicr/include");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let profile = env::var("PROFILE").unwrap_or_else(|_| "release".to_string());
    let is_debug = profile == "debug";

    let use_mbedtls = cfg!(feature = "mbedtls");
    let use_openssl = cfg!(feature = "openssl") && !use_mbedtls;
    let use_boringssl = cfg!(feature = "boringssl") && !use_mbedtls;

    // Build libquicr
    let libquicr_build = {
        let cmake_build_type = if is_debug { "Debug" } else { "Release" };

        let mut config = cmake::Config::new(&libquicr_dir);
        config
            .define("QUICR_BUILD_TESTS", "OFF")
            .define("quicr_BUILD_BENCHMARKS", "OFF")
            .define("QUICR_BUILD_C_BRIDGE", "OFF")
            .define("QUICR_BUILD_SHARED", "OFF")
            .define("CMAKE_BUILD_TYPE", cmake_build_type);

        if target_os == "macos" {
            config
                .define("CMAKE_OSX_ARCHITECTURES", "arm64")
                .define("CMAKE_OSX_DEPLOYMENT_TARGET", "14.0");
        }

        if use_mbedtls {
            config.define("USE_MBEDTLS", "ON");
        }
        if use_openssl {
            config.define("USE_OPENSSL", "ON");
        }
        if use_boringssl {
            config.define("USE_BORINGSSL", "ON");
        }

        config.build()
    };

    // Link libraries
    println!(
        "cargo:rustc-link-search=native={}/lib",
        libquicr_build.display()
    );
    println!(
        "cargo:rustc-link-search=native={}/lib64",
        libquicr_build.display()
    );
    println!(
        "cargo:rustc-link-search=native={}/build/src",
        libquicr_build.display()
    );
    println!(
        "cargo:rustc-link-search=native={}/build/dependencies/picoquic",
        libquicr_build.display()
    );
    println!(
        "cargo:rustc-link-search=native={}/build/dependencies/picotls",
        libquicr_build.display()
    );
    println!(
        "cargo:rustc-link-search=native={}/build/dependencies/mbedtls/library",
        libquicr_build.display()
    );
    println!(
        "cargo:rustc-link-search=native={}/build/dependencies/spdlog",
        libquicr_build.display()
    );

    println!("cargo:rustc-link-lib=static=quicr");
    println!("cargo:rustc-link-lib=static=picoquic-core");
    println!("cargo:rustc-link-lib=static=picoquic-log");
    println!("cargo:rustc-link-lib=static=picohttp-core");
    println!("cargo:rustc-link-lib=static=picotls-core");
    println!("cargo:rustc-link-lib=static=picotls-minicrypto");

    if use_mbedtls {
        println!("cargo:rustc-link-lib=static=picotls-mbedtls");
        println!(
            "cargo:rustc-link-search=native={}/build/dependencies/mbedtls/library",
            libquicr_build.display()
        );
        println!(
            "cargo:rustc-link-search=native={}/build/dependencies/mbedtls/tf-psa-crypto/core",
            libquicr_build.display()
        );
        println!(
            "cargo:rustc-link-search=native={}/build/dependencies/mbedtls/tf-psa-crypto/drivers/builtin",
            libquicr_build.display()
        );
        println!(
            "cargo:rustc-link-search=native={}/build/dependencies/mbedtls/tf-psa-crypto/drivers/everest",
            libquicr_build.display()
        );
        println!(
            "cargo:rustc-link-search=native={}/build/dependencies/mbedtls/tf-psa-crypto/drivers/p256-m",
            libquicr_build.display()
        );
        println!("cargo:rustc-link-lib=static=mbedtls");
        println!("cargo:rustc-link-lib=static=mbedx509");
        println!("cargo:rustc-link-lib=static=tfpsacrypto");
        println!("cargo:rustc-link-lib=static=builtin");
        println!("cargo:rustc-link-lib=static=everest");
        println!("cargo:rustc-link-lib=static=p256m");
    } else {
        println!("cargo:rustc-link-lib=static=picotls-openssl");
        if let Ok(lib) = pkg_config::Config::new().probe("openssl") {
            for path in lib.link_paths {
                println!("cargo:rustc-link-search=native={}", path.display());
            }
        } else if target_os == "macos" {
            println!("cargo:rustc-link-search=native=/opt/homebrew/opt/openssl@3/lib");
            println!("cargo:rustc-link-search=native=/usr/local/opt/openssl@3/lib");
        }
        println!("cargo:rustc-link-lib=ssl");
        println!("cargo:rustc-link-lib=crypto");
    }

    if is_debug {
        println!("cargo:rustc-link-lib=static=spdlogd");
    } else {
        println!("cargo:rustc-link-lib=static=spdlog");
    }

    match target_os.as_str() {
        "macos" => {
            println!("cargo:rustc-link-lib=framework=Security");
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=c++");
        }
        "linux" => {
            println!("cargo:rustc-link-lib=stdc++");
            println!("cargo:rustc-link-lib=pthread");
        }
        _ => {}
    }

    // Generate bindings
    let c_bridge_header = libquicr_dir.join("c-bridge/include/quicr/quicr_bridge.h");

    let mut builder = bindgen::Builder::default()
        .header(c_bridge_header.to_string_lossy())
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_function("quicr_.*")
        .allowlist_type("Quicr.*")
        .allowlist_var("QUICR_.*")
        .derive_debug(true)
        .derive_default(true)
        .derive_eq(true)
        .derive_hash(true)
        .clang_arg(format!("-I{}", libquicr_dir.join("include").display()))
        .clang_arg(format!(
            "-I{}",
            libquicr_dir.join("c-bridge/include").display()
        ));

    if target_os == "macos" && target_arch == "aarch64" {
        builder = builder.clang_arg("--target=aarch64-apple-darwin");
    }

    builder
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

/// Generate bindings for ESP-IDF component build.
/// The C++ libraries are built by esp-idf-sys via extra_components.
fn build_esp_idf_component() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let libquicr_dir = manifest_dir.join("libquicr");
    let c_bridge_header = libquicr_dir.join("c-bridge/include/quicr/quicr_bridge.h");

    println!("cargo:warning=quicr: esp-idf-component build, expecting libquicr from ESP-IDF");

    // Use 32-bit ARM target for bindgen since ESP32 is 32-bit.
    // Using 64-bit host target causes struct size mismatches.
    bindgen::Builder::default()
        .header(c_bridge_header.to_string_lossy())
        .clang_arg(format!("-I{}", libquicr_dir.join("include").display()))
        .clang_arg(format!(
            "-I{}",
            libquicr_dir.join("c-bridge/include").display()
        ))
        .allowlist_function("quicr_.*")
        .allowlist_type("Quicr.*")
        .allowlist_var("QUICR_.*")
        .derive_debug(true)
        .derive_default(true)
        .derive_eq(true)
        .derive_hash(true)
        .clang_arg("--target=arm-unknown-linux-gnueabi")
        .clang_arg("-DESP_PLATFORM=1")
        .clang_arg("-DPLATFORM_ESP_IDF=1")
        .clang_arg("-DUSE_MBEDTLS=1")
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

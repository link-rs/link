// SPDX-FileCopyrightText: Copyright (c) 2024 QuicR Contributors
// SPDX-License-Identifier: BSD-2-Clause

fn main() {}

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let libquicr_dir = manifest_dir.join("libquicr");

    println!("cargo:rerun-if-changed=ffi/src/quicr_ffi.cpp");
    println!("cargo:rerun-if-changed=ffi/include/quicr_ffi.h");
    println!("cargo:rerun-if-changed=build.rs");

    // Check for mutually exclusive features
    let ffi_stub = cfg!(feature = "ffi-stub");
    let prebuilt_esp32s3 = cfg!(feature = "prebuilt-esp32s3");
    let prebuilt_esp32s3_std = cfg!(feature = "prebuilt-esp32s3-std");
    let espidf_build = cfg!(feature = "espidf-build");
    let espidf_std = cfg!(feature = "espidf-std");
    let esp_idf_native = cfg!(feature = "esp-idf-native");
    let esp_idf_component = cfg!(feature = "esp-idf-component");

    let exclusive_count = [
        ffi_stub,
        prebuilt_esp32s3,
        prebuilt_esp32s3_std,
        espidf_build,
        espidf_std,
        esp_idf_native,
        esp_idf_component,
    ]
    .iter()
    .filter(|&&x| x)
    .count();

    if exclusive_count > 1 {
        panic!(
            "quicr: Features 'ffi-stub', 'prebuilt-esp32s3', 'prebuilt-esp32s3-std', 'espidf-build', \
             'espidf-std', 'esp-idf-native', and 'esp-idf-component' are mutually exclusive. Only enable one at a time."
        );
    }

    // Check for ffi-stub feature - skip C++ build if enabled
    // The stub implementations are in src/ffi_stub.rs, no need to generate bindings
    if ffi_stub {
        println!("cargo:warning=quicr: ffi-stub feature enabled, using mock FFI implementations from src/ffi_stub.rs");
        println!("cargo:rustc-cfg=ffi_stub");
        return;
    }

    // Check for prebuilt-esp32s3 feature - use prebuilt static libraries (bare-metal)
    if prebuilt_esp32s3 {
        link_prebuilt_esp32s3(&manifest_dir, &out_dir, false);
        return;
    }

    // Check for prebuilt-esp32s3-std feature - use prebuilt static libraries (ESP-IDF std)
    if prebuilt_esp32s3_std {
        link_prebuilt_esp32s3(&manifest_dir, &out_dir, true);
        return;
    }

    // Check for espidf-build feature - build from source using Docker (bare-metal)
    if espidf_build {
        build_with_espidf_docker(&manifest_dir, &out_dir, false);
        return;
    }

    // Check for espidf-std feature - build from source using Docker (ESP-IDF std)
    if espidf_std {
        build_with_espidf_docker(&manifest_dir, &out_dir, true);
        return;
    }

    // Check for esp-idf-native feature - build from source using native ESP-IDF toolchain
    if esp_idf_native {
        build_native_espidf(&manifest_dir, &out_dir);
        return;
    }

    // Check for esp-idf-component feature - libquicr built via ESP-IDF component system
    // The C++ libraries are built by esp-idf-sys via extra_components, we just need bindings
    if esp_idf_component {
        println!("cargo:warning=quicr: esp-idf-component feature enabled, expecting libquicr from ESP-IDF component");
        // Generate bindings for the FFI header
        generate_espidf_component_bindings(&out_dir);
        return;
    }

    // Track libquicr C++ sources for rebuild on changes
    println!("cargo:rerun-if-changed=libquicr/src");
    println!("cargo:rerun-if-changed=libquicr/include");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    // Detect build profile (debug vs release)
    let profile = env::var("PROFILE").unwrap_or_else(|_| "release".to_string());
    let is_debug = profile == "debug";

    // Detect ESP-IDF platform (std-based, uses ESP-IDF framework)
    let is_esp_idf = cfg!(feature = "esp-idf")
        || cfg!(feature = "esp-idf-hal")
        || env::var("IDF_PATH").is_ok()
        || target_os == "espidf";

    // Detect ESP-HAL platform (bare-metal with lwIP)
    // Target triples: xtensa-esp32-none-elf, riscv32imc-unknown-none-elf, etc.
    let is_esp_hal = cfg!(feature = "esp-hal")
        || (target_arch == "xtensa" && target_os == "none")
        || (target_arch == "riscv32"
            && target_os == "none"
            && env::var("CARGO_CFG_TARGET_VENDOR")
                .map(|v| v.contains("esp"))
                .unwrap_or(false));

    // Combined ESP platform detection
    let is_esp = is_esp_idf || is_esp_hal;

    // Determine TLS backend
    // ESP platforms (both esp-hal and esp-idf) use mbedtls
    let use_mbedtls = cfg!(feature = "mbedtls") || is_esp;
    let use_openssl = cfg!(feature = "openssl") && !use_mbedtls;
    let use_boringssl = cfg!(feature = "boringssl") && !use_mbedtls;

    // Build libquicr using CMake
    // On macOS, we invoke cmake directly to avoid the cmake crate adding
    // --target flags that cause issues with some SDK versions
    let libquicr_build = if target_os == "macos" {
        // Build directly with cmake to avoid target triple issues
        let build_dir = out_dir.join("build");
        std::fs::create_dir_all(&build_dir).expect("Failed to create build directory");

        let cmake_build_type = if is_debug { "Debug" } else { "Release" };

        // Configure
        let mut configure = std::process::Command::new("cmake");
        configure
            .current_dir(&build_dir)
            .arg(&libquicr_dir)
            .arg("-G")
            .arg("Unix Makefiles")
            .arg(format!("-DCMAKE_BUILD_TYPE={}", cmake_build_type))
            .arg("-DCMAKE_OSX_ARCHITECTURES=arm64")
            .arg("-DCMAKE_OSX_DEPLOYMENT_TARGET=14.0")
            .arg("-DQUICR_BUILD_TESTS=OFF")
            .arg("-Dquicr_BUILD_BENCHMARKS=OFF")
            .arg("-DQUICR_BUILD_C_BRIDGE=OFF")
            .arg("-DQUICR_BUILD_SHARED=OFF")
            .arg(format!("-DCMAKE_INSTALL_PREFIX={}", out_dir.display()));

        if use_mbedtls {
            configure.arg("-DUSE_MBEDTLS=ON");
        }
        if use_openssl {
            configure.arg("-DUSE_OPENSSL=ON");
        }
        if use_boringssl {
            configure.arg("-DUSE_BORINGSSL=ON");
        }

        let status = configure.status().expect("Failed to run cmake configure");
        if !status.success() {
            panic!("cmake configure failed");
        }

        // Build
        let status = std::process::Command::new("cmake")
            .current_dir(&build_dir)
            .args(["--build", ".", "--target", "install", "--parallel", "10"])
            .status()
            .expect("Failed to run cmake build");
        if !status.success() {
            panic!("cmake build failed");
        }

        out_dir.clone()
    } else {
        // Use cmake crate for other platforms
        let mut cmake_config = cmake::Config::new(&libquicr_dir);
        let cmake_build_type = if is_debug { "Debug" } else { "Release" };

        cmake_config
            .define("QUICR_BUILD_TESTS", "OFF")
            .define("quicr_BUILD_BENCHMARKS", "OFF")
            .define("QUICR_BUILD_C_BRIDGE", "OFF")
            .define("QUICR_BUILD_SHARED", "OFF")
            .define("CMAKE_BUILD_TYPE", cmake_build_type);

        // Handle TLS backend selection
        if use_mbedtls {
            cmake_config.define("USE_MBEDTLS", "ON");
        }
        if use_openssl {
            cmake_config.define("USE_OPENSSL", "ON");
        }
        if use_boringssl {
            cmake_config.define("USE_BORINGSSL", "ON");
        }

        // ESP32 common configuration (both esp-hal and esp-idf use ESP_PLATFORM)
        if is_esp {
            cmake_config.define("CMAKE_C_FLAGS", "-DESP_PLATFORM=1");
            cmake_config.define("CMAKE_CXX_FLAGS", "-DESP_PLATFORM=1");

            match target_arch.as_str() {
                "xtensa" => {
                    cmake_config.define("CMAKE_SYSTEM_NAME", "Generic");
                    cmake_config.define("CMAKE_SYSTEM_PROCESSOR", "xtensa");
                }
                "riscv32" => {
                    cmake_config.define("CMAKE_SYSTEM_NAME", "Generic");
                    cmake_config.define("CMAKE_SYSTEM_PROCESSOR", "riscv32");
                }
                _ => {}
            }
        }

        // ESP-IDF specific configuration
        if is_esp_idf {
            cmake_config.define("PLATFORM_ESP_IDF", "ON");
            if let Ok(idf_path) = env::var("IDF_PATH") {
                cmake_config.define("IDF_PATH", &idf_path);
                let toolchain_file = format!("{}/tools/cmake/toolchain-esp32.cmake", idf_path);
                if std::path::Path::new(&toolchain_file).exists() {
                    cmake_config.define("CMAKE_TOOLCHAIN_FILE", &toolchain_file);
                }
            }
        }

        // ESP-HAL specific configuration
        if is_esp_hal {
            cmake_config.define("PLATFORM_ESP_HAL", "ON");
            cmake_config.define("QUICR_BAREMETAL", "ON");
        }

        cmake_config.build()
    };

    // Add library search paths - cmake puts libraries in build/src/ and build/dependencies/
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

    // Link libquicr and its dependencies
    println!("cargo:rustc-link-lib=static=quicr");
    println!("cargo:rustc-link-lib=static=picoquic-core");
    println!("cargo:rustc-link-lib=static=picoquic-log");
    println!("cargo:rustc-link-lib=static=picohttp-core");
    println!("cargo:rustc-link-lib=static=picotls-core");
    println!("cargo:rustc-link-lib=static=picotls-minicrypto");

    // TLS backend-specific linking
    if use_mbedtls {
        println!("cargo:rustc-link-lib=static=picotls-mbedtls");

        // Link mbedtls libraries
        if is_esp_idf {
            // ESP-IDF provides mbedtls as part of the framework
            // The libraries are linked through the ESP-IDF build system
            println!("cargo:rustc-link-lib=mbedtls");
            println!("cargo:rustc-link-lib=mbedcrypto");
            println!("cargo:rustc-link-lib=mbedx509");
        } else {
            // Standalone mbedtls build - newer mbedtls uses tf-psa-crypto structure
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
            // New mbedtls uses tfpsacrypto instead of mbedcrypto
            println!("cargo:rustc-link-lib=static=tfpsacrypto");
            println!("cargo:rustc-link-lib=static=builtin");
            println!("cargo:rustc-link-lib=static=everest");
            println!("cargo:rustc-link-lib=static=p256m");
        }
    } else {
        // OpenSSL or BoringSSL
        println!("cargo:rustc-link-lib=static=picotls-openssl");

        // Link OpenSSL (required by picoquic and picotls)
        if let Ok(lib) = pkg_config::Config::new().probe("openssl") {
            for path in lib.link_paths {
                println!("cargo:rustc-link-search=native={}", path.display());
            }
        } else if target_os == "macos" {
            // Fallback to common homebrew locations
            println!("cargo:rustc-link-search=native=/opt/homebrew/opt/openssl@3/lib");
            println!("cargo:rustc-link-search=native=/usr/local/opt/openssl@3/lib");
        }
        println!("cargo:rustc-link-lib=ssl");
        println!("cargo:rustc-link-lib=crypto");
    }

    // Link spdlog (not used on ESP platforms)
    // Note: spdlog uses 'd' suffix for debug builds
    if !is_esp {
        if is_debug {
            println!("cargo:rustc-link-lib=static=spdlogd");
        } else {
            println!("cargo:rustc-link-lib=static=spdlog");
        }
    }

    // Platform-specific linking
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
        "espidf" => {
            // ESP-IDF specific linking
            // Most libraries are handled by the ESP-IDF build system
            // Link newlib C library components if needed
            println!("cargo:rustc-link-lib=c");
            println!("cargo:rustc-link-lib=m");
        }
        "none" => {
            // Bare-metal ESP-HAL linking
            if is_esp_hal {
                // ESP-HAL with lwIP provides minimal libc
                println!("cargo:rustc-link-lib=c");
                println!("cargo:rustc-link-lib=m");
            }
        }
        _ => {}
    }

    // Build the FFI wrapper
    let mut cc_build = cc::Build::new();

    // The build directory contains generated headers like version.h
    let build_include_dir = out_dir.join("build/include");

    cc_build
        .cpp(true)
        .file("ffi/src/quicr_ffi.cpp")
        .include("ffi/include")
        .include(libquicr_dir.join("include"))
        .include(&build_include_dir)
        .include(libquicr_dir.join("dependencies/spdlog/include"))
        .include(libquicr_build.join("include"));

    // Debug build configuration
    if is_debug {
        cc_build
            .flag("-g") // Debug symbols
            .flag("-O0") // Disable optimization for better debugging
            .define("DEBUG", "1");
    }

    // Add include paths for dependencies
    let deps_dir = libquicr_dir.join("dependencies");
    cc_build.include(deps_dir.join("picoquic"));
    cc_build.include(deps_dir.join("picotls/include"));
    cc_build.include(deps_dir.join("mbedtls/include"));

    // Platform-specific compiler flags
    match target_os.as_str() {
        "macos" => {
            cc_build.std("c++20");
            cc_build.flag("-stdlib=libc++");
        }
        "linux" => {
            cc_build.std("c++20");
        }
        "espidf" => {
            // ESP-IDF uses a lower C++ standard and has specific requirements
            cc_build.std("c++17");

            // Add ESP-IDF include paths if available
            if let Ok(idf_path) = env::var("IDF_PATH") {
                cc_build.include(format!("{}/components/mbedtls/mbedtls/include", idf_path));
                cc_build.include(format!("{}/components/mbedtls/port/include", idf_path));
                cc_build.include(format!("{}/components/newlib/platform_include", idf_path));
                cc_build.include(format!("{}/components/freertos/include", idf_path));
                cc_build.include(format!("{}/components/esp_common/include", idf_path));
            }

            // ESP32 specific defines
            cc_build.define("ESP_PLATFORM", "1");
            cc_build.define("IDF_VER", None);

            // Disable features not available on ESP32
            cc_build.define("QUICR_NO_EXCEPTIONS", "1");
        }
        "none" => {
            // Bare-metal ESP-HAL configuration
            cc_build.std("c++17");

            // ESP32 specific defines for bare-metal with lwIP
            cc_build.define("ESP_PLATFORM", "1");
            cc_build.define("ESP_HAL_BAREMETAL", "1");
            cc_build.define("QUICR_NO_EXCEPTIONS", "1");
            cc_build.define("QUICR_BAREMETAL", "1");
        }
        _ => {
            cc_build.std("c++20");
        }
    }

    // TLS backend defines for FFI wrapper
    if use_mbedtls {
        cc_build.define("USE_MBEDTLS", "1");
    }
    if use_openssl {
        cc_build.define("USE_OPENSSL", "1");
    }
    if use_boringssl {
        cc_build.define("USE_BORINGSSL", "1");
    }

    cc_build.compile("quicr_ffi");

    // Generate Rust bindings
    let mut bindgen_builder = bindgen::Builder::default()
        .header("ffi/include/quicr_ffi.h")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_function("quicr_.*")
        .allowlist_type("Quicr.*")
        .allowlist_var("QUICR_.*")
        .derive_debug(true)
        .derive_default(true)
        .derive_eq(true)
        .derive_hash(true);

    // macOS target configuration - use canonical aarch64 triple to avoid arm64/aarch64 mismatch
    if target_os == "macos" && target_arch == "aarch64" {
        bindgen_builder = bindgen_builder.clang_arg("--target=aarch64-apple-darwin");
    }

    // ESP platform specific bindgen configuration (both esp-idf and esp-hal)
    if is_esp {
        bindgen_builder = bindgen_builder
            .use_core()
            .ctypes_prefix("crate::ffi")
            .clang_arg("-DESP_PLATFORM=1");

        // Add target-specific clang args for cross-compilation
        match target_arch.as_str() {
            "xtensa" => {
                bindgen_builder = bindgen_builder.clang_arg("--target=xtensa-esp32-elf");
            }
            "riscv32" => {
                bindgen_builder = bindgen_builder.clang_arg("--target=riscv32-esp-elf");
            }
            _ => {}
        }
    }

    // Additional bare-metal specific bindgen options
    if is_esp_hal {
        bindgen_builder = bindgen_builder
            .clang_arg("-DQUICR_BAREMETAL=1")
            .clang_arg("-DESP_HAL_BAREMETAL=1");
    }

    let bindings = bindgen_builder
        .generate()
        .expect("Unable to generate bindings");

    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

/// Link prebuilt ESP32-S3 static libraries
/// std_mode: true for ESP-IDF std, false for bare-metal
fn link_prebuilt_esp32s3(manifest_dir: &PathBuf, out_dir: &PathBuf, std_mode: bool) {
    let subdir = if std_mode { "esp32s3-std" } else { "esp32s3" };
    let prebuilt_dir = manifest_dir.join("vendor/prebuilt").join(subdir);
    let lib_dir = prebuilt_dir.join("lib");
    let include_dir = prebuilt_dir.join("include");

    let build_script_flag = if std_mode { " --std" } else { "" };

    // Check that prebuilt libraries exist
    if !lib_dir.exists() {
        panic!(
            "quicr: Prebuilt libraries not found at {}. \
             Run ./scripts/docker-build-esp32s3.sh{} to build them, \
             or use 'ffi-stub' feature for development.",
            lib_dir.display(),
            build_script_flag
        );
    }

    // Check for required libraries
    let required_libs = ["quicr", "picoquic-core", "picotls-core"];
    for lib in &required_libs {
        let lib_path = lib_dir.join(format!("lib{}.a", lib));
        if !lib_path.exists() {
            panic!(
                "quicr: Required library {} not found. \
                 Run ./scripts/docker-build-esp32s3.sh{} to rebuild.",
                lib_path.display(),
                build_script_flag
            );
        }
    }

    let mode_str = if std_mode {
        "ESP-IDF std"
    } else {
        "bare-metal"
    };
    println!(
        "cargo:warning=quicr: Using prebuilt ESP32-S3 ({}) libraries from {}",
        mode_str,
        lib_dir.display()
    );

    // Add library search path
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    // Link all static libraries
    // Core libraries
    println!("cargo:rustc-link-lib=static=quicr");
    println!("cargo:rustc-link-lib=static=picoquic-core");
    println!("cargo:rustc-link-lib=static=picoquic-log");
    println!("cargo:rustc-link-lib=static=picohttp-core");
    println!("cargo:rustc-link-lib=static=picotls-core");
    println!("cargo:rustc-link-lib=static=picotls-minicrypto");
    println!("cargo:rustc-link-lib=static=picotls-mbedtls");

    // ESP-IDF provides mbedtls, pthread, lwip at runtime
    if std_mode {
        // For std mode, link against ESP-IDF provided libraries
        println!("cargo:rustc-link-lib=mbedtls");
        println!("cargo:rustc-link-lib=mbedcrypto");
        println!("cargo:rustc-link-lib=mbedx509");
        println!("cargo:rustc-link-lib=pthread");
    }

    // Build FFI wrapper
    let mut cc_build = cc::Build::new();

    cc_build
        .cpp(true)
        .file("ffi/src/quicr_ffi.cpp")
        .include("ffi/include")
        .include(include_dir.join("quicr"))
        .include(&include_dir)
        .define("ESP_PLATFORM", "1")
        .define("USE_MBEDTLS", "1");

    if std_mode {
        // ESP-IDF std mode - can use exceptions, C++20
        cc_build.std("c++20").define("ESP_IDF_STD", "1");
    } else {
        // Bare-metal mode - no exceptions
        cc_build.std("c++17").define("QUICR_NO_EXCEPTIONS", "1");

        // Cross-compilation flags for ESP32-S3 (xtensa)
        let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        if target_arch == "xtensa" {
            cc_build
                .flag("-mlongcalls")
                .flag("-fno-exceptions")
                .flag("-fno-rtti");
        }
    }

    cc_build.compile("quicr_ffi");

    // Generate bindings
    let mut bindgen_builder = bindgen::Builder::default()
        .header("ffi/include/quicr_ffi.h")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_function("quicr_.*")
        .allowlist_type("Quicr.*")
        .allowlist_var("QUICR_.*")
        .derive_debug(true)
        .derive_default(true)
        .derive_eq(true)
        .derive_hash(true)
        .clang_arg("-DESP_PLATFORM=1")
        .clang_arg("-DUSE_MBEDTLS=1");

    if std_mode {
        // ESP-IDF std mode - use std types
        bindgen_builder = bindgen_builder.clang_arg("-DESP_IDF_STD=1");
    } else {
        // Bare-metal mode - use core types
        bindgen_builder = bindgen_builder
            .use_core()
            .ctypes_prefix("crate::ffi")
            .clang_arg("--target=xtensa-esp32-elf");
    }

    let bindings = bindgen_builder
        .generate()
        .expect("Unable to generate bindings");

    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

/// Build libquicr from source using ESP-IDF Docker
/// std_mode: true for ESP-IDF std, false for bare-metal
fn build_with_espidf_docker(manifest_dir: &PathBuf, out_dir: &PathBuf, std_mode: bool) {
    let script_path = manifest_dir.join("scripts/docker-build-esp32s3.sh");

    // Check if Docker is available
    let docker_check = Command::new("docker").arg("--version").output();

    if docker_check.is_err() || !docker_check.unwrap().status.success() {
        let feature = if std_mode {
            "espidf-std"
        } else {
            "espidf-build"
        };
        let prebuilt_feature = if std_mode {
            "prebuilt-esp32s3-std"
        } else {
            "prebuilt-esp32s3"
        };
        panic!(
            "quicr: Docker not found. The '{}' feature requires Docker. \
             Install Docker or use '{}' with pre-built libraries.",
            feature, prebuilt_feature
        );
    }

    // Check if build script exists
    if !script_path.exists() {
        panic!("quicr: Build script not found at {}", script_path.display());
    }

    let mode_str = if std_mode {
        "ESP-IDF std"
    } else {
        "bare-metal"
    };
    println!("cargo:warning=quicr: Building libquicr ({}) with ESP-IDF Docker (this may take several minutes)...", mode_str);

    // Run the Docker build script
    let mut cmd = Command::new("bash");
    cmd.arg(&script_path);
    if std_mode {
        cmd.arg("--std");
    }
    cmd.current_dir(manifest_dir);

    let status = cmd.status().expect("Failed to run docker-build-esp32s3.sh");

    if !status.success() {
        panic!("quicr: Docker build failed. Check the output above for errors.");
    }

    // Now link the built libraries (same as prebuilt)
    link_prebuilt_esp32s3(manifest_dir, out_dir, std_mode);
}

/// Build libquicr from source using native ESP-IDF toolchain (no Docker)
/// Requires ESP-IDF to be installed and IDF_PATH to be set
fn build_native_espidf(manifest_dir: &PathBuf, out_dir: &PathBuf) {
    let libquicr_dir = manifest_dir.join("libquicr");

    // Track libquicr C++ sources for rebuild on changes
    println!("cargo:rerun-if-changed=libquicr/src");
    println!("cargo:rerun-if-changed=libquicr/include");
    println!("cargo:rerun-if-changed=cmake/esp-idf-toolchain.cmake");

    // Get IDF_PATH - required for native ESP-IDF build
    let idf_path = env::var("IDF_PATH").unwrap_or_else(|_| {
        // Try to get from esp-idf-sys's DEP_ESP_IDF_* variables
        env::var("DEP_ESP_IDF_PATH").unwrap_or_else(|_| {
            panic!(
                "quicr: ESP-IDF not found. The 'esp-idf-native' feature requires ESP-IDF.\n\
                 Set IDF_PATH environment variable or ensure esp-idf-sys is a dependency.\n\
                 Install ESP-IDF via: https://docs.espressif.com/projects/esp-idf/en/latest/esp32/get-started/"
            );
        })
    });

    // Detect target architecture from cargo target
    let target = env::var("TARGET").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    // Determine ESP chip variant from target triple
    let esp_chip = if target.contains("esp32s3") || target.contains("xtensa-esp32s3") {
        "esp32s3"
    } else if target.contains("esp32s2") || target.contains("xtensa-esp32s2") {
        "esp32s2"
    } else if target.contains("esp32c3") || target.contains("riscv32imc-esp-espidf") {
        "esp32c3"
    } else if target.contains("esp32c6") {
        "esp32c6"
    } else if target.contains("esp32h2") {
        "esp32h2"
    } else if target.contains("esp32") || target.contains("xtensa-esp32-espidf") {
        "esp32"
    } else {
        // Default to ESP32-S3 if not specified
        println!("cargo:warning=quicr: Could not detect ESP chip from target '{}', defaulting to ESP32-S3", target);
        "esp32s3"
    };

    println!(
        "cargo:warning=quicr: Building libquicr natively for {} using ESP-IDF at {}",
        esp_chip, idf_path
    );

    // Detect compiler paths from ESP-IDF or environment
    let (c_compiler, cxx_compiler) = detect_espidf_compilers(&idf_path, esp_chip, &target_arch);

    // Create build directory
    let build_dir = out_dir.join("libquicr-build");
    std::fs::create_dir_all(&build_dir).expect("Failed to create build directory");

    // Write CMake toolchain file for ESP-IDF cross-compilation
    let toolchain_file =
        write_espidf_toolchain(out_dir, &idf_path, esp_chip, &c_compiler, &cxx_compiler);

    // Create custom FindThreads.cmake for ESP-IDF pthread
    let cmake_modules_dir = build_dir.join("cmake");
    std::fs::create_dir_all(&cmake_modules_dir).expect("Failed to create cmake modules dir");
    write_find_threads_cmake(&cmake_modules_dir, &idf_path);

    // Configure CMake - following hactar project approach
    let mut configure = Command::new("cmake");
    configure
        .current_dir(&build_dir)
        .arg(&libquicr_dir)
        .arg("-G")
        .arg("Unix Makefiles")
        .arg(format!(
            "-DCMAKE_TOOLCHAIN_FILE={}",
            toolchain_file.display()
        ))
        .arg("-DCMAKE_BUILD_TYPE=Release")
        // libquicr options
        .arg("-DQUICR_BUILD_TESTS=OFF")
        .arg("-Dquicr_BUILD_BENCHMARKS=OFF")
        .arg("-DQUICR_BUILD_C_BRIDGE=OFF")
        .arg("-DQUICR_BUILD_SHARED=OFF")
        // ESP-IDF platform flag - tells libquicr to use ESP-IDF's mbedtls
        .arg("-DPLATFORM_ESP_IDF=ON")
        .arg("-DUSE_MBEDTLS=ON")
        .arg(format!("-DIDF_PATH={}", idf_path))
        .arg(format!("-DIDF_TARGET={}", esp_chip))
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", out_dir.display()))
        // Point to our custom cmake modules (FindThreads.cmake)
        .arg(format!(
            "-DCMAKE_MODULE_PATH={}",
            cmake_modules_dir.display()
        ))
        // ESP-IDF mbedtls - use the component's headers
        .arg(format!(
            "-DMBEDTLS_INCLUDE_DIR={}/components/mbedtls/mbedtls/include",
            idf_path
        ))
        .arg(format!(
            "-DMBEDTLS_ROOT_DIR={}/components/mbedtls",
            idf_path
        ))
        // spdlog options for embedded
        .arg("-DSPDLOG_NO_EXCEPTIONS=ON")
        .arg("-DSPDLOG_NO_THREAD_ID=ON")
        .arg("-DSPDLOG_NO_TLS=ON")
        .arg("-DSPDLOG_DISABLE_DEFAULT_LOGGER=ON")
        // picotls options
        .arg("-DWITH_FUSION=OFF")
        .arg("-DWITH_BROTLI=OFF")
        .arg("-DBUILD_TESTS=OFF")
        // picoquic options for ESP-IDF (following libquicr's PLATFORM_ESP_HAL approach)
        .arg("-DBUILD_SOCKLOOP=OFF") // sockloop.c uses Linux-specific headers (sys/prctl.h)
        .arg("-DBUILD_DEMO=OFF") // Demo app not needed
        .arg("-DBUILD_HTTP=ON") // HTTP support for MoQ
        .arg("-DBUILD_LOGREADER=OFF") // Logreader not needed
        // CMake cross-compile settings
        .arg("-DHAVE_FWRITE_UNLOCKED=0")
        .arg("-DCMAKE_CROSSCOMPILING=TRUE")
        // Disable mbedtls test/programs
        .arg("-DENABLE_TESTING=OFF")
        .arg("-DENABLE_PROGRAMS=OFF")
        // Environment
        .env("IDF_PATH", &idf_path);

    // Add sysroot from esp-idf-sys if available
    if let Ok(sysroot) = env::var("DEP_ESP_IDF_SYSROOT") {
        configure.arg(format!("-DCMAKE_SYSROOT={}", sysroot));
    }

    let status = configure.status().expect("Failed to run cmake configure");
    if !status.success() {
        panic!("quicr: CMake configure failed for ESP-IDF native build");
    }

    // Build
    let status = Command::new("cmake")
        .current_dir(&build_dir)
        .args(["--build", ".", "--target", "install", "--parallel"])
        .arg(num_cpus().to_string())
        .status()
        .expect("Failed to run cmake build");
    if !status.success() {
        panic!("quicr: CMake build failed for ESP-IDF native build");
    }

    // Link the built libraries
    link_native_espidf_libs(out_dir, &idf_path);

    // Build FFI wrapper
    build_espidf_ffi_wrapper(
        manifest_dir,
        out_dir,
        &idf_path,
        esp_chip,
        &c_compiler,
        &cxx_compiler,
    );

    // Generate Rust bindings
    generate_espidf_bindings(out_dir, &target_arch);
}

/// Detect ESP-IDF compilers from IDF_PATH or esp-idf-sys
fn detect_espidf_compilers(idf_path: &str, esp_chip: &str, target_arch: &str) -> (String, String) {
    // First try esp-idf-sys provided compiler paths
    if let (Ok(cc), Ok(cxx)) = (env::var("DEP_ESP_IDF_CC"), env::var("DEP_ESP_IDF_CXX")) {
        return (cc, cxx);
    }

    // Determine toolchain directory and binary prefix based on architecture and chip
    // The toolchain directory uses a generic prefix (xtensa-esp-elf or riscv32-esp-elf)
    // but the actual compiler binaries are chip-specific (e.g., xtensa-esp32s3-elf-gcc)
    let (toolchain_dir_prefix, binary_prefix) =
        if target_arch == "xtensa" || esp_chip.starts_with("esp32") {
            // For Xtensa chips (ESP32, ESP32-S2, ESP32-S3), use chip-specific binary prefix
            // to get correct endianness and chip configuration
            let chip_prefix = match esp_chip {
                "esp32s3" => "xtensa-esp32s3-elf",
                "esp32s2" => "xtensa-esp32s2-elf",
                "esp32" => "xtensa-esp32-elf",
                _ => "xtensa-esp32s3-elf", // Default to ESP32-S3
            };
            ("xtensa-esp-elf", chip_prefix)
        } else {
            // RISC-V chips (ESP32-C3, C6, H2)
            ("riscv32-esp-elf", "riscv32-esp-elf")
        };

    // Try to find in IDF_TOOLS_PATH or standard locations
    let idf_tools = env::var("IDF_TOOLS_PATH")
        .unwrap_or_else(|_| format!("{}/.espressif", env::var("HOME").unwrap_or_default()));

    // Try common ESP-IDF toolchain paths
    let search_paths = [
        format!("{}/tools/{}", idf_tools, toolchain_dir_prefix),
        format!("{}/dist/{}", idf_path, toolchain_dir_prefix),
        format!("/opt/esp/{}", toolchain_dir_prefix),
        format!(
            "{}/.espressif/tools/{}",
            env::var("HOME").unwrap_or_default(),
            toolchain_dir_prefix
        ),
    ];

    for base_path in &search_paths {
        // ESP-IDF tools are versioned, find the latest
        if let Ok(entries) = std::fs::read_dir(base_path) {
            for entry in entries.flatten() {
                // The bin directory is inside toolchain_dir_prefix, but binaries use chip-specific prefix
                let bin_dir = entry.path().join(format!("{}/bin", toolchain_dir_prefix));
                let cc_path = bin_dir.join(format!("{}-gcc", binary_prefix));
                let cxx_path = bin_dir.join(format!("{}-g++", binary_prefix));
                if cc_path.exists() && cxx_path.exists() {
                    return (
                        cc_path.to_string_lossy().to_string(),
                        cxx_path.to_string_lossy().to_string(),
                    );
                }
            }
        }
    }

    // Fall back to hoping they're in PATH
    println!("cargo:warning=quicr: ESP-IDF compilers not found in standard locations, trying PATH");
    (
        format!("{}-gcc", binary_prefix),
        format!("{}-g++", binary_prefix),
    )
}

/// Generate a minimal sdkconfig.h and lwipopts.h for ESP-IDF native builds
fn generate_sdkconfig(out_dir: &PathBuf, esp_chip: &str) -> PathBuf {
    let config_dir = out_dir.join("config");
    std::fs::create_dir_all(&config_dir).expect("Failed to create config directory");

    let sdkconfig_path = config_dir.join("sdkconfig.h");

    // Minimal sdkconfig.h with essential defines for compiling ESP-IDF components
    let esp_chip_upper = esp_chip.to_uppercase();
    let sdkconfig_content = format!(
        r#"/*
 * Minimal sdkconfig.h for native ESP-IDF libquicr build
 * Auto-generated by quicr build.rs
 */
#pragma once

/* Target chip */
#define CONFIG_IDF_TARGET "{esp_chip}"
#define CONFIG_IDF_TARGET_{esp_chip_upper} 1

/* SoC capabilities */
#define CONFIG_SOC_SERIES_ESP32S3 1

/* FreeRTOS */
#define CONFIG_FREERTOS_HZ 1000
#define CONFIG_FREERTOS_UNICORE 0
#define CONFIG_FREERTOS_NO_AFFINITY 0x7FFFFFFF
#define CONFIG_FREERTOS_ENABLE_BACKWARD_COMPATIBILITY 1

/* Newlib/libc configuration */
#define CONFIG_NEWLIB_NANO_FORMAT 0
#define CONFIG_NEWLIB_STDOUT_LINE_BUFFERING 1

/* Heap */
#define CONFIG_HEAP_POISONING_DISABLED 1

/* Log */
#define CONFIG_LOG_DEFAULT_LEVEL 3
#define CONFIG_LOG_MAXIMUM_LEVEL 3

/* Compiler */
#define CONFIG_COMPILER_OPTIMIZATION_DEFAULT 1
#define CONFIG_COMPILER_OPTIMIZATION_ASSERTIONS_ENABLE 1
#define CONFIG_COMPILER_FLOAT_LIB_FROM_GCCLIB 1
#define CONFIG_COMPILER_STACK_CHECK_MODE_NONE 1
#define CONFIG_COMPILER_RT_LIB_GCCLIB 1

/* ESP system */
#define CONFIG_ESP_SYSTEM_SINGLE_CORE_MODE 0
#define CONFIG_ESP_SYSTEM_CHECK_INT_LEVEL_5 1

/* mbedTLS */
#define CONFIG_MBEDTLS_HARDWARE_AES 1
#define CONFIG_MBEDTLS_HARDWARE_SHA 1
#define CONFIG_MBEDTLS_TLS_CLIENT_ONLY 0
#define CONFIG_MBEDTLS_SSL_OUT_CONTENT_LEN 4096
#define CONFIG_MBEDTLS_SSL_IN_CONTENT_LEN 16384

/* POSIX */
#define CONFIG_PTHREAD_TASK_PRIO_DEFAULT 5
#define CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT 3072

/* SPI flash */
#define CONFIG_SPI_FLASH_ROM_DRIVER_PATCH 1
"#,
        esp_chip = esp_chip,
        esp_chip_upper = esp_chip_upper,
    );

    std::fs::write(&sdkconfig_path, sdkconfig_content).expect("Failed to write sdkconfig.h");

    // Minimal lwipopts.h to satisfy lwip headers without ESP-IDF dependencies
    let lwipopts_path = config_dir.join("lwipopts.h");
    let lwipopts_content = r#" /*
                            * Minimal lwipopts.h for libquicr native ESP-IDF build
                            * Auto-generated by quicr build.rs
                            */
#pragma once

#define NO_SYS 0
#define LWIP_SOCKET 1
#define LWIP_COMPAT_SOCKETS 0 /* Disable - socket name macros conflict with C++ code */
#define LWIP_POSIX_SOCKETS_IO_NAMES 0 /* Disable - conflicts with spdlog/fmt's write() */
#define LWIP_IPV4 1
#define LWIP_IPV6 1
#define LWIP_UDP 1
#define LWIP_TCP 1
#define LWIP_DNS 1
#define LWIP_NETCONN 1
#define SO_REUSE 1
#define LWIP_SO_RCVTIMEO 1
#define LWIP_SO_SNDTIMEO 1
#define LWIP_TIMEVAL_PRIVATE 0
#define MEMP_NUM_NETCONN 16

/* Disable lwip's select/poll to avoid fd_set conflicts with newlib */
#define LWIP_SOCKET_SELECT 0
#define LWIP_SOCKET_POLL 0

/* Pre-include system select to prevent lwip from defining fd_set */
#include <sys/select.h>
"#;
    std::fs::write(&lwipopts_path, lwipopts_content).expect("Failed to write lwipopts.h");

    // Create arch directory for lwip architecture-specific headers
    let arch_dir = config_dir.join("arch");
    std::fs::create_dir_all(&arch_dir).expect("Failed to create arch directory");

    // Minimal arch/cc.h - lwip compiler/platform abstraction
    let cc_h_path = arch_dir.join("cc.h");
    let cc_h_content = r#" /*
                        * Minimal arch/cc.h for libquicr native ESP-IDF build
                        * Auto-generated by quicr build.rs
                        */
#ifndef __ARCH_CC_H__
#define __ARCH_CC_H__

#include <stdint.h>
#include <errno.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

#ifndef BYTE_ORDER
#define BYTE_ORDER LITTLE_ENDIAN
#endif

#define LWIP_DONT_PROVIDE_BYTEORDER_FUNCTIONS
#define htons(x) __builtin_bswap16(x)
#define ntohs(x) __builtin_bswap16(x)
#define htonl(x) __builtin_bswap32(x)
#define ntohl(x) __builtin_bswap32(x)

#define LWIP_NOASSERT 1

typedef uint8_t  u8_t;
typedef int8_t   s8_t;
typedef uint16_t u16_t;
typedef int16_t  s16_t;
typedef uint32_t u32_t;
typedef int32_t  s32_t;

typedef int sys_prot_t;

#define S16_F "d"
#define U16_F "d"
#define X16_F "x"
#define S32_F "d"
#define U32_F "u"
#define X32_F "x"

#define PACK_STRUCT_FIELD(x) x
#define PACK_STRUCT_STRUCT __attribute__((packed))
#define PACK_STRUCT_BEGIN
#define PACK_STRUCT_END

#define LWIP_PLATFORM_DIAG(x) do {printf x;} while(0)
#define LWIP_PLATFORM_ASSERT(message)

#ifdef __cplusplus
}
#endif

#endif /* __ARCH_CC_H__ */
"#;
    std::fs::write(&cc_h_path, cc_h_content).expect("Failed to write arch/cc.h");

    // Minimal arch/sys_arch.h - lwip system abstraction (stub)
    let sys_arch_h_path = arch_dir.join("sys_arch.h");
    let sys_arch_h_content = r#" /*
                              * Minimal arch/sys_arch.h for libquicr native ESP-IDF build
                              * Auto-generated by quicr build.rs
                              */
#ifndef __ARCH_SYS_ARCH_H__
#define __ARCH_SYS_ARCH_H__

#ifdef __cplusplus
extern "C" {
#endif

/* Minimal definitions - libquicr doesn't use lwip threading directly */
typedef void* sys_sem_t;
typedef void* sys_mutex_t;
typedef void* sys_mbox_t;
typedef int sys_thread_t;

#define sys_mbox_valid(x) ((x) != NULL)
#define sys_mbox_set_invalid(x) ((x) = NULL)
#define sys_sem_valid(x) ((x) != NULL)
#define sys_sem_set_invalid(x) ((x) = NULL)

#define SYS_MBOX_NULL NULL
#define SYS_SEM_NULL NULL

#ifdef __cplusplus
}
#endif

#endif /* __ARCH_SYS_ARCH_H__ */
"#;
    std::fs::write(&sys_arch_h_path, sys_arch_h_content).expect("Failed to write arch/sys_arch.h");

    // Create netinet directory for BSD socket compatibility headers
    // Following hactar project approach - just stub headers that include lwip
    let netinet_dir = config_dir.join("netinet");
    std::fs::create_dir_all(&netinet_dir).expect("Failed to create netinet directory");

    // netinet/udp.h - stub that includes lwip (following hactar approach)
    let udp_h_path = netinet_dir.join("udp.h");
    let udp_h_content = r#" /*
                         * BSD socket compatibility: netinet/udp.h for ESP-IDF
                         * Following hactar project approach - stub that includes lwip
                         * Auto-generated by quicr build.rs
                         */
#ifndef _NETINET_UDP_H
#define _NETINET_UDP_H

#include "lwip/tcp.h"

#endif /* _NETINET_UDP_H */
"#;
    std::fs::write(&udp_h_path, udp_h_content).expect("Failed to write netinet/udp.h");

    // netinet/in.h - stub that includes lwip inet (following hactar approach)
    let in_h_path = netinet_dir.join("in.h");
    let in_h_content = r#" /*
                        * BSD socket compatibility: netinet/in.h for ESP-IDF
                        * Following hactar project approach - stub that includes lwip
                        * Auto-generated by quicr build.rs
                        */
#ifndef _NETINET_IN_H
#define _NETINET_IN_H

#include "lwip/inet.h"
#include "lwip/sockets.h"

#endif /* _NETINET_IN_H */
"#;
    std::fs::write(&in_h_path, in_h_content).expect("Failed to write netinet/in.h");

    // netinet/tcp.h - stub for TCP socket options
    let tcp_h_path = netinet_dir.join("tcp.h");
    let tcp_h_content = r#" /*
                         * BSD socket compatibility: netinet/tcp.h for ESP-IDF
                         * Auto-generated by quicr build.rs
                         */
#ifndef _NETINET_TCP_H
#define _NETINET_TCP_H

#include "lwip/tcp.h"

/* TCP socket options - define if not provided by lwip */
#ifndef TCP_NODELAY
#define TCP_NODELAY 1
#endif

#endif /* _NETINET_TCP_H */
"#;
    std::fs::write(&tcp_h_path, tcp_h_content).expect("Failed to write netinet/tcp.h");

    // Create sys directory for system header stubs
    let sys_dir = config_dir.join("sys");
    std::fs::create_dir_all(&sys_dir).expect("Failed to create sys directory");

    // sys/prctl.h - Linux process control stub (used by sockloop.c)
    // ESP-IDF uses pthread_setname_np() instead of prctl()
    let prctl_h_path = sys_dir.join("prctl.h");
    let prctl_h_content = r#" /*
                           * BSD socket compatibility: sys/prctl.h stub for ESP-IDF
                           * picoquic's sockloop.c uses prctl() to set thread names
                           * On ESP-IDF, we provide stub definitions (thread naming done via pthread)
                           * Auto-generated by quicr build.rs
                           */
#ifndef _SYS_PRCTL_H
#define _SYS_PRCTL_H

/* prctl operations used by picoquic */
#define PR_SET_NAME 15
#define PR_GET_NAME 16

/* prctl stub - returns success but does nothing */
static inline int prctl(int option, ...) {
    (void)option;
    return 0;
}

#endif /* _SYS_PRCTL_H */
"#;
    std::fs::write(&prctl_h_path, prctl_h_content).expect("Failed to write sys/prctl.h");

    // sys/socket.h - BSD socket interface via lwip
    let socket_h_path = sys_dir.join("socket.h");
    let socket_h_content = r#" /*
                            * BSD socket compatibility: sys/socket.h for ESP-IDF
                            * Wraps lwip socket interface
                            * Auto-generated by quicr build.rs
                            */
#ifndef _SYS_SOCKET_H
#define _SYS_SOCKET_H

#include "lwip/sockets.h"

#endif /* _SYS_SOCKET_H */
"#;
    std::fs::write(&socket_h_path, socket_h_content).expect("Failed to write sys/socket.h");

    // Create arpa directory for socket address conversion functions
    let arpa_dir = config_dir.join("arpa");
    std::fs::create_dir_all(&arpa_dir).expect("Failed to create arpa directory");

    // arpa/inet.h - Socket address conversion wrappers for lwip
    // When LWIP_COMPAT_SOCKETS=0, we need to provide function wrappers
    let arpa_inet_h_path = arpa_dir.join("inet.h");
    let arpa_inet_h_content = r#" /*
                               * BSD socket compatibility: arpa/inet.h for ESP-IDF
                               * Provides socket function wrappers when LWIP_COMPAT_SOCKETS=0
                               * Auto-generated by quicr build.rs
                               */
#ifndef _ARPA_INET_H
#define _ARPA_INET_H

#include "lwip/inet.h"
#include "lwip/sockets.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Map standard socket functions to lwip_* versions */
#ifndef inet_ntop
#define inet_ntop(af, src, dst, size) lwip_inet_ntop(af, src, dst, size)
#endif

#ifndef inet_pton
#define inet_pton(af, src, dst) lwip_inet_pton(af, src, dst)
#endif

#ifndef inet_addr
#define inet_addr(cp) ipaddr_addr(cp)
#endif

#ifndef inet_ntoa
#define inet_ntoa(addr) ip4addr_ntoa((const ip4_addr_t*)&(addr))
#endif

/* Socket operations - map to lwip functions */
#ifndef socket
#define socket(domain, type, protocol) lwip_socket(domain, type, protocol)
#endif

#ifndef bind
#define bind(s, name, namelen) lwip_bind(s, name, namelen)
#endif

#ifndef connect
#define connect(s, name, namelen) lwip_connect(s, name, namelen)
#endif

#ifndef listen
#define listen(s, backlog) lwip_listen(s, backlog)
#endif

#ifndef accept
#define accept(s, addr, addrlen) lwip_accept(s, addr, addrlen)
#endif

#ifndef send
#define send(s, data, size, flags) lwip_send(s, data, size, flags)
#endif

#ifndef recv
#define recv(s, mem, len, flags) lwip_recv(s, mem, len, flags)
#endif

#ifndef sendto
#define sendto(s, data, size, flags, to, tolen) lwip_sendto(s, data, size, flags, to, tolen)
#endif

#ifndef recvfrom
#define recvfrom(s, mem, len, flags, from, fromlen) lwip_recvfrom(s, mem, len, flags, from, fromlen)
#endif

#ifndef setsockopt
#define setsockopt(s, level, optname, optval, optlen) lwip_setsockopt(s, level, optname, optval, optlen)
#endif

#ifndef getsockopt
#define getsockopt(s, level, optname, optval, optlen) lwip_getsockopt(s, level, optname, optval, optlen)
#endif

#ifndef getsockname
#define getsockname(s, name, namelen) lwip_getsockname(s, name, namelen)
#endif

#ifndef getpeername
#define getpeername(s, name, namelen) lwip_getpeername(s, name, namelen)
#endif

#ifndef fcntl
#define fcntl(s, cmd, val) lwip_fcntl(s, cmd, val)
#endif

#ifndef ioctl
#define ioctl(s, cmd, argp) lwip_ioctl(s, cmd, argp)
#endif

/* Note: shutdown and close are NOT defined here to avoid spdlog conflicts.
 * Code should use lwip_shutdown() and lwip_close() directly, or include
 * this header BEFORE spdlog headers if these are needed. */

#ifdef __cplusplus
}
#endif

#endif /* _ARPA_INET_H */
"#;
    std::fs::write(&arpa_inet_h_path, arpa_inet_h_content).expect("Failed to write arpa/inet.h");

    config_dir
}

/// Write custom FindThreads.cmake for ESP-IDF pthread
fn write_find_threads_cmake(cmake_dir: &PathBuf, idf_path: &str) {
    let find_threads_path = cmake_dir.join("FindThreads.cmake");
    let content = format!(
        r#"# Custom FindThreads.cmake for ESP-IDF
# Auto-generated by quicr build.rs
# Based on hactar project approach

set(Threads_FOUND TRUE)
set(CMAKE_THREAD_LIBS_INIT "")
set(CMAKE_USE_PTHREADS_INIT TRUE)
set(CMAKE_USE_WIN32_THREADS_INIT FALSE)
set(THREADS_PREFER_PTHREAD_FLAG TRUE)

# ESP-IDF pthread include path
set(IDF_PTHREADS "{idf_path}/components/pthread/include")

# Create imported target
if(NOT TARGET Threads::Threads)
    add_library(Threads::Threads INTERFACE IMPORTED)
    set_target_properties(Threads::Threads PROPERTIES
        INTERFACE_INCLUDE_DIRECTORIES "${{IDF_PTHREADS}}"
    )
endif()
"#,
        idf_path = idf_path
    );
    std::fs::write(&find_threads_path, content).expect("Failed to write FindThreads.cmake");
}

/// Write CMake toolchain file for ESP-IDF cross-compilation
fn write_espidf_toolchain(
    out_dir: &PathBuf,
    idf_path: &str,
    esp_chip: &str,
    c_compiler: &str,
    cxx_compiler: &str,
) -> PathBuf {
    // First generate sdkconfig.h
    let config_dir = generate_sdkconfig(out_dir, esp_chip);
    let toolchain_path = out_dir.join("esp-idf-toolchain.cmake");

    let system_processor = if esp_chip.starts_with("esp32c") || esp_chip == "esp32h2" {
        "riscv"
    } else {
        "xtensa"
    };

    let toolchain_content = format!(
        r#"# ESP-IDF Toolchain file for {esp_chip}
# Auto-generated by quicr build.rs

set(CMAKE_SYSTEM_NAME Generic)
set(CMAKE_SYSTEM_PROCESSOR {system_processor})

# Compilers
set(CMAKE_C_COMPILER "{c_compiler}")
set(CMAKE_CXX_COMPILER "{cxx_compiler}")

# ESP-IDF paths
set(IDF_PATH "{idf_path}")
set(IDF_TARGET "{esp_chip}")

# ESP platform defines
add_compile_definitions(ESP_PLATFORM=1)
add_compile_definitions(IDF_VER="5.3")
add_compile_definitions(CONFIG_IDF_TARGET_{esp_chip_upper}=1)

# spdlog configuration for ESP-IDF (no exceptions, no unlocked stdio)
add_compile_definitions(SPDLOG_NO_EXCEPTIONS=1)
add_compile_definitions(SPDLOG_NO_THREAD_ID=1)
add_compile_definitions(SPDLOG_NO_TLS=1)
add_compile_definitions(SPDLOG_DISABLE_DEFAULT_LOGGER=1)

# mbedtls configuration for ESP-IDF embedded platform
add_compile_definitions(MBEDTLS_NO_PLATFORM_ENTROPY=1)
add_compile_definitions(MBEDTLS_PLATFORM_MS_TIME_ALT=1)

# Generated config directory (sdkconfig.h for mbedtls port if needed)
include_directories(SYSTEM "{config_dir}")

# Include paths for ESP-IDF components (following hactar project)
include_directories(SYSTEM
    ${{IDF_PATH}}/components/mbedtls/mbedtls/include
    ${{IDF_PATH}}/components/mbedtls/port/include
    ${{IDF_PATH}}/components/pthread/include
    ${{IDF_PATH}}/components/lwip/lwip/src/include
    ${{IDF_PATH}}/components/lwip/lwip/src/include/compat/posix
    ${{IDF_PATH}}/components/lwip/port/include
    ${{IDF_PATH}}/components/newlib/platform_include
    ${{IDF_PATH}}/components/esp_common/include
)

# POSIX support for newlib
add_compile_definitions(_POSIX_SOURCE=1)
add_compile_definitions(_DEFAULT_SOURCE=1)

# ESP-IDF platform flag - critical for picoquic/picotls code paths
# Following hactar approach: add_definitions(-DPLATFORM_ESP_IDF)
add_definitions(-DPLATFORM_ESP_IDF)

# Compiler flags - matching hactar project settings
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_EXTENSIONS ON)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_EXTENSIONS ON)
# -mlongcalls is required for ESP32 segmented memory
# -Wno-error flags from hactar for compatibility
set(CMAKE_C_FLAGS "${{CMAKE_C_FLAGS}} -mlongcalls -ffunction-sections -fdata-sections -Wno-error=format -Wno-error")
set(CMAKE_CXX_FLAGS "${{CMAKE_CXX_FLAGS}} -mlongcalls -ffunction-sections -fdata-sections -Wno-error=format -Wno-error=pessimizing-move -Wno-error -frtti")

# Use static libraries
set(BUILD_SHARED_LIBS OFF)

# Skip compiler tests (cross-compiling)
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)

# =============================================================================
# Pre-create mbedtls imported targets to prevent libquicr from building bundled mbedtls
# This makes the "NOT TARGET mbedcrypto" check in dependencies/CMakeLists.txt return FALSE,
# allowing the PLATFORM_ESP_IDF branch to be reached which uses ESP-IDF's mbedtls.
# =============================================================================

# Guard to prevent re-creating targets on subsequent toolchain loads
if(NOT TARGET mbedcrypto)
    # mbedcrypto - core cryptographic functions
    add_library(mbedcrypto INTERFACE IMPORTED GLOBAL)
    set_target_properties(mbedcrypto PROPERTIES
        INTERFACE_INCLUDE_DIRECTORIES "${{IDF_PATH}}/components/mbedtls/mbedtls/include;${{IDF_PATH}}/components/mbedtls/port/include"
    )
endif()

if(NOT TARGET mbedtls)
    # mbedtls - TLS protocol implementation
    add_library(mbedtls INTERFACE IMPORTED GLOBAL)
    set_target_properties(mbedtls PROPERTIES
        INTERFACE_INCLUDE_DIRECTORIES "${{IDF_PATH}}/components/mbedtls/mbedtls/include;${{IDF_PATH}}/components/mbedtls/port/include"
        INTERFACE_LINK_LIBRARIES mbedcrypto
    )
endif()

if(NOT TARGET mbedx509)
    # mbedx509 - X.509 certificate handling
    add_library(mbedx509 INTERFACE IMPORTED GLOBAL)
    set_target_properties(mbedx509 PROPERTIES
        INTERFACE_INCLUDE_DIRECTORIES "${{IDF_PATH}}/components/mbedtls/mbedtls/include;${{IDF_PATH}}/components/mbedtls/port/include"
        INTERFACE_LINK_LIBRARIES mbedcrypto
    )
endif()

if(NOT TARGET tfpsacrypto)
    # Also create tfpsacrypto as alias (newer mbedtls uses this)
    add_library(tfpsacrypto INTERFACE IMPORTED GLOBAL)
    set_target_properties(tfpsacrypto PROPERTIES
        INTERFACE_INCLUDE_DIRECTORIES "${{IDF_PATH}}/components/mbedtls/mbedtls/include"
    )
endif()

# Set MbedTLS_FOUND so FindMbedTLS doesn't search again
set(MbedTLS_FOUND TRUE CACHE BOOL "MbedTLS found (ESP-IDF)" FORCE)
set(MBEDTLS_FOUND TRUE CACHE BOOL "MbedTLS found (ESP-IDF)" FORCE)
"#,
        esp_chip = esp_chip,
        system_processor = system_processor,
        c_compiler = c_compiler,
        cxx_compiler = cxx_compiler,
        idf_path = idf_path,
        esp_chip_upper = esp_chip.to_uppercase(),
        config_dir = config_dir.display(),
    );

    std::fs::write(&toolchain_path, toolchain_content)
        .expect("Failed to write ESP-IDF toolchain file");

    toolchain_path
}

/// Link libraries built with native ESP-IDF
fn link_native_espidf_libs(out_dir: &PathBuf, _idf_path: &str) {
    let build_dir = out_dir.join("libquicr-build");

    // Add library search paths - multiple locations based on CMake install structure
    println!("cargo:rustc-link-search=native={}/lib", out_dir.display());
    println!("cargo:rustc-link-search=native={}/lib64", out_dir.display());
    // Source directory has libquicr.a
    println!("cargo:rustc-link-search=native={}/src", build_dir.display());
    // Dependencies have their own directories
    println!(
        "cargo:rustc-link-search=native={}/dependencies/picoquic",
        build_dir.display()
    );
    println!(
        "cargo:rustc-link-search=native={}/dependencies/picotls",
        build_dir.display()
    );
    println!(
        "cargo:rustc-link-search=native={}/dependencies/spdlog",
        build_dir.display()
    );

    // Link libquicr and its dependencies
    println!("cargo:rustc-link-lib=static=quicr");
    println!("cargo:rustc-link-lib=static=picoquic-core");
    println!("cargo:rustc-link-lib=static=picoquic-log");
    println!("cargo:rustc-link-lib=static=picohttp-core");
    println!("cargo:rustc-link-lib=static=picotls-core");
    println!("cargo:rustc-link-lib=static=picotls-minicrypto");
    println!("cargo:rustc-link-lib=static=picotls-mbedtls");
    // spdlog for logging
    println!("cargo:rustc-link-lib=static=spdlog");

    // ESP-IDF provides mbedtls - the esp-idf-sys crate will provide these at final link time.
    // We do NOT emit link instructions for mbedtls here because:
    // 1. esp-idf-sys already links mbedtls as part of its framework
    // 2. The libraries are in esp-idf-sys's build directory, not accessible from here
    // 3. Emitting link instructions causes "cannot find -lmbedtls" errors
    // The mbedtls symbols are resolved when esp-idf-sys links the final binary.

    // Standard C library from ESP-IDF newlib
    println!("cargo:rustc-link-lib=c");
    println!("cargo:rustc-link-lib=m");

    // Rerun if IDF_PATH changes
    println!("cargo:rerun-if-env-changed=IDF_PATH");
    println!("cargo:rerun-if-env-changed=DEP_ESP_IDF_PATH");
}

/// Build FFI wrapper for ESP-IDF
fn build_espidf_ffi_wrapper(
    manifest_dir: &PathBuf,
    out_dir: &PathBuf,
    idf_path: &str,
    esp_chip: &str,
    _c_compiler: &str,
    cxx_compiler: &str,
) {
    let libquicr_dir = manifest_dir.join("libquicr");

    // Get the cargo target for cc::Build
    let target = env::var("TARGET").unwrap_or_else(|_| "xtensa-esp32s3-espidf".to_string());

    // Find the archiver - derive from compiler name (xtensa-esp32s3-elf-g++ -> xtensa-esp32s3-elf-ar)
    let compiler_name = std::path::Path::new(cxx_compiler)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("xtensa-esp32s3-elf-g++");
    let ar_name = compiler_name.replace("-g++", "-ar").replace("-gcc", "-ar");
    let ar_path = std::path::Path::new(cxx_compiler)
        .parent()
        .map(|p| p.join(&ar_name))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ar_name);

    let mut cc_build = cc::Build::new();

    // Generated config directory contains sdkconfig.h and lwipopts.h
    let config_dir = out_dir.join("config");

    cc_build
        .cpp(true)
        // Set target explicitly for cross-compilation
        .target(&target)
        .file("ffi/src/quicr_ffi.cpp")
        .include("ffi/include")
        .include(libquicr_dir.join("include"))
        .include(out_dir.join("include"))
        // CMake build output includes (version.h is generated here)
        .include(out_dir.join("libquicr-build/include"))
        .include(libquicr_dir.join("dependencies/spdlog/include"))
        // Generated config (sdkconfig.h, lwipopts.h, arch/, netinet/, arpa/)
        .include(&config_dir)
        // ESP-IDF includes
        .include(format!("{}/components/mbedtls/mbedtls/include", idf_path))
        .include(format!("{}/components/mbedtls/port/include", idf_path))
        .include(format!("{}/components/newlib/platform_include", idf_path))
        .include(format!(
            "{}/components/freertos/FreeRTOS-Kernel/include",
            idf_path
        ))
        .include(format!("{}/components/freertos/config/include", idf_path))
        .include(format!("{}/components/esp_common/include", idf_path))
        .include(format!("{}/components/log/include", idf_path))
        // lwip includes for socket types
        .include(format!("{}/components/lwip/lwip/src/include", idf_path))
        .include(format!("{}/components/lwip/port/include", idf_path))
        // Compiler
        .compiler(cxx_compiler)
        // Flags - C++20 required for std::span, operator<=> used by libquicr
        .std("c++20")
        .define("ESP_PLATFORM", "1")
        .define("USE_MBEDTLS", "1")
        .define(
            format!("CONFIG_IDF_TARGET_{}", esp_chip.to_uppercase()).as_str(),
            "1",
        )
        // POSIX defines for newlib (enables fileno, etc.)
        .define("_POSIX_SOURCE", "1")
        .define("_DEFAULT_SOURCE", "1")
        // spdlog configuration - note: we enable exceptions for libquicr headers
        // but use SPDLOG_NO_EXCEPTIONS to avoid spdlog's exception dependencies
        .define("SPDLOG_NO_EXCEPTIONS", "1")
        .define("SPDLOG_NO_THREAD_ID", "1")
        .define("SPDLOG_NO_TLS", "1")
        .define("SPDLOG_DISABLE_DEFAULT_LOGGER", "1")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        // Note: libquicr headers use exceptions (throw), so we must keep exceptions enabled
        // for the FFI wrapper even though this increases binary size
        .flag("-fno-rtti");

    // Add xtensa-specific flags
    if esp_chip.starts_with("esp32s") || esp_chip == "esp32" {
        cc_build.flag("-mlongcalls");
    }

    // Set archiver for cross-compilation
    cc_build.archiver(&ar_path);

    cc_build.compile("quicr_ffi");
}

/// Generate Rust bindings for ESP-IDF
fn generate_espidf_bindings(out_dir: &PathBuf, _target_arch: &str) {
    // For ESP-IDF builds targeting ESP32 (32-bit), we must use a 32-bit target
    // that clang understands. Using the host target (64-bit) causes struct size
    // mismatches because pointers are 8 bytes on 64-bit but 4 bytes on ESP32.
    //
    // We use arm-unknown-linux-gnueabi as a 32-bit target that clang knows.
    // The actual struct layouts match ESP32 since both are 32-bit with similar ABI.
    let bindings = bindgen::Builder::default()
        .header("ffi/include/quicr_ffi.h")
        .allowlist_function("quicr_.*")
        .allowlist_type("Quicr.*")
        .allowlist_var("QUICR_.*")
        .derive_debug(true)
        .derive_default(true)
        .derive_eq(true)
        .derive_hash(true)
        // Use 32-bit ARM target - clang understands this and sizes match ESP32
        .clang_arg("--target=arm-unknown-linux-gnueabi")
        .clang_arg("-DESP_PLATFORM=1")
        .clang_arg("-DUSE_MBEDTLS=1")
        .generate()
        .expect("Unable to generate bindings");

    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

/// Generate Rust bindings for ESP-IDF component build
/// The C++ libraries are built by esp-idf-sys's CMake via extra_components,
/// so we only need to generate bindings here (no build, no link directives).
fn generate_espidf_component_bindings(out_dir: &PathBuf) {
    println!("cargo:warning=quicr: Generating bindings for ESP-IDF component build");

    // Generate bindings - same as generate_espidf_bindings but no link directives
    // The linking is handled by esp-idf-sys which builds the component
    //
    // IMPORTANT: We must use a 32-bit target that clang understands. The ESP32 is
    // 32-bit, so using a 64-bit host target causes struct size mismatches (pointers
    // are 8 bytes on 64-bit but 4 bytes on ESP32).
    // We use arm-unknown-linux-gnueabi as it's 32-bit and well-supported by clang.
    let bindings = bindgen::Builder::default()
        .header("ffi/include/quicr_ffi.h")
        .allowlist_function("quicr_.*")
        .allowlist_type("Quicr.*")
        .allowlist_var("QUICR_.*")
        .derive_debug(true)
        .derive_default(true)
        .derive_eq(true)
        .derive_hash(true)
        // Use 32-bit ARM target - clang understands this and sizes match ESP32
        .clang_arg("--target=arm-unknown-linux-gnueabi")
        .clang_arg("-DESP_PLATFORM=1")
        .clang_arg("-DPLATFORM_ESP_IDF=1")
        .clang_arg("-DUSE_MBEDTLS=1")
        .generate()
        .expect("Unable to generate bindings");

    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    // Note: We do NOT emit cargo:rustc-link-lib directives here because
    // the ESP-IDF component will be linked automatically by esp-idf-sys
}

/// Get number of CPUs for parallel build
fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
}

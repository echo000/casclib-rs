extern crate cmake;

use std::{env, fs};
use std::{process::Command};

// Return (major, minor)
fn get_cmake_version() -> Option<(u32, u32, u32)> {
    let output = Command::new("cmake").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = core::str::from_utf8(&output.stdout).ok()?;

    // Parse
    let version = stdout.lines().next()?.strip_prefix("cmake version ")?;
    let mut digits = version.splitn(3, '.'); // split version string to major minor patch
    let major = digits.next()?.parse::<u32>().ok()?;
    let minor = digits.next()?.parse::<u32>().ok()?;
    let patch = digits.next()?.parse::<u32>().ok()?;
    Some((major, minor, patch))
}

fn main() {
    // Gets CacsLib source path from env CASCLIB_DIR
    let casclib_path = env::var("CASCLIB_DIR").unwrap_or("../deps/CascLib".to_string());

    println!("cargo:rerun-if-changed={}", casclib_path);

    let mut cfg = cmake::Config::new(&casclib_path);

    // TODO: CMake requires higher minimum version in new release, and CascLib has not been updated for it yet
    let cmake_version = get_cmake_version().unwrap_or_default();
    if cmake_version >= (4, 0, 0) {
        println!("CMake version: {:?} is >= 4.0, option \"-CMAKE_POLICY_VERSION_MINIMUM=3.5\" is required", cmake_version);
        cfg.define("CMAKE_POLICY_VERSION_MINIMUM", "3.5");
    }

    #[cfg(target_os = "windows")]
    {
        cfg.cxxflag("-D UNICODE")
            .cxxflag("-D _UNICODE")
            .define("CASC_UNICODE", "ON");
    }

    // Builds CascLib using cmake
    let dst = cfg
        .define("CASC_BUILD_SHARED_LIB", "OFF")
        .define("CASC_BUILD_STATIC_LIB", "ON")
        .build();

    let mut lib = dst.join("lib");
    // on some distributions on 64 bit lib dir is called lib64
    if fs::metadata(&lib).is_err() {
        lib = dst.join("lib64");
        if fs::metadata(&lib).is_err() {
            println!("libcasc is missing");
        }
    }

    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static=casc");

    let target = env::var("TARGET").unwrap();
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=z");
    } else if target.contains("linux") {
        println!("cargo:rustc-link-lib=stdc++");
        println!("cargo:rustc-link-lib=z");
    }
}

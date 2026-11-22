//! On macOS, link the clang runtime archive that provides
//! `__isPlatformVersionAtLeast`.
//!
//! ort's prebuilt onnxruntime (downloaded by ort-sys) ships CoreML objects
//! (`host_utils.mm.o`, `model.mm.o`) that reference `__isPlatformVersionAtLeast`
//! as a strong symbol. That symbol is vended by compiler-rt
//! (`libclang_rt.osx.a`), which the clang driver links automatically — but
//! rustc links with `-nodefaultlibs`, so without this the final link fails on
//! GitHub's macOS runners with:
//!   Undefined symbols for architecture arm64: "___isPlatformVersionAtLeast"
//!   ld: symbol(s) not found for architecture arm64

use std::env;
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    // rustc drives the system `cc`; ask that same compiler for its resource
    // dir so the `-L` path matches the toolchain actually linking (CLT or the
    // Xcode selected via `xcode-select`).
    let cc = env::var("CC").unwrap_or_else(|_| "cc".into());
    let Ok(out) = Command::new(&cc).arg("-print-resource-dir").output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if dir.is_empty() {
        return;
    }
    println!("cargo:rustc-link-search=native={dir}/lib/darwin");
    println!("cargo:rustc-link-lib=clang_rt.osx");
}

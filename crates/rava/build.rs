//! Link settings per system. The VM needs the low 4 GiB of its address space (classes and
//! methods live below 2 GiB, `src/low.rs`) and the range up to 32 GiB for the heap of compressed
//! references: on macOS a default `__PAGEZERO` takes the low 4 GiB, and a Windows executable's
//! default base (5 GiB) sits in the heap's range.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-pagezero_size,0x1000");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let base = "0x7ff600000000";
        match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
            Ok("msvc") => println!("cargo:rustc-link-arg-bins=/BASE:{base}"),
            _ => println!("cargo:rustc-link-arg-bins=-Wl,--image-base,{base}"),
        }
    }
}

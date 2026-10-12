//! Link settings per system. On macOS the VM needs the low 4 GiB of its address space (classes
//! and methods live below 2 GiB, `src/low.rs`), which a default `__PAGEZERO` takes whole.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-pagezero_size,0x1000");
    }
}

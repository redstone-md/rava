//! `java.lang.Math`'s transcendental functions from the C library, as HotSpot supplies its own
//! stubs for them instead of the bytecode (which delegates to `StrictMath`'s fdlibm port).
//! `Math` allows any result within 1 ulp of the exact one, semi-monotonic, and the C library's
//! are; `StrictMath` keeps fdlibm's bit-exact results. Every tier computes these with the same
//! functions (the interpreter and the baseline compiler call the bodies below, the optimizing
//! tier calls `FNS` directly), so a result never depends on which tier ran it.

use super::NativeRegistry;
use crate::thread::{JResult, Thread};

/// The functions on raw double bits (the intrinsic passes and returns them in integer
/// registers), indexed as `NAMES`.
pub(crate) const FNS: [extern "C" fn(u64) -> u64; 5] = [sin, cos, tan, exp, log];
pub(crate) const NAMES: [&str; 5] = ["sin", "cos", "tan", "exp", "log"];

macro_rules! libm {
    ($($name:ident),*) => {$(
        extern "C" fn $name(x: u64) -> u64 {
            f64::from_bits(x).$name().to_bits()
        }
    )*};
}
libm!(sin, cos, tan, exp);

extern "C" fn log(x: u64) -> u64 {
    f64::from_bits(x).ln().to_bits()
}

/// The `Math` method `name(D)D` the C library computes: its index in `FNS`.
pub(crate) fn index(name: &str) -> Option<u8> {
    NAMES.iter().position(|&n| n == name).map(|i| i as u8)
}

fn body<const F: usize>(_: &mut Thread, a: &[u64]) -> JResult<u64> {
    Ok(FNS[F](a[0]))
}

pub(super) fn register(r: &mut NativeRegistry) {
    type Stub = fn(&mut Thread, &[u64]) -> JResult<u64>;
    let bodies: [Stub; 5] = [body::<0>, body::<1>, body::<2>, body::<3>, body::<4>];
    for (name, f) in NAMES.iter().zip(bodies) {
        r.add_stub("java/lang/Math", name, "(D)D", f);
    }
}

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
pub(crate) const FNS: [extern "sysv64" fn(u64) -> u64; 5] = [sin, cos, tan, exp, log];
pub(crate) const NAMES: [&str; 5] = ["sin", "cos", "tan", "exp", "log"];

macro_rules! libm {
    ($($name:ident),*) => {$(
        extern "sysv64" fn $name(x: u64) -> u64 {
            f64::from_bits(x).$name().to_bits()
        }
    )*};
}
libm!(sin, cos, tan, exp);

extern "sysv64" fn log(x: u64) -> u64 {
    f64::from_bits(x).ln().to_bits()
}

/// The `Math` method `name(D)D` the C library computes: its index in `FNS`. macOS's `tan` can
/// land on the other side of the exact result from fdlibm's, two ulps from it, and the Windows C
/// runtime's are not within an ulp at all, so there these keep their bytecode (fdlibm, as
/// HotSpot's own stubs compute it).
pub(crate) fn index(name: &str) -> Option<u8> {
    if cfg!(windows) || cfg!(target_os = "macos") && name == "tan" {
        return None;
    }
    NAMES.iter().position(|&n| n == name).map(|i| i as u8)
}

fn body<const F: usize>(_: &mut Thread, a: &[u64]) -> JResult<u64> {
    Ok(FNS[F](a[0]))
}

/// `Math.fma(double, double, double)`: the exact `a * b + c` rounded once, as the processor's
/// fused multiply-add computes it. The bytecode computes it with `BigDecimal` (HotSpot never runs
/// it: the method is an intrinsic), microseconds a call: JOML multiplies matrices with it, so a
/// modded client's render thread spent a tenth of its time there.
fn fma_d(_: &mut Thread, a: &[u64]) -> JResult<u64> {
    let (x, y, z) = (f64::from_bits(a[0]), f64::from_bits(a[2]), f64::from_bits(a[4]));
    Ok(x.mul_add(y, z).to_bits())
}

/// `Math.fma(float, float, float)`, the same in single precision.
fn fma_f(_: &mut Thread, a: &[u64]) -> JResult<u64> {
    let f = |v: u64| f32::from_bits(v as u32);
    Ok(f(a[0]).mul_add(f(a[1]), f(a[2])).to_bits() as u64)
}

pub(super) fn register(r: &mut NativeRegistry) {
    r.add_stub("java/lang/Math", "fma", "(DDD)D", fma_d);
    r.add_stub("java/lang/Math", "fma", "(FFF)F", fma_f);
    type Stub = fn(&mut Thread, &[u64]) -> JResult<u64>;
    let bodies: [Stub; 5] = [body::<0>, body::<1>, body::<2>, body::<3>, body::<4>];
    for (name, f) in NAMES.iter().zip(bodies).filter(|(n, _)| index(n).is_some()) {
        r.add_stub("java/lang/Math", name, "(D)D", f);
    }
}

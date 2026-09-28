//! Production counterpart to `rava_sampler`: preserves the diagnostic API while
//! compiling every inactive wall-profile hook out of the execution path.

pub const OTHER: u8 = 0;
pub const JIT_EXEC: u8 = 1;
pub const INTERP_EXEC: u8 = 2;
pub const VM_DISPATCH: u8 = 3;
pub const CLASS_LOADING: u8 = 4;
pub const NATIVE_BRIDGES: u8 = 5;
pub const JIT_CALLBACK: u8 = 6;

/// API-compatible no-op for the few JIT entry/exit marker stores. Keeping this
/// type local lets those stores inline away in production without spreading
/// `wall-profile` conditionals through JIT ownership code.
pub struct NoopAtomicU32;
impl NoopAtomicU32 {
    #[inline]
    pub fn store(&self, _value: u32, _ordering: std::sync::atomic::Ordering) {}

    #[inline]
    pub fn swap(&self, _value: u32, _ordering: std::sync::atomic::Ordering) -> u32 {
        0
    }
}

pub static CURRENT_METHOD: NoopAtomicU32 = NoopAtomicU32;

/// A zero-sized guard makes inactive category scopes disappear after inlining.
pub struct Guard;

#[inline]
pub fn enabled() -> bool {
    false
}

#[inline]
pub fn enter(_category: u8) -> Guard {
    Guard
}

#[inline]
pub fn enter_site(_category: u8, _class: usize, _cp: u32) -> Guard {
    Guard
}

#[inline]
pub fn register_bridge(_owner: &str, _name: &str) -> u32 {
    0
}

pub struct BridgeScope;

#[inline]
pub fn bridge_scope(_bridge: u32) -> BridgeScope {
    BridgeScope
}

#[inline]
pub fn reset() {}

#[inline]
pub fn dump_tagged(_tag: &str) {}

#[inline]
pub fn dump() {}

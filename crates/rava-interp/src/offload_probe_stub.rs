//! Production counterpart to `offload_probe`: all instrumentation inlines away.

#[inline]
pub(crate) fn epoch_clear(_evicted: usize) {}

#[inline]
pub(crate) fn hit() {}

#[inline]
pub(crate) fn analyzed() {}

#[inline]
pub(crate) fn stored() {}

#[inline]
pub(crate) fn unstable() {}

#[inline]
pub(crate) fn dump() {}

// Readers for the diag gauges — always zero in the production build, where the counters do not
// exist at all.
pub(crate) fn hits_n() -> u64 {
    0
}
pub(crate) fn analyzed_n() -> u64 {
    0
}
pub(crate) fn stored_n() -> u64 {
    0
}
pub(crate) fn unstable_n() -> u64 {
    0
}

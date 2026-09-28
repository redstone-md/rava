//! Stage stable native exit addresses, then commit their machine-code lifetime.

use super::OwnedExceptionSite;

/// Unpublished sites are freed on a compile refusal, including their class pins.
/// Box addresses remain stable when this batch moves or its vector grows.
#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct PendingExceptionSites {
    sites: Vec<Box<OwnedExceptionSite>>,
}

#[allow(dead_code)]
impl PendingExceptionSites {
    /// The returned address must not escape through callable machine code until
    /// publish completes. Dropping an unpublished batch invalidates its addresses.
    pub(crate) fn insert(&mut self, site: OwnedExceptionSite) -> *const OwnedExceptionSite {
        let site = Box::new(site);
        let address = std::ptr::from_ref(&*site);
        self.sites.push(site);
        address
    }

    /// Commit before exposing any entry pointer containing these addresses.
    ///
    /// Cranelift 0.132.1 keeps finalized code alive after JITModule drop; only
    /// explicit unsafe free_memory invalidates it. RAVA never calls free_memory.
    /// Published metadata therefore has the same process lifetime as native code,
    /// including code retained through old IC pointers or compiler-thread exit.
    /// A future executable-memory reclamation design must reclaim both together.
    pub(crate) fn publish(self) -> usize {
        let count = self.sites.len();
        for site in self.sites { let _ = Box::leak(site); }
        count
    }
}

#[cfg(test)]
mod tests;

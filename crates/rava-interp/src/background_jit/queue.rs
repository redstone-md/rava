use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub(crate) struct PermitPool {
    available: AtomicUsize,
    capacity: usize,
}

impl PermitPool {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            available: AtomicUsize::new(capacity),
            capacity,
        })
    }

    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<Permit> {
        self.available
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |available| {
                available.checked_sub(1)
            })
            .ok()
            .map(|_| Permit {
                pool: Arc::clone(self),
            })
    }
}

/// A non-cloneable queue slot. Dropping it is the only refund operation.
pub(crate) struct Permit {
    pool: Arc<PermitPool>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let previous = self.pool.available.fetch_add(1, Ordering::Release);
        debug_assert!(previous < self.pool.capacity, "queue permit refunded twice");
    }
}

pub(crate) struct Envelope<T> {
    item: T,
    _permit: Option<Permit>,
}

impl<T> Envelope<T> {
    pub(crate) fn new(item: T, permit: Option<Permit>) -> Self {
        Self {
            item,
            _permit: permit,
        }
    }

    /// Workers call this immediately after `recv`, refunding any permit before codegen.
    pub(crate) fn into_inner(self) -> T {
        self.item
    }
}

#[cfg(test)]
mod tests {
    use super::{Envelope, PermitPool};
    use std::sync::mpsc::{self, TrySendError};

    #[test]
    fn capacity_is_bounded_and_drop_refunds() {
        let pool = PermitPool::new(2);
        let first = pool.try_acquire().expect("first slot");
        let _second = pool.try_acquire().expect("second slot");
        assert!(pool.try_acquire().is_none());

        drop(first);

        assert!(pool.try_acquire().is_some());
    }

    #[test]
    fn recv_unwrap_refunds_accepted_envelope() {
        let pool = PermitPool::new(1);
        let permit = pool.try_acquire().expect("queue slot");
        let (tx, rx) = mpsc::sync_channel(1);
        tx.try_send(Envelope::new(7_u8, Some(permit)))
            .expect("reserved send");
        assert!(pool.try_acquire().is_none());

        assert_eq!(rx.recv().expect("reserved receive").into_inner(), 7);
        assert!(pool.try_acquire().is_some());
    }

    #[test]
    fn disconnected_send_refunds_envelope() {
        let pool = PermitPool::new(1);
        let permit = pool.try_acquire().expect("queue slot");
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);

        match tx.try_send(Envelope::new(7_u8, Some(permit))) {
            Err(TrySendError::Disconnected(envelope)) => drop(envelope.into_inner()),
            _ => panic!("expected disconnected queue"),
        }

        assert!(pool.try_acquire().is_some());
    }

    #[test]
    fn full_send_refunds_envelope() {
        let pool = PermitPool::new(1);
        let permit = pool.try_acquire().expect("queue slot");
        let (tx, _rx) = mpsc::sync_channel(1);
        tx.try_send(Envelope::new(1_u8, None))
            .expect("fill channel");

        match tx.try_send(Envelope::new(2_u8, Some(permit))) {
            Err(TrySendError::Full(envelope)) => drop(envelope.into_inner()),
            _ => panic!("expected full queue"),
        }

        assert!(pool.try_acquire().is_some());
    }

    #[test]
    fn concurrent_acquire_never_oversubscribes() {
        let pool = PermitPool::new(4);
        let start = std::sync::Arc::new(std::sync::Barrier::new(9));
        let joins: Vec<_> = (0..8)
            .map(|_| {
                let pool = std::sync::Arc::clone(&pool);
                let start = std::sync::Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    pool.try_acquire()
                })
            })
            .collect();
        start.wait();

        let held: Vec<_> = joins
            .into_iter()
            .filter_map(|join| join.join().expect("acquirer thread"))
            .collect();

        assert_eq!(held.len(), 4);
        assert!(pool.try_acquire().is_none());
    }
}

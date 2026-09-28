//! Bounded reuse of inactive prepared-frame buffers. Stored cells are not roots.
#[derive(Default)]
pub(crate) struct Pool {
    idle: Vec<Vec<i64>>,
}

impl Pool {
    const MAX_BUFFERS: usize = 64;
    const MAX_CELLS: usize = 1024;

    pub(super) fn take(&mut self) -> Vec<i64> {
        self.idle.pop().unwrap_or_default()
    }

    pub(super) fn give(&mut self, mut cells: Vec<i64>) {
        if cells.capacity() > Self::MAX_CELLS || self.idle.len() >= Self::MAX_BUFFERS {
            return;
        }
        cells.clear();
        self.idle.push(cells);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aot_cell_pool_never_reissues_an_active_buffer_and_bounds_retention() {
        let mut pool = Pool::default();
        let original = vec![41; 8];
        let address = original.as_ptr();
        pool.give(original);
        let mut outer = pool.take();
        assert_eq!(outer.as_ptr(), address);
        assert!(outer.is_empty());
        outer.resize(8, 7);
        let mut inner = pool.take();
        inner.resize(8, 9);
        assert_ne!(outer.as_ptr(), inner.as_ptr());
        assert_eq!(outer, vec![7; 8]);
        pool.give(inner);
        pool.give(outer);
        for _ in 0..100 { pool.give(Vec::with_capacity(8)); }
        assert_eq!(pool.idle.len(), Pool::MAX_BUFFERS);
        pool.idle.clear();
        pool.give(Vec::with_capacity(Pool::MAX_CELLS + 1));
        assert!(pool.idle.is_empty());
    }
}

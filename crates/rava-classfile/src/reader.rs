//! Big-endian byte cursor over a class file. One authoritative reader, reused
//! everywhere we walk class bytes (DRY).

use crate::ClassError;

pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    #[inline]
    fn take(&mut self, n: usize) -> Result<&'a [u8], ClassError> {
        let end = self.pos.checked_add(n).ok_or(ClassError::UnexpectedEof)?;
        let slice = self.bytes.get(self.pos..end).ok_or(ClassError::UnexpectedEof)?;
        self.pos = end;
        Ok(slice)
    }

    /// Take exactly `N` bytes as an ARRAY, `N` a compile-time constant.
    ///
    /// Why this exists rather than `take(2)?` + `b[0]`, `b[1]`: `take` hands back a slice
    /// whose length is a runtime value, so each subsequent index carries its own bounds
    /// check and `from_be_bytes` is fed through a `[b[0], b[1], ..]` re-materialisation.
    /// LLVM can often fold that once everything inlines, but "often" is an inlining
    /// decision, and this reader is the workhorse of a phase that is 53% of the boot wall:
    /// every `u16` of every constant-pool entry, every attribute header, every branch
    /// offset in every `Code` body, across ~15,000 classes. Handing the type system a
    /// `&[u8; N]` makes the absence of the checks a property of the signature instead.
    ///
    /// BYTE-IDENTICAL to `take(N)`: the same `checked_add`, the same `end > len` rejection
    /// with the same `UnexpectedEof`, the same cursor advance, the same bytes.
    ///
    /// SAFETY: the guard establishes `pos <= end <= bytes.len()` (`end = pos + N` without
    /// wrap), so `[pos, pos + N)` lies wholly inside the borrowed slice. `[u8; N]` has size
    /// `N` and alignment 1, identical to `N` contiguous bytes, so the reference is valid
    /// and inherits `'a` from `self.bytes`.
    #[inline]
    fn take_n<const N: usize>(&mut self) -> Result<&'a [u8; N], ClassError> {
        let end = self.pos.checked_add(N).ok_or(ClassError::UnexpectedEof)?;
        if end > self.bytes.len() {
            return Err(ClassError::UnexpectedEof);
        }
        let at = unsafe { self.bytes.as_ptr().add(self.pos) };
        // The annotation is load-bearing: without it inference would try to borrow through
        // `&mut self` and the cursor advance below would conflict.
        let head: &'a [u8; N] = unsafe { &*(at as *const [u8; N]) };
        self.pos = end;
        Ok(head)
    }

    #[inline]
    pub fn u8(&mut self) -> Result<u8, ClassError> {
        Ok(self.take_n::<1>()?[0])
    }

    #[inline]
    pub fn u16(&mut self) -> Result<u16, ClassError> {
        Ok(u16::from_be_bytes(*self.take_n::<2>()?))
    }

    #[inline]
    pub fn u32(&mut self) -> Result<u32, ClassError> {
        Ok(u32::from_be_bytes(*self.take_n::<4>()?))
    }

    #[inline]
    pub fn i64(&mut self) -> Result<i64, ClassError> {
        Ok(i64::from_be_bytes(*self.take_n::<8>()?))
    }

    #[inline]
    pub fn f64_bits(&mut self) -> Result<u64, ClassError> {
        Ok(u64::from_be_bytes(*self.take_n::<8>()?))
    }

    /// Read `n` raw bytes (e.g. a UTF-8 constant or an attribute body).
    #[inline]
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], ClassError> {
        self.take(n)
    }

    /// Skip `n` bytes (attributes we do not yet interpret).
    #[inline]
    pub fn skip(&mut self, n: usize) -> Result<(), ClassError> {
        self.take(n)?;
        Ok(())
    }
}

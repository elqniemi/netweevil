//! Reusable search scratch shared by every thread.

use std::sync::{Mutex, PoisonError};

/// A stack of reusable search scratch values (dense per-edge label arrays
/// and queues). A search borrows the most recently returned value, which is
/// also the most likely to be cache-warm, and hands it back afterwards.
///
/// Unlike a thread-local, the number of values is the peak number of
/// concurrent searches rather than the number of threads that ever ran
/// one. The API runs single routes inline on Tokio workers through
/// `block_in_place`, so successive requests land on many different OS
/// threads; thread-local scratch then kept a dense label array per thread.
pub(crate) struct ScratchPool<T> {
    free: Mutex<Vec<Box<T>>>,
}

impl<T: Default> ScratchPool<T> {
    pub(crate) const fn new() -> Self {
        Self {
            free: Mutex::new(Vec::new()),
        }
    }

    /// Runs `search` with exclusive use of one scratch value. Nested calls
    /// get distinct values. A value is dropped if `search` panics.
    pub(crate) fn with<R>(&self, search: impl FnOnce(&mut T) -> R) -> R {
        let mut scratch = self
            .free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
            .unwrap_or_default();
        let result = search(&mut scratch);
        self.free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(scratch);
        result
    }
}

#[cfg(test)]
mod tests;

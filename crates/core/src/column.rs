//! Read-only arrays that either own their values or borrow them from a
//! shared buffer, typically a memory-mapped bundle file.

use std::any::Any;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use bytemuck::Pod;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A fixed-width array that dereferences to `&[T]`.
///
/// It holds either a `Vec<T>` or a typed view into a buffer kept alive by a
/// shared owner (a memory map of a bundle file), so large arrays load
/// without copying. Mapped pages are file-backed: the kernel shares them
/// between processes and can drop them under memory pressure. Clones of a
/// mapped column share the buffer; clones of an owned column copy it.
pub struct Column<T: Pod> {
    /// Start of the values: the vector's buffer or a pointer into the
    /// owner's bytes, valid for `len` values while `storage` lives.
    ptr: *const T,
    len: usize,
    storage: Storage<T>,
}

enum Storage<T> {
    Owned(Vec<T>),
    Shared(Arc<dyn Any + Send + Sync>),
}

// SAFETY: the column only hands out shared references to `T` values that
// it owns or that its `Send + Sync` owner keeps alive and never mutates.
unsafe impl<T: Pod + Send + Sync> Send for Column<T> {}
// SAFETY: as above; `&Column<T>` only yields `&[T]`.
unsafe impl<T: Pod + Send + Sync> Sync for Column<T> {}

impl<T: Pod> Column<T> {
    /// An empty owned column.
    pub fn new() -> Self {
        Self::from(Vec::new())
    }

    /// A view of `bytes` as `T` values, kept alive by `owner`.
    ///
    /// Returns `None` when `bytes` is not aligned for `T` or its length is
    /// not a multiple of `T`'s size.
    ///
    /// # Safety
    ///
    /// `bytes` must stay valid and unmodified for as long as `owner` lives:
    /// it must lie in memory that `owner` owns, such as a memory map that
    /// no process truncates or writes in place.
    pub unsafe fn from_shared(owner: Arc<dyn Any + Send + Sync>, bytes: &[u8]) -> Option<Self> {
        let values: &[T] = bytemuck::try_cast_slice(bytes).ok()?;
        Some(Self {
            ptr: values.as_ptr(),
            len: values.len(),
            storage: Storage::Shared(owner),
        })
    }

    /// Whether the values live in a shared buffer rather than in a vector.
    pub fn is_shared(&self) -> bool {
        matches!(self.storage, Storage::Shared(_))
    }

    pub fn as_slice(&self) -> &[T] {
        self
    }

    /// The values as a vector; copies them when the column is shared.
    pub fn into_vec(self) -> Vec<T> {
        match self.storage {
            Storage::Owned(values) => values,
            Storage::Shared(_) => self.as_slice().to_vec(),
        }
    }

    /// The values as an owned, mutable vector, copying a shared column
    /// first.
    pub fn make_mut(&mut self) -> &mut Vec<T> {
        if let Storage::Shared(_) = self.storage {
            *self = Self::from(self.as_slice().to_vec());
        }
        match &mut self.storage {
            Storage::Owned(values) => values,
            Storage::Shared(_) => unreachable!("shared column was just copied"),
        }
    }
}

impl<T: Pod> Deref for Column<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        // SAFETY: `ptr` and `len` describe the vector's initialized buffer
        // or the owner's bytes, both alive as long as `self`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl<T: Pod> AsRef<[T]> for Column<T> {
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T: Pod> From<Vec<T>> for Column<T> {
    fn from(values: Vec<T>) -> Self {
        Self {
            ptr: values.as_ptr(),
            len: values.len(),
            storage: Storage::Owned(values),
        }
    }
}

impl<'a, T: Pod> IntoIterator for &'a Column<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T: Pod> FromIterator<T> for Column<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from(iter.into_iter().collect::<Vec<_>>())
    }
}

impl<T: Pod> Default for Column<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Pod> Clone for Column<T> {
    fn clone(&self) -> Self {
        match &self.storage {
            Storage::Owned(values) => Self::from(values.clone()),
            Storage::Shared(owner) => Self {
                ptr: self.ptr,
                len: self.len,
                storage: Storage::Shared(owner.clone()),
            },
        }
    }
}

impl<T: Pod + fmt::Debug> fmt::Debug for Column<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Pod + PartialEq> PartialEq for Column<T> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Pod + Eq> Eq for Column<T> {}

impl<T: Pod + PartialEq> PartialEq<Vec<T>> for Column<T> {
    fn eq(&self, other: &Vec<T>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Pod + PartialEq> PartialEq<Column<T>> for Vec<T> {
    fn eq(&self, other: &Column<T>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Pod + PartialEq> PartialEq<[T]> for Column<T> {
    fn eq(&self, other: &[T]) -> bool {
        self.as_slice() == other
    }
}

impl<T: Pod + Serialize> Serialize for Column<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_slice().serialize(serializer)
    }
}

impl<'de, T: Pod + Deserialize<'de>> Deserialize<'de> for Column<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<T>::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_and_shared_columns_read_the_same_values() {
        let owned = Column::from(vec![1_u32, 2, 3]);
        assert_eq!(&*owned, &[1, 2, 3]);
        assert!(!owned.is_shared());

        let bytes: Arc<Vec<u64>> = Arc::new(vec![0, u64::from_le_bytes([4, 0, 0, 0, 5, 0, 0, 0])]);
        let view = bytemuck::cast_slice::<u64, u8>(&bytes[1..]);
        // SAFETY: `view` points into the vector the owner keeps alive.
        let shared =
            unsafe { Column::<u32>::from_shared(bytes.clone(), view) }.expect("aligned view");
        assert!(shared.is_shared());
        assert_eq!(&*shared, &[4, 5]);
        let mut copy = shared.clone();
        drop(shared);
        assert_eq!(&*copy, &[4, 5]);
        copy.make_mut().push(6);
        assert!(!copy.is_shared());
        assert_eq!(copy.into_vec(), vec![4, 5, 6]);

        let misaligned = &bytemuck::cast_slice::<u64, u8>(&bytes)[1..5];
        // SAFETY: as above.
        assert!(unsafe { Column::<u32>::from_shared(bytes.clone(), misaligned) }.is_none());
    }
}

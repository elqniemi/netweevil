//! Per-state search arrays whose empty value is all zero bytes.
//!
//! The arrays come from zeroed allocations, which the OS backs with shared
//! zero pages until they are written, so a search that labels `k` states
//! commits memory for about `k` entries however many states the graph has.
//! Searches record the entries they write in a touched list and clear
//! exactly those afterwards; a search that touched a large share of the
//! array releases it instead, so a pooled scratch keeps no high-water pages.

use bytemuck::Zeroable;

/// A scratch array is released rather than cleared entry by entry once a
/// search touched more than `1 / RELEASE_DIVISOR` of its entries.
const RELEASE_DIVISOR: usize = 8;

/// Whether a scratch array of `len` entries with `touched` written entries
/// is released instead of cleared through its touched list.
pub(crate) fn release_after(len: usize, touched: usize) -> bool {
    touched > len / RELEASE_DIVISOR
}

/// Bit pattern of `+inf`; costs are stored XORed with it, so zero bits
/// decode to `+inf` and every other value round-trips exactly.
const INFINITY_BITS: u64 = f64::INFINITY.to_bits();

/// Encodes a cost so that `+inf` is all zero bits.
#[inline]
pub(crate) fn encode_cost(cost: f64) -> u64 {
    cost.to_bits() ^ INFINITY_BITS
}

/// Inverse of [`encode_cost`].
#[inline]
pub(crate) fn decode_cost(raw: u64) -> f64 {
    f64::from_bits(raw ^ INFINITY_BITS)
}

/// Encodes a state or arc link so that `u32::MAX` (no link) is zero.
#[inline]
pub(crate) fn encode_link(link: u32) -> u32 {
    link.wrapping_add(1)
}

/// Inverse of [`encode_link`].
#[inline]
pub(crate) fn decode_link(raw: u32) -> u32 {
    raw.wrapping_sub(1)
}

/// A zero-allocated array of `T` whose all-zero value means "empty".
#[derive(Clone)]
pub(crate) struct ZeroedArray<T> {
    values: Box<[T]>,
}

impl<T: Zeroable + Copy> Default for ZeroedArray<T> {
    fn default() -> Self {
        Self::zeroed(0)
    }
}

impl<T: Zeroable + Copy> ZeroedArray<T> {
    pub(crate) fn zeroed(len: usize) -> Self {
        Self {
            values: bytemuck::zeroed_slice_box(len),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    #[inline]
    pub(crate) fn get(&self, index: usize) -> T {
        self.values[index]
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, index: usize) -> &mut T {
        &mut self.values[index]
    }

    /// Leaves at least `len` entries, all zero. Entries outside `touched`
    /// must already be zero. With `release` (or when the array is too short)
    /// the buffer is dropped and a fresh zeroed one allocated; otherwise the
    /// `touched` entries are cleared in place.
    pub(crate) fn reset(&mut self, len: usize, touched: &[u32], release: bool) {
        if release || self.values.len() < len {
            self.values = Box::default();
            self.values = bytemuck::zeroed_slice_box(len);
            return;
        }
        for &index in touched {
            self.values[index as usize] = T::zeroed();
        }
    }

    /// A copy holding the `touched` entries (all others are zero). A
    /// densely touched array is moved out instead and this one left empty,
    /// so the next [`Self::reset`] allocates fresh pages.
    pub(crate) fn take_snapshot(&mut self, touched: &[u32]) -> Self {
        if release_after(self.values.len(), touched.len()) {
            return std::mem::take(self);
        }
        let mut copy = Self::zeroed(self.values.len());
        for &index in touched {
            copy.values[index as usize] = self.values[index as usize];
        }
        copy
    }

    /// Number of non-zero entries; for tests.
    #[cfg(test)]
    pub(crate) fn count_nonzero(&self) -> usize
    where
        T: bytemuck::Pod,
    {
        self.values
            .iter()
            .filter(|value| bytemuck::bytes_of(*value).iter().any(|&byte| byte != 0))
            .count()
    }
}

/// Per-state costs with `+inf` (unreached) stored as zero.
#[derive(Clone, Default)]
pub(crate) struct CostArray(ZeroedArray<u64>);

impl CostArray {
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub(crate) fn get(&self, index: usize) -> f64 {
        decode_cost(self.0.get(index))
    }

    /// Whether `index` holds `+inf`.
    #[inline]
    pub(crate) fn is_unreached(&self, index: usize) -> bool {
        self.0.get(index) == 0
    }

    #[inline]
    pub(crate) fn set(&mut self, index: usize, cost: f64) {
        *self.0.get_mut(index) = encode_cost(cost);
    }

    pub(crate) fn reset(&mut self, len: usize, touched: &[u32], release: bool) {
        self.0.reset(len, touched, release);
    }

    pub(crate) fn take_snapshot(&mut self, touched: &[u32]) -> Self {
        Self(self.0.take_snapshot(touched))
    }

    #[cfg(test)]
    pub(crate) fn count_reached(&self) -> usize {
        self.0.count_nonzero()
    }
}

/// Per-state links with `u32::MAX` (no link) stored as zero.
#[derive(Clone, Default)]
pub(crate) struct LinkArray(ZeroedArray<u32>);

impl LinkArray {
    #[inline]
    pub(crate) fn get(&self, index: usize) -> u32 {
        decode_link(self.0.get(index))
    }

    #[inline]
    pub(crate) fn set(&mut self, index: usize, link: u32) {
        *self.0.get_mut(index) = encode_link(link);
    }

    pub(crate) fn reset(&mut self, len: usize, touched: &[u32], release: bool) {
        self.0.reset(len, touched, release);
    }

    pub(crate) fn take_snapshot(&mut self, touched: &[u32]) -> Self {
        Self(self.0.take_snapshot(touched))
    }

    #[cfg(test)]
    pub(crate) fn count_linked(&self) -> usize {
        self.0.count_nonzero()
    }
}

/// Clears a touched list, dropping its buffer after a large search so the
/// pooled scratch does not keep its high-water capacity.
pub(crate) fn reset_touched(touched: &mut Vec<u32>, release: bool) {
    if release {
        *touched = Vec::new();
    } else {
        touched.clear();
    }
}

/// One search direction's per-state costs and links with the touched list
/// that resets them.
#[derive(Default)]
pub(crate) struct SearchLabels {
    pub(crate) cost: CostArray,
    pub(crate) link: LinkArray,
    pub(crate) touched: Vec<u32>,
}

impl SearchLabels {
    /// Leaves at least `len` unreached states.
    pub(crate) fn prepare(&mut self, len: usize) {
        let release = release_after(self.cost.len(), self.touched.len());
        self.cost.reset(len, &self.touched, release);
        self.link.reset(len, &self.touched, release);
        reset_touched(&mut self.touched, release);
    }

    /// Lowers the cost of `index` to a finite `cost` that beats the known
    /// one by more than `f64::EPSILON`, recording `link`.
    #[inline]
    pub(crate) fn update(&mut self, index: usize, cost: f64, link: u32) -> bool {
        if !cost.is_finite() || cost + f64::EPSILON >= self.cost.get(index) {
            return false;
        }
        if self.cost.is_unreached(index) {
            self.touched.push(index as u32);
        }
        self.cost.set(index, cost);
        self.link.set(index, link);
        true
    }

    /// A copy of the labels without the touched list; see
    /// [`ZeroedArray::take_snapshot`].
    pub(crate) fn take_snapshot(&mut self) -> (CostArray, LinkArray) {
        (
            self.cost.take_snapshot(&self.touched),
            self.link.take_snapshot(&self.touched),
        )
    }
}

#[cfg(test)]
mod tests;

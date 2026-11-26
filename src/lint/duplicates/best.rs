//! A bounded selection: the best few of many keys offered one by one, and
//! how many were offered, in memory that grows with the few alone.

use std::collections::BinaryHeap;

/// The `limit` smallest keys offered, and the count of every key offered.
/// Keys are totally ordered, so which are kept does not depend on the order
/// they come in.
pub(super) struct Best<K> {
    limit: usize,
    /// The kept keys, the largest on top: the first to leave.
    kept: BinaryHeap<K>,
    offered: usize,
}

impl<K: Ord> Best<K> {
    pub(super) const fn new(limit: usize) -> Self {
        Self {
            limit,
            kept: BinaryHeap::new(),
            offered: 0,
        }
    }

    pub(super) fn offer(&mut self, key: K) {
        self.offered = self.offered.saturating_add(1);
        if self.kept.len() < self.limit {
            self.kept.push(key);
        } else if self.kept.peek().is_some_and(|largest| key < *largest) {
            self.kept.pop();
            self.kept.push(key);
        }
    }

    /// The selection of every key either was offered: what one selection
    /// would hold had it been offered them all.
    pub(super) fn merge(mut self, other: Self) -> Self {
        let offered = self.offered.saturating_add(other.offered);
        for key in other.kept {
            self.offer(key);
        }
        self.offered = offered;
        self
    }

    /// The count of keys offered, and the kept ones, smallest first.
    pub(super) fn into_sorted(self) -> (usize, Vec<K>) {
        (self.offered, self.kept.into_sorted_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_keeps_the_smallest_whatever_the_order_and_counts_every_offer() {
        let mut forward = Best::new(3);
        let mut backward = Best::new(3);
        for key in 0..100 {
            forward.offer(key);
            backward.offer(99 - key);
        }
        assert_eq!(forward.into_sorted(), (100, vec![0, 1, 2]));
        assert_eq!(backward.into_sorted(), (100, vec![0, 1, 2]));
        let mut none = Best::new(0);
        none.offer(1);
        assert_eq!(none.into_sorted(), (1, vec![]));
    }

    /// Two selections merged hold what one offered both their keys holds.
    #[test]
    fn a_merge_is_one_selection_of_both() {
        let (mut odd, mut even) = (Best::new(3), Best::new(3));
        for key in 0..50 {
            if key % 2 == 0 {
                even.offer(key)
            } else {
                odd.offer(key)
            }
        }
        assert_eq!(odd.merge(even).into_sorted(), (50, vec![0, 1, 2]));
    }
}

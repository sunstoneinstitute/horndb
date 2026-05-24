//! `Zset<K>` — a multiplicity-weighted set over keys of type `K`.
//!
//! Invariant: no key maps to multiplicity 0. Adding `-m` to a key with
//! existing multiplicity `m` removes the row entirely.
//!
//! This is the F1 storage primitive from SPEC-06. We use `BTreeMap` for
//! deterministic iteration order (needed by the change-feed ordering
//! guarantee in acceptance #5) and to give the bilinear join a predictable
//! merge pattern. Hash-based variants are a Stage-2 optimization.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

use crate::types::Multiplicity;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Zset<K: Ord + Clone> {
    inner: BTreeMap<K, Multiplicity>,
}

impl<K: Ord + Clone> Zset<K> {
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns the current multiplicity of `key`, or 0 if absent.
    pub fn get(&self, key: &K) -> Multiplicity {
        self.inner.get(key).copied().unwrap_or(0)
    }

    /// Adds `delta` to the multiplicity of `key`. Removes the row if the
    /// resulting multiplicity is zero.
    pub fn add(&mut self, key: K, delta: Multiplicity) {
        if delta == 0 {
            return;
        }
        match self.inner.entry(key) {
            Entry::Occupied(mut o) => {
                let v = o.get_mut();
                *v += delta;
                if *v == 0 {
                    o.remove();
                }
            }
            Entry::Vacant(v) => {
                v.insert(delta);
            }
        }
    }

    /// Pointwise sum: `self += other`. Drops zero results.
    pub fn add_assign(&mut self, other: &Zset<K>) {
        for (k, &m) in &other.inner {
            self.add(k.clone(), m);
        }
    }

    /// Pointwise subtraction: `self -= other`.
    pub fn sub_assign(&mut self, other: &Zset<K>) {
        for (k, &m) in &other.inner {
            self.add(k.clone(), -m);
        }
    }

    /// Iterate `(&K, multiplicity)` pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, Multiplicity)> {
        self.inner.iter().map(|(k, &m)| (k, m))
    }

    /// Construct from an iterator of `(key, multiplicity)` pairs.
    /// Duplicate keys are summed; zero results are dropped.
    ///
    /// Deliberately an inherent method rather than `FromIterator` because
    /// our element type is `(K, Multiplicity)` not `K`, which would make
    /// the trait impl misleading — `Zset::from_iter([1, 2, 3])` does not
    /// mean what a casual reader expects.
    #[allow(clippy::should_implement_trait)]
    pub fn from_iter<I: IntoIterator<Item = (K, Multiplicity)>>(it: I) -> Self {
        let mut z = Self::new();
        for (k, m) in it {
            z.add(k, m);
        }
        z
    }
}

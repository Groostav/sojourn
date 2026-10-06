//! The GPU sieve, for the throughput fixture and nothing else.
//!
//! Hidden because the sieve is an implementation detail of brute force; this
//! exists so that `tests/brute_squad.rs` can measure it and record the rate
//! beside the CPU's, on the same candidates.

use faer::MatRef;

use crate::{ConstraintSystem, GpuTarget, Point};

/// A compiled sieve for one system, or `None` without an adapter.
pub struct Sieve {
    inner: super::sieve::Sieve,
}

#[must_use]
pub fn sieve_for(system: &ConstraintSystem) -> Option<Sieve> {
    super::sieve::Sieve::new(system, &GpuTarget::Default).map(|inner| Sieve { inner })
}

/// The adapter's name and backend, or `None` without one.
#[must_use]
pub fn adapter_name() -> Option<String> {
    super::sieve::adapter_name(&GpuTarget::Default)
}

impl Sieve {
    /// Indices of the columns of `candidates` that survive the `f32`
    /// pass; `None` if the device failed.
    #[must_use]
    pub fn survivors_of(&self, candidates: MatRef<'_, f64>) -> Option<Vec<usize>> {
        self.inner.sieve_given(candidates)
    }

    /// `count` candidates drawn on the device from `(base, batch)`,
    /// sieved; the survivors as points. `None` if the device failed.
    #[must_use]
    pub fn survivors_generated(&self, base: u64, batch: u64, count: u32) -> Option<Vec<Point>> {
        self.inner.sieve_generated(base, batch, count)
    }
}

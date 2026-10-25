//! A computation limit per public entry point: a spin becomes a `panic!`.
//!
//! # Why
//!
//! Z3 left this crate because it could not be trusted to return: a timeout
//! and a resource limit were both set, and cases that spun anyway were easy
//! to find. A search of our own that can do the same would be the same flaw
//! with less excuse. Every loop here already carries a count of its own, and
//! that turned out not to be enough: before `1ad8b39`, a repair over eighty
//! continuous variables and three discrete ones spent 140 s with every loop
//! inside its bound — sixty-four assignments of levels, each two Newton
//! projections of up to 128 iterations, 5120 draws and `64·(d+1)` COBYLA
//! evaluations — because the bounds multiply and each was sized for its own
//! stage. A count per loop answers "does this loop end"; this answers "does
//! this *call* end soon", which is what looked hung.
//!
//! # What it is
//!
//! A watchdog in the units of a count. [`enter`] at the top of each public
//! entry point installs a [`Meter`] for the calling thread, and every loop
//! reachable from there calls [`tick`] as its first statement. A call whose
//! ticks pass its limit panics, naming the entry point. The limit is set far
//! above what any input spends — the measured headroom is in `docs/todo.md`
//! under *Standing* — so reaching it is a bug, never an outcome: it never
//! decides an answer, only that something is broken, and that is why it is a
//! panic and not an error a caller would plan around. A budget is a count
//! that decides the answer; a ceiling is a watchdog that decides only that
//! something is broken; this is a watchdog that is also a count, so it trips
//! at the same point on every machine.
//!
//! A limit scales with what the caller asked for — a design of a million
//! points, a brute-force budget of a trillion proposals — so that no legal
//! request is refused by it ([`for_solve`], [`for_sample`], [`for_repair`]).
//! [`RLIMIT_VARIABLE`] multiplies every limit. No limit and no count is part
//! of the API: a revision that ticks in a loop it did not before is a legal
//! change, and is how a newly found spin is closed.
//!
//! # Ticks
//!
//! Every loop's first statement is `rlimit::tick()`, hot loops included — a
//! feasibility check's loop over constraints, a batch's loop over columns —
//! because ticks have to track work: the 140 s repair ran few iterations,
//! each expensive, and a meter on the outer loops alone would have counted
//! almost none of it. The evaluator's per-instruction loops are the one
//! exception, charged once per run at the per-tile loop and at `eval_row`.
//! A tick is a thread-local increment and a compare; the shared count is
//! touched once every [`FLUSH`] ticks, so brute force's workers do not
//! contend on it, and the limit is checked there — a call panics no sooner
//! than its limit and within a flush of it per thread.
//!
//! Outside every entry point a tick counts nothing, so the engine's own
//! unit tests, which call `cvg` directly, are not metered, and neither is
//! `ConstraintSystem::new`, whose loops are bounded by the syntax tree.
//!
//! # Threads
//!
//! The meter is the calling thread's. Brute force hands it to its workers
//! with [`current`] and [`adopt`], and they spend from the same count; the
//! first to pass the limit panics, every other at its next flush, and the
//! join re-raises the first payload so the message survives `thread::scope`.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::solve::RLIMIT_VARIABLE;

/// Ticks a thread counts locally before it adds them to the shared meter.
const FLUSH: u64 = 1024;

/// The public call a meter is counting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Solve,
    Sample,
    Repair,
}

impl Entry {
    fn name(self) -> &'static str {
        match self {
            Self::Solve => "solve",
            Self::Sample => "sample",
            Self::Repair => "repair",
        }
    }
}

/// What one entry point's call has spent, shared by every thread it runs on.
pub(crate) struct Meter {
    entry: Entry,
    limit: u64,
    spent: AtomicU64,
    zz_units: Vec<u64>, // ZZ calibration, temporary
}

/// The guard [`enter`] and [`adopt`] return. The scope that installed a
/// meter removes it when dropped; one that found a meter already installed
/// leaves it alone.
pub(crate) struct Scope {
    installed: bool,
    root: bool,
}

thread_local! {
    static METER: RefCell<Option<Arc<Meter>>> = const { RefCell::new(None) };
    static PENDING: Cell<u64> = const { Cell::new(0) };
}

/// Meters the calling thread for `entry` until the scope drops, unless a
/// meter is already installed there, which then keeps counting: a call that
/// enters another is one call.
pub(crate) fn enter(entry: Entry, limit: u64) -> Scope {
    let installed = install(Arc::new(Meter {
        entry,
        limit,
        spent: AtomicU64::new(0),
        zz_units: ZZ_UNITS.with(|u| u.take()), // ZZ calibration, temporary
    }));
    Scope {
        installed,
        root: installed,
    }
}

/// Meters a worker thread against the meter its spawner was [`current`] on,
/// so the call's threads spend from one count.
pub(crate) fn adopt(meter: Option<Arc<Meter>>) -> Scope {
    Scope {
        installed: meter.is_some_and(install),
        root: false,
    }
}

/// The calling thread's meter, to hand to a worker.
pub(crate) fn current() -> Option<Arc<Meter>> {
    METER.with(|slot| slot.borrow().clone())
}

/// One iteration of a loop. See the module doc.
pub(crate) fn tick() {
    let pending = PENDING.with(|pending| {
        let n = pending.get() + 1;
        pending.set(if n >= FLUSH { 0 } else { n });
        n
    });
    if pending >= FLUSH {
        flush(pending);
    }
}

/// Work done in foreign code between two of our calls, as `units` ticks.
/// See the module doc.
pub(crate) fn charge(units: u64) {
    let pending = PENDING.with(|pending| {
        let n = pending.get().saturating_add(units);
        pending.set(if n >= FLUSH { 0 } else { n });
        n
    });
    if pending >= FLUSH {
        flush(pending);
    }
}

/// Installs `meter` on this thread, unless one is there already.
fn install(meter: Arc<Meter>) -> bool {
    METER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return false;
        }
        *slot = Some(meter);
        PENDING.with(|pending| pending.set(0));
        true
    })
}

/// Adds `ticks` to the thread's meter, and panics if that passes its limit.
fn flush(ticks: u64) {
    let passed = METER.with(|slot| {
        let slot = slot.borrow();
        let meter = slot.as_ref()?;
        let spent = meter.spent.fetch_add(ticks, Ordering::Relaxed) + ticks;
        (spent > meter.limit).then_some((meter.entry, meter.limit))
    });
    if let Some((entry, limit)) = passed {
        panic!(
            "sojourn's {} spent its computation limit of {limit} ticks. No supported \
             input reaches it, so this is a bug in sojourn and not in the call: please \
             report the system and the call that reached it. {RLIMIT_VARIABLE} scales \
             the limit, which is not part of the API and changes between revisions.",
            entry.name()
        );
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if !self.installed {
            return;
        }
        let pending = PENDING.with(|pending| pending.replace(0));
        let meter = METER
            .with(|slot| slot.borrow_mut().take())
            .expect("a scope that installed a meter finds it still there");
        // Never panics here: a drop may run while unwinding already.
        let spent = meter.spent.fetch_add(pending, Ordering::Relaxed) + pending;
        if self.root {
            zz_record(meter.entry.name(), spent, &meter.zz_units); // ZZ calibration, temporary
            tracing::debug!(
                entry = meter.entry.name(),
                spent,
                limit = meter.limit,
                "metered"
            );
        }
    }
}

/// [`RLIMIT_VARIABLE`]'s factor, read once.
fn factor() -> f64 {
    static FACTOR: OnceLock<f64> = OnceLock::new();
    *FACTOR.get_or_init(|| {
        let Some(value) = std::env::var_os(RLIMIT_VARIABLE) else {
            return 1.0;
        };
        match value.to_str().map(|text| text.trim().parse::<f64>()) {
            Some(Ok(factor)) if factor.is_finite() && factor > 0.0 => factor,
            _ => panic!("{RLIMIT_VARIABLE} is {value:?}; it must be a positive number"),
        }
    })
}

/// `base + Σ per · units`, scaled by [`factor`], saturating: a limit too
/// large to count to is one that never trips, which is the safe direction
/// for a watchdog to err in.
fn scaled(base: u64, terms: &[(u64, u64)]) -> u64 {
    let raw = terms.iter().fold(base, |sum, (per, units)| {
        sum.saturating_add(per.saturating_mul(*units))
    });
    // A cast from f64 saturates, so a factor that overflows is u64::MAX.
    (raw as f64 * factor()) as u64
}

// Each about a hundred times the worst the release suite spends per unit
// (`docs/todo.md`, *Standing*), except brute force's per proposal: that term
// is linear and deterministic — two ticks a proposal, give or take the
// system's size — so three times its worst keeps a stalled brute force to
// seconds on a budget of a million and minutes, not hours, on the default.
const SOLVE_BASE: u64 = 100_000_000;
const SOLVE_PER_SIZE_SQUARED: u64 = 1_000_000;
const SOLVE_PER_PROPOSAL: u64 = 2;
const SOLVE_PER_GPU_BATCH: u64 = 1_000_000;
const SAMPLE_BASE: u64 = 100_000_000;
const SAMPLE_PER_PAIR: u64 = 64_000;
const REPAIR_BASE: u64 = 20_000_000;
const REPAIR_PER_SIZE_SQUARED: u64 = 70_000;

/// `solve`'s limit, for a system of `size` rows and constraints together and
/// the brute-force budgets it was given. The walker's burn-in and the local
/// solve go as rows times constraints; brute force as its proposals, each
/// judged against the system.
pub(crate) fn for_solve(size: usize, proposals: u64, gpu_batches: u64) -> u64 {
    zz_units(&[size as u64, proposals, gpu_batches]); // ZZ calibration, temporary
    let size = size as u64;
    scaled(
        SOLVE_BASE,
        &[
            (SOLVE_PER_SIZE_SQUARED, size.saturating_mul(size)),
            (SOLVE_PER_PROPOSAL, proposals.saturating_mul(size)),
            (SOLVE_PER_GPU_BATCH, gpu_batches),
        ],
    )
}

/// `sample`'s limit, for a pool of `pool` candidates from which `chosen`
/// points are spread — the caller's and the design's together — over a
/// system of `size` rows and constraints. Farthest-first is a pass over the
/// pool per point chosen, and the walker's extension goes as the pool
/// times the system.
pub(crate) fn for_sample(size: usize, pool: usize, chosen: usize) -> u64 {
    zz_units(&[size as u64, pool as u64, chosen as u64]); // ZZ calibration, temporary
    let pairs = (pool as u64).saturating_mul((chosen as u64).saturating_add(size as u64));
    scaled(SAMPLE_BASE, &[(SAMPLE_PER_PAIR, pairs)])
}

/// `repair`'s limit, for a system of `size` rows and constraints together:
/// each assignment of levels is a derivative-free solve whose evaluations go
/// as the dimension, each judged against every constraint.
pub(crate) fn for_repair(size: usize) -> u64 {
    zz_units(&[size as u64]); // ZZ calibration, temporary
    let size = size as u64;
    scaled(
        REPAIR_BASE,
        &[(REPAIR_PER_SIZE_SQUARED, size.saturating_mul(size))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panic_message(act: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let payload = std::panic::catch_unwind(act).expect_err("the call should panic");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .expect("a panic message")
    }

    /// The limit is checked when a thread's ticks are flushed, every
    /// [`FLUSH`], so a meter panics no sooner than its limit and no later
    /// than a flush past it.
    #[test]
    fn a_scope_panics_past_its_limit_and_names_its_entry() {
        // setup
        let limit = 10;

        // act
        let message = panic_message(|| {
            let _scope = enter(Entry::Repair, limit);
            for _ in 0..limit + FLUSH {
                tick();
            }
        });
        let within = std::panic::catch_unwind(|| {
            let _scope = enter(Entry::Repair, 3 * FLUSH);
            for _ in 0..3 * FLUSH {
                tick();
            }
        });

        // assert
        assert!(message.contains("repair"), "{message}");
        assert!(message.contains(RLIMIT_VARIABLE), "{message}");
        assert!(within.is_ok(), "a call within its limit does not panic");
    }

    /// A call that enters another keeps the outer meter: the inner scope
    /// neither resets the count nor ends it.
    #[test]
    fn a_nested_scope_keeps_the_outer_meter() {
        // act
        let message = panic_message(|| {
            let _outer = enter(Entry::Solve, 2 * FLUSH);
            for _ in 0..FLUSH {
                tick();
            }
            {
                let _inner = enter(Entry::Repair, u64::MAX);
                for _ in 0..FLUSH {
                    tick();
                }
            }
            for _ in 0..FLUSH {
                tick();
            }
        });

        // assert
        assert!(message.contains("solve"), "{message}");
    }

    /// Workers that adopt a meter spend from one count, and once one has
    /// passed the limit every other panics at its next flush.
    #[test]
    fn adopting_threads_share_one_count() {
        // act
        let message = panic_message(|| {
            let _scope = enter(Entry::Solve, 4 * FLUSH);
            let meter = current();
            assert!(meter.is_some(), "a scope is current on its own thread");
            std::thread::scope(|scope| {
                let workers: Vec<_> = (0..2)
                    .map(|_| {
                        let meter = meter.clone();
                        scope.spawn(move || {
                            let _adopted = adopt(meter);
                            // Far past the limit, so neither can finish first
                            // and stay clear of it.
                            for _ in 0..100 * FLUSH {
                                tick();
                            }
                        })
                    })
                    .collect();
                let outcomes: Vec<_> = workers.into_iter().map(|w| w.join()).collect();
                assert!(
                    outcomes.iter().all(Result::is_err),
                    "both workers pass the shared limit"
                );
                std::panic::resume_unwind(
                    outcomes
                        .into_iter()
                        .find_map(Result::err)
                        .expect("a worker panicked"),
                );
            });
        });

        // assert
        assert!(message.contains("solve"), "{message}");
    }

    /// Engine code called outside every entry point, as the unit tests call
    /// it, is not metered.
    #[test]
    fn a_tick_outside_every_scope_is_free() {
        // act
        let outcome = std::panic::catch_unwind(|| {
            for _ in 0..4 * FLUSH {
                tick();
            }
        });

        // assert
        assert!(outcome.is_ok());
        assert!(current().is_none());
    }
}

// ZZ calibration, temporary: not to be committed.
thread_local! {
    static ZZ_UNITS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}
fn zz_units(units: &[u64]) {
    ZZ_UNITS.with(|u| *u.borrow_mut() = units.to_vec());
}
fn zz_record(entry: &str, spent: u64, units: &[u64]) {
    use std::io::Write;
    if let Some(path) = std::env::var_os("ZZ_CALIBRATE") {
        let test = std::thread::current().name().unwrap_or("?").to_owned();
        let line = format!("{entry};{spent};{units:?};{test}\n");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(line.as_bytes()).unwrap();
    }
}

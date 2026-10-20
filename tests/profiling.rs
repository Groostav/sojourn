//! Targets for a profiler, ignored by default: each runs one expensive path
//! long enough to sample and asserts only that it returned. Not measurements —
//! the counts the engine reports at `debug` are those — but the thing to point
//! `samply` at when the counts say where and not why.
//!
//! ```text
//! CARGO_PROFILE_RELEASE_DEBUG=1 cargo test --release --test profiling --no-run
//! samply record target/release/deps/profiling-<hash>.exe a_design_on_the_100_segment_beam --ignored --nocapture
//! ```

mod common;

use faer::Mat;
use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{ConstraintSolver, ConstraintSystem, InputVariable, Strategy};

/// The walker at two hundred variables under two dense constraints: eight
/// chains burnt in for 3200 steps each, then a pool of 148 points walked at
/// 400 steps of thinning apiece. 14 s in release on 2026-09-16 with the axis
/// move's slice walking the deflection sums' ASTs; 3.4 s with the slice on
/// the interval tape; then the burn-in moved into `solve` (1.26 s there,
/// 3.0 s in the walk).
#[test]
#[ignore = "a profiling target, not a check"]
fn a_design_on_the_100_segment_beam() -> anyhow::Result<()> {
    let system = common::stepped_beam(100)?;
    let region = ConstraintSolver::new()
        .with_strategies(vec![
            Strategy::BruteSquad,
            Strategy::LocalSolve,
            Strategy::HitAndRun,
        ])
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(0))?;
    let design = region.sample(
        Mat::zeros(0, 0).as_ref(),
        10,
        &mut Xoshiro256PlusPlus::seed_from_u64(7),
    )?;
    assert_eq!(design.ncols(), 10);
    Ok(())
}

/// The walker under driving: a hundred variables and ninety-nine chained
/// equalities, so every step retracts ninety-nine coordinates through their
/// slices. About 2 s in release on 2026-09-16.
#[test]
#[ignore = "a profiling target, not a check"]
fn a_design_on_the_99_equation_chain() -> anyhow::Result<()> {
    let variables: Vec<InputVariable> = (1..=100)
        .map(|i| InputVariable::new_continuous(format!("x{i}"), 0.0, 1.0))
        .collect();
    let sources: Vec<String> = (1..100)
        .map(|i| format!("x{i} + x{} == 1 +/- 0.000001", i + 1))
        .collect();
    let system = ConstraintSystem::new(variables, sources)?;
    let region =
        ConstraintSolver::new().solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(0))?;
    let design = region.sample(
        Mat::zeros(0, 0).as_ref(),
        16,
        &mut Xoshiro256PlusPlus::seed_from_u64(7),
    )?;
    assert_eq!(design.ncols(), 16);
    Ok(())
}

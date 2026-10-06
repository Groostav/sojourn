//! Constraints built to defeat every seeder but one, so the pool has to
//! survive on what is left.
//!
//! A real literal exponent is the one power shape every backend refuses to turn
//! into multiplication: `x^1.234` is `exp(1.234 * ln x)`, which has no inverse
//! interval narrowing will use, so nothing contracts through it (the solver
//! this crate used to link had no `exp` either, and ran minutes past its
//! limit on exactly this exponent — `docs/todo.md`, "Z3 holes"). A local
//! solve finds a point on a band of it; where nothing does, the pool retreats
//! to sampling: `powf` on every CPU lane and `pow` on the GPU when the sieve
//! is compiled in, both of them bound by the special-function hardware rather
//! than by arithmetic.
//!
//! The contract these pin: sampling finds what is there to be found; what an
//! enclosure rules out is proved before a proposal is spent; and what it
//! cannot find and nothing can rule out is reported as *not found* — never as
//! proved empty — with the sentence a caller can act on.

mod common;

use anyhow::Context;
use faer::Mat;
use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{
    ConstraintSolver, ConstraintSystem, FeasibleRegion, Infeasibility, InputVariable, Strategy,
};

/// Same value the other cvg suites use, so a point seen in one is the point
/// seen in another.
const SEED: u64 = 0x50_50_1E_5E_ED;

/// A fixture's [`ConstraintSystem`]; one that does not bind is the test's error.
fn system(
    variables: &[(&str, f64, f64)],
    constraints: &[&str],
) -> anyhow::Result<ConstraintSystem> {
    let variables = variables
        .iter()
        .map(|(name, low, high)| InputVariable::new(*name, *low, *high))
        .collect();
    Ok(ConstraintSystem::new(
        variables,
        constraints.iter().copied(),
    )?)
}

/// Whether every constraint holds at `point`, judged independently of the
/// pool through the public evaluator, strictly: the residual must be `<= 0`.
fn holds(system: &ConstraintSystem, point: &[f64]) -> bool {
    let bindings: Vec<(&str, f64)> = system
        .variables()
        .iter()
        .zip(point)
        .map(|(variable, value)| (variable.name.as_str(), *value))
        .collect();
    system.constraints().all(|constraint| {
        common::eval_one(constraint, &bindings).is_ok_and(|residual| residual <= 0.0)
    })
}

fn in_box(system: &ConstraintSystem, point: &[f64]) -> bool {
    system
        .variables()
        .iter()
        .zip(point)
        .all(|(variable, value)| (variable.lower_bound..=variable.upper_bound).contains(value))
}

/// The solver every fixture here starts from: the test-sized budget, the CPU
/// alone so the verdict is a function of the generator's state.
fn solver() -> ConstraintSolver {
    ConstraintSolver::new().with_proposal_budget(common::PROPOSAL_BUDGET)
}

/// The generator every solve here draws from.
fn rng() -> Xoshiro256PlusPlus {
    Xoshiro256PlusPlus::seed_from_u64(SEED)
}

/// The region a solve returns, which is where `repair` lives. The fixture
/// below hands it its own anchor, so the census is not what is being tested.
fn region(system: &ConstraintSystem) -> anyhow::Result<FeasibleRegion> {
    solver()
        .solve(system, &mut rng())
        .context("the fixture should be satisfiable")
}

/// A curve `x1^1.234 + x2^1.234 == 5` thickened to a band a hundredth wide in
/// a hundred-unit box: about one proposal in two thousand lands, which is
/// well inside brute force's reach and far outside the probe's luck. The
/// solver can say nothing about it, so nothing here was proved; every point
/// was sampled or walked from a sampled or locally solved seed.
#[test]
fn a_thin_curve_is_found_without_the_solver_helping() -> anyhow::Result<()> {
    const WANTED: usize = 10;

    let system = system(
        &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)],
        &["x1^1.234 + x2^1.234 == 5 +/- 0.01"],
    )?;
    let samples = solver()
        .solve(&system, &mut rng())
        .context("a curve that sampling can reach should be found")?;

    let points = samples.sample(Mat::zeros(0, 0).as_ref(), WANTED, &mut rng())?;
    assert_eq!(points.ncols(), WANTED, "the stream ended early");
    for column in 0..points.ncols() {
        let point: Vec<f64> = (0..points.nrows())
            .map(|row| points[(row, column)])
            .collect();
        assert!(holds(&system, &point), "{point:?} is off the curve");
        assert!(in_box(&system, &point), "{point:?} is outside the box");
    }
    Ok(())
}

/// `x^1.234` never reaches a million on `[0, 10]`, and an enclosure says so:
/// a real exponent has no inverse to narrow through, but the forward check
/// needs none — the whole box evaluates to at most `10^1.234`, and that is a
/// proof, before a single proposal is spent on it.
#[test]
fn what_an_enclosure_rules_out_is_proved_before_sampling() -> anyhow::Result<()> {
    let source = "x1^1.234 > 1000000";
    let system = system(&[("x1", 0.0, 10.0)], &[source])?;
    let verdict = solver().solve(&system, &mut rng());

    let Err(because) = verdict else {
        panic!("a point beyond the box was reported {verdict:?}");
    };
    let Infeasibility::Proved { blamed } = because else {
        panic!("the enclosure rules this out, yet it was reported {because:?}");
    };
    let named: Vec<&str> = blamed.iter().map(|c| c.source.as_str()).collect();
    assert_eq!(named, vec![source]);
    Ok(())
}

/// `var[n]` reads whichever coordinate the point says, so no enclosure can
/// see through it and nothing can prove that neither coordinate reaches
/// twenty on `[0, 10]`; sampling can only report that it found nothing. The
/// verdict has to say exactly that, name the constraint nothing could be
/// concluded from, and stop short of calling the region empty.
///
/// This spends brute force's whole budget — a billion proposals across
/// every thread in release, about ten seconds — because giving up early
/// would be the bug.
#[test]
fn what_sampling_cannot_find_is_reported_not_proved() -> anyhow::Result<()> {
    let source = "var[floor(n)] > 20";
    let system = system(
        &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0), ("n", 1.0, 2.0)],
        &[source],
    )?;
    let verdict = solver().solve(&system, &mut rng());

    let Err(because) = verdict else {
        panic!("a point beyond the box was reported {verdict:?}");
    };
    let sentence = because.to_string();
    let Infeasibility::NotFound { unexpressed } = because else {
        panic!("nothing could have proved this empty, yet it was reported {because:?}");
    };
    let named: Vec<&str> = unexpressed.iter().map(|c| c.source.as_str()).collect();
    assert_eq!(named, vec![source]);
    assert!(
        sentence.contains("sampling found nothing") && sentence.contains(source),
        "the verdict should read as a sentence a caller can act on, got {sentence:?}"
    );
    Ok(())
}

/// A thin band, seeded by the local solve alone: a real exponent has no
/// inverse for a contraction to narrow through and a band this thin is
/// outside the probe's luck, but a point on it is an ordinary constrained
/// optimisation from the box centre, and that is what the local solve is
/// for.
#[test]
fn a_thin_curve_is_seeded_by_the_local_solve() -> anyhow::Result<()> {
    let system = system(
        &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)],
        &["x1^1.234 + x2^1.234 == 5 +/- 0.01"],
    )?;
    let region = solver()
        .with_strategies(vec![Strategy::LocalSolve, Strategy::HitAndRun])
        .solve(&system, &mut rng())
        .context("the band is not empty")?;
    let points = region.sample(Mat::zeros(0, 0).as_ref(), 4, &mut rng())?;
    assert_eq!(
        points.ncols(),
        4,
        "the walker should carry on from the seed"
    );
    for column in 0..points.ncols() {
        let point: Vec<f64> = (0..points.nrows())
            .map(|row| points[(row, column)])
            .collect();
        assert!(holds(&system, &point), "{point:?} is off the curve");
    }
    Ok(())
}

/// Repair has no interval to clamp to either — narrowing declines a real
/// exponent — so the projection is all it has, and that is enough: the point
/// lands inside, judged by the evaluator alone. The clearance has to come
/// from the projection too, since no slice can say where the wall is to step
/// off it: the rows carry the margin, and failing that the landing is backed
/// off along the projection's own direction until the axis neighbours pass.
#[test]
fn repair_lands_without_an_interval_to_clamp_to() -> anyhow::Result<()> {
    const CLEARANCE: f64 = 1e-3;
    let system = system(
        &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)],
        &["x1^1.234 + x2^1.234 < 5"],
    )?;
    let region = region(&system)?;
    let repaired = region
        .repair(&[9.0, 9.0], CLEARANCE)
        .context("the origin is feasible, so something is reachable")?;

    assert!(
        holds(&system, &repaired),
        "{repaired:?} is outside the region"
    );
    assert!(
        in_box(&system, &repaired),
        "{repaired:?} is outside the box"
    );
    for coordinate in 0..2 {
        for sign in [-1.0, 1.0] {
            let mut neighbour = repaired.clone();
            neighbour[coordinate] += sign * CLEARANCE * 10.0;
            assert!(
                holds(&system, &neighbour) && in_box(&system, &neighbour),
                "{repaired:?} lacks the clearance: {neighbour:?} is outside"
            );
        }
    }
    Ok(())
}

/// A hundred variables under ninety-nine chained equalities, `x_i + x_{i+1}
/// == 1` at `1e-6`: one degree of freedom, and every other coordinate driven
/// from it in a chain ninety-nine deep. What this pins is that the matching
/// in `classify::plan` — every equation wanting the variable its neighbour
/// wants too — and the ordering after it stay cheap at this size, and that
/// the whole pipeline then moves along the line rather than sitting on its
/// seed: the design's free coordinate spans the box.
///
/// The ceiling is a watchdog in the five-sigma sense, not a budget. Measured:
/// the matching and the solve are 70 ms in a debug build; the design is 13 s
/// there and 2 s in release, all of it the walker's burn-in — two thousand
/// steps on eight chains, twice, each step retracting ninety-nine driven
/// coordinates through their slices. Reaching the ceiling means the matching
/// or the ordering went combinatorial, which is the thing this is here to
/// catch.
#[test]
fn a_chain_of_ninety_nine_equalities_is_matched_and_walked() -> anyhow::Result<()> {
    const WIDTH: f64 = 0.000_001;
    let names: Vec<String> = (1..=100).map(|i| format!("x{i}")).collect();
    let variables: Vec<(&str, f64, f64)> = names.iter().map(|n| (n.as_str(), 0.0, 1.0)).collect();
    let sources: Vec<String> = (1..100)
        .map(|i| format!("x{i} + x{} == 1 +/- {WIDTH}", i + 1))
        .collect();
    let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
    let system = system(&variables, &sources)?;

    let started = std::time::Instant::now();
    let region = region(&system)?;
    let design = region.sample(Mat::zeros(0, 0).as_ref(), 16, &mut rng())?;
    let took = started.elapsed();

    assert_eq!(design.ncols(), 16);
    let mut lowest = f64::INFINITY;
    let mut highest = f64::NEG_INFINITY;
    for column in 0..design.ncols() {
        let point: Vec<f64> = (0..design.nrows())
            .map(|row| design[(row, column)])
            .collect();
        assert!(
            in_box(&system, &point) && holds(&system, &point),
            "{point:?}"
        );
        lowest = lowest.min(point[0]);
        highest = highest.max(point[0]);
    }
    assert!(
        highest - lowest > 0.5,
        "x1 spans only {lowest}..{highest}: the chain was not walked along"
    );
    assert!(
        took < std::time::Duration::from_secs(60),
        "matching and walking a chain of ninety-nine took {took:?}"
    );
    Ok(())
}

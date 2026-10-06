//! Constraints with a jump in them — `floor`, `ceil`, `sgn`, `%` — and
//! constraints that fault, put to every rung of the ladder and to repair on
//! the unhappy path. The sibling `torture_tests.rs` is the smooth case that
//! nothing can narrow through (a real exponent); this is the case where
//! nothing is even continuous.
//!
//! What makes these shapes dangerous is not that they are hard to sample —
//! most are fat — but what they do to every tool that is not a sampler. A
//! jump has no gradient, so Newton declines and COBYLA is the projection;
//! COBYLA fits linear models, and a staircase's are zero or nonsense. A
//! remainder's interval image is a magnitude bound and nothing narrows back
//! through it, so a contradiction written in `%` is invisible to the
//! enclosure at every scale, and the only honest verdict is *not found*. A
//! fault (`ln` of a negative, `%` by zero, a subscript off the schema) is a
//! residual of `INFINITY` to the local solve and a rejection to everything
//! else, and a proposal that faults can still be what a caller asks to have
//! repaired.
//!
//! The contract these pin, on every shape: **the search returns** — every
//! rung is a count and a solve that has spent them says so; **a verdict is
//! honest** — a satisfiable system is never `Proved` empty, and a point that
//! is delivered holds by the public evaluator; and **repair lands or says
//! why** — never a panic, never a point that fails the oracle.
//!
//! Where a case's answer is "the local solve said no", that is the thing
//! being tested: `Strategy::LocalSolve` alone with brute force zeroed puts
//! COBYLA on the spot with nothing to hide behind, and the evaluations it
//! traces are counted against the budget it announced.
//!
//! Two fixture rules, both learnt here. **A box per coordinate is offset**
//! (`i/7`) wherever several coordinates share a constraint's shape:
//! identical boxes put every coordinate in the same cell at the centre and
//! at every vertex of COBYLA's simplex, and a uniform step lands all forty
//! in an even cell at once — the comb was "found" by symmetry, not search.
//! **A jump is spelled as inequalities** where COBYLA's stepping is the
//! subject: `floor(x) == 3 +/- t` is *driven* — `floor` inverts, so
//! `classify::centre` puts `x` on `[2.9, 4.1]` before any point is judged —
//! and the solve lands on the first evaluation without stepping at all.

mod common;

use std::time::{Duration, Instant};

use anyhow::Context;
use common::Captured;
use faer::Mat;
use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{
    ConstraintSolver, ConstraintSystem, FeasibleRegion, Infeasibility, InputVariable, RepairError,
    SampleError, Strategy,
};

/// Same value the other cvg suites use, so a point seen in one is the point
/// seen in another.
const SEED: u64 = 0x50_50_1E_5E_ED;

/// A watchdog, not a budget, on the two calls that run foreign code: the
/// local solve alone and a repair, whose counts are a few hundred
/// evaluations and whose honest cost is under a second of a debug build.
/// Reaching this means a loop that is not counting — the bug this suite
/// exists to catch — and nextest's own limit would kill the test at sixty
/// without saying which call; this names it. A full-ladder solve is not
/// held to it: its cost is brute force's budget, a count spent honestly,
/// which is the documented price of a shrug and takes what the machine
/// takes.
const CEILING: Duration = Duration::from_secs(10);

/// The lower bound of the `i`th of several coordinates: distinct per
/// coordinate, so that no two share a cell boundary or a centre. See the
/// module doc for why.
#[expect(clippy::cast_precision_loss, reason = "a coordinate index is small")]
fn offset(i: usize) -> f64 {
    i as f64 / 7.0
}

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

/// `count` coordinates named `x1..`, each on a ten-wide box offset by its
/// index, under one constraint per coordinate per shape, `{n}` standing for
/// the name; the sources come back in the order the system holds them.
fn per_coordinate(
    count: usize,
    shapes: &[&str],
) -> anyhow::Result<(ConstraintSystem, Vec<String>)> {
    let names: Vec<String> = (1..=count).map(|i| format!("x{i}")).collect();
    let variables: Vec<InputVariable> = names
        .iter()
        .enumerate()
        .map(|(i, name)| InputVariable::new(name.clone(), offset(i), 10.0 + offset(i)))
        .collect();
    let sources: Vec<String> = shapes
        .iter()
        .flat_map(|shape| names.iter().map(|name| shape.replace("{n}", name)))
        .collect();
    let system = ConstraintSystem::new(variables, sources.iter().cloned())?;
    Ok((system, sources))
}

/// Whether every constraint holds at `point`, judged independently of the
/// pool through the public evaluator, strictly: the residual must be `<= 0`,
/// and a residual that cannot be evaluated is not a pass.
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

/// COBYLA with nothing to hide behind: the local solve alone, no
/// contraction to prove anything and no brute force to find it.
fn cobyla_alone() -> ConstraintSolver {
    ConstraintSolver::new()
        .with_strategies(vec![Strategy::LocalSolve])
        .with_proposal_budget(0)
}

/// The generator every solve here draws from.
fn rng() -> Xoshiro256PlusPlus {
    Xoshiro256PlusPlus::seed_from_u64(SEED)
}

/// The columns of a design as points.
fn columns(design: &Mat<f64>) -> Vec<Vec<f64>> {
    (0..design.ncols())
        .map(|column| {
            (0..design.nrows())
                .map(|row| design[(row, column)])
                .collect()
        })
        .collect()
}

/// Every point of a design holds and is in the box, or the design is the
/// complaint.
fn assert_all_hold(system: &ConstraintSystem, points: &[Vec<f64>]) {
    for point in points {
        assert!(holds(system, point), "{point:?} fails a constraint");
        assert!(in_box(system, point), "{point:?} is outside the box");
    }
}

/// The verdict an unsatisfiable system the enclosure cannot see gets: not
/// found, never proved. The names of the constraints nothing narrowed
/// through come back for the caller to check.
fn assert_not_found(verdict: Result<FeasibleRegion, Infeasibility>) -> Vec<String> {
    let Err(because) = verdict else {
        panic!("a point was delivered for a system with no feasible point: {verdict:?}");
    };
    let Infeasibility::NotFound { unexpressed } = because else {
        panic!("nothing could have proved this empty, yet it was reported {because:?}");
    };
    unexpressed
        .iter()
        .map(|constraint| constraint.source.clone())
        .collect()
}

/// A satisfiable system's verdict: a region whose witness holds, or an
/// honest shrug. `Proved` is the one answer that is a bug.
fn assert_never_proved(system: &ConstraintSystem, verdict: Result<FeasibleRegion, Infeasibility>) {
    match verdict {
        Ok(region) => assert!(
            holds(system, region.witness()),
            "the witness {:?} does not hold",
            region.witness()
        ),
        Err(Infeasibility::NotFound { .. }) => {}
        Err(because) => panic!("a satisfiable system was reported {because:?}"),
    }
}

/// What a run of the local solve spent, from the log: the budget it
/// announced, and per start the evaluations *we* traced — the callback's
/// own count, not the solver's report of itself — so a solver that kept
/// asking past its budget would show here.
fn cobyla_spent(text: &str) -> (u64, Vec<u64>) {
    let budget = text
        .lines()
        .find(|line| line.contains("cobyla starts"))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("budget="))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or_else(|| panic!("no run was announced:\n{text}"));
    let spent = text
        .split("cobyla starts")
        .skip(1)
        .map(|run| {
            run.lines()
                .filter(|line| line.contains("cobyla evaluation"))
                .count() as u64
        })
        .collect();
    (budget, spent)
}

/// Every start of the local solve in `log` stayed within its budget.
fn assert_within_budget(log: &Captured) {
    let text = log.text();
    let (budget, spent) = cobyla_spent(&text);
    assert!(!spent.is_empty(), "no start was traced:\n{text}");
    for (start, evaluations) in spent.iter().enumerate() {
        assert!(
            *evaluations <= budget,
            "start {start} was asked {evaluations} times against a budget of {budget}"
        );
    }
}

// ---------------------------------------------------------------------------
// Contradictions the enclosure cannot see
// ---------------------------------------------------------------------------

/// `x % 2` is at most `0.5` and at least `1.5`: each alone admits a quarter
/// of the line, together they admit nothing, and no enclosure will ever say
/// so. The image of a remainder over an interval is a magnitude bound —
/// `[0, 2]` for any non-negative dividend, whatever its width — so the
/// forward pass sees `[0, 0.5]` and `[1.5, 2]` as two consistent facts about
/// two *different* instructions, and nothing inverts `%` to push either back
/// onto `x`. Bisection changes nothing: the image is the same on every
/// sub-box. So the contraction says nothing, the bisection spends its budget
/// and brute force spends its own, and the verdict must be *not found* with
/// both constraints named as the ones nothing could be concluded from.
#[test]
fn a_remainder_contradiction_is_reported_not_found() -> anyhow::Result<()> {
    let sources = &["x % 2 < 0.5", "x % 2 > 1.5"];
    let system = system(&[("x", 0.0, 10.0)], sources)?;
    let unexpressed = assert_not_found(solver().solve(&system, &mut rng()));
    assert_eq!(
        unexpressed, sources,
        "both remainders are opaque to the enclosure"
    );
    Ok(())
}

/// Parity, spelled two incompatible ways: `floor(x) % 2` is both zero and
/// one. The same blindness as above with a `floor` under the remainder,
/// which is the shape a discrete constraint takes when written in a
/// continuous language — and the shape a modeller writes twice by mistake.
#[test]
fn a_parity_contradiction_is_reported_not_found() -> anyhow::Result<()> {
    let sources = &["floor(x) % 2 == 0 +/- 0.1", "floor(x) % 2 == 1 +/- 0.1"];
    let system = system(&[("x", 0.0, 10.0)], sources)?;
    let unexpressed = assert_not_found(solver().solve(&system, &mut rng()));
    assert_eq!(unexpressed, sources);
    Ok(())
}

/// `floor(x) + floor(y) > x + y` is false everywhere — a floor never exceeds
/// its argument — and the enclosure encloses a little of it at every scale.
/// On a box that contains no integer the contradiction is visible: `floor`
/// is a constant there and the residual's interval is strictly positive.
/// On a box that straddles an integer, `floor(x)` is `[k - 1, k]` and `x` is
/// beside `k`, and the hull of the difference reaches below zero. Every
/// bisection that splits at an integer leaves a child that straddles one,
/// so the tree is infinite and the budget ends it — the honest shape of a
/// proof that is not there, and the log says so in as many words.
///
/// Unlike the remainder cases the constraint *did* narrow something —
/// `floor` inverts — so nothing is unexpressed: the shrug has no constraint
/// to name.
#[test]
fn a_floor_below_its_argument_spends_the_budget_and_is_not_found() -> anyhow::Result<()> {
    let system = system(
        &[("x", 0.0, 10.0), ("y", 0.0, 10.0)],
        &["floor(x) + floor(y) > x + y"],
    )?;
    let log = Captured::default();
    let verdict =
        tracing::subscriber::with_default(log.subscriber(), || solver().solve(&system, &mut rng()));
    let unexpressed = assert_not_found(verdict);
    assert!(
        unexpressed.is_empty(),
        "the contraction narrowed through `floor`, yet {unexpressed:?} was called unexpressed"
    );
    let text = log.text();
    assert!(
        text.contains("bisection cut short"),
        "the bisection should have run out of budget on an infinite tree:\n{text}"
    );
    Ok(())
}

/// The product of two signs is both positive and negative. Away from the
/// axes a sub-box has a definite sign on each coordinate and the enclosure
/// empties it at once; *on* the axes `sgn([0, ε])` is `[-1, 1]`, the product
/// straddles both targets, and the bisection descends toward the cross
/// without end. So the verdict is a shrug at every budget — none at all,
/// the default, sixteen times it — and never a point. Pinned at every
/// budget because a proof here would take an inverse for `sgn`, which
/// `interval` declines on purpose; the day it stops declining, this is the
/// test that says so.
#[test]
fn a_sign_contradiction_is_not_found_at_any_budget() -> anyhow::Result<()> {
    let sources = &["sgn(x) * sgn(y) > 0.5", "sgn(x) * sgn(y) < -0.5"];
    let system = system(&[("x", -5.0, 5.0), ("y", -5.0, 5.0)], sources)?;
    for prune_budget in [0, 16, 256, 4096] {
        let verdict = solver()
            .with_prune_budget(prune_budget)
            .solve(&system, &mut rng());
        assert_not_found(verdict);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Satisfiable systems that must never be proved empty
// ---------------------------------------------------------------------------

/// A region of measure zero that sampling can still hit: `floor(1000 x)` and
/// `ceil(1000 x)` are both even only where `1000 x` is an integer, and an
/// integer in `f64` is a set of points, not an interval. No enclosure can
/// prove it empty — the forward image through `%` is `[0, 2]` on every box —
/// and nothing may claim to. The honest answers are a witness that holds or
/// *not found*; the box is offset so its centre is not one, since a local
/// solve judges the centre first. Handed a witness, the region must accept
/// it and everything after must hold: a repair from a hair off the point
/// has one place to land, and a design of one point is a degenerate design
/// and says so rather than searching for a second.
#[test]
fn a_region_of_measure_zero_is_never_proved_empty() -> anyhow::Result<()> {
    let system = system(
        &[("x", 0.0001, 10.0002)],
        &[
            "floor(x * 1000) % 2 == 0 +/- 0.1",
            "ceil(x * 1000) % 2 == 0 +/- 0.1",
        ],
    )?;
    assert!(holds(&system, &[0.5]), "the fixture's own witness fails");
    assert!(
        !holds(&system, &[5.00015]),
        "the box centre must not be one"
    );

    assert_never_proved(&system, solver().solve(&system, &mut rng()));

    let region = solver()
        .with_known_feasible(vec![vec![0.5]])
        .solve(&system, &mut rng())
        .context("a hint that holds is a witness")?;
    assert!(holds(&system, region.witness()));
    for proposal in [0.5004, 0.4996] {
        let started = Instant::now();
        let repaired = region.repair(&[proposal], 0.0);
        assert!(started.elapsed() < CEILING, "{:?}", started.elapsed());
        match repaired {
            Ok(landed) => assert!(holds(&system, &landed), "{landed:?}"),
            Err(RepairError::Cramped { nearest, .. }) => {
                assert!(holds(&system, &nearest), "{nearest:?}");
            }
            Err(RepairError::Stranded) => {
                panic!("the witness is a thousandth of the box from {proposal} and was not reached")
            }
        }
    }
    match region.sample(Mat::zeros(0, 0).as_ref(), 4, &mut rng()) {
        Err(SampleError::Degenerate { found, wanted }) => {
            assert_eq!((found.ncols(), wanted), (1, 4));
            assert_all_hold(&system, &columns(&found));
        }
        Ok(design) => panic!(
            "a region of one point yielded a design of {}",
            design.ncols()
        ),
    }
    Ok(())
}

/// Twenty-four coordinates, each in an even cell of its own offset box:
/// `2^-24` of the box, every residual piecewise constant, nothing for an
/// enclosure to narrow through `%`. The verdict without help is whatever
/// the proposal budget's luck allows — a million proposals in a debug
/// build find nothing, a billion in release find it — and never a proof.
/// With the witness handed in, the region is a comb of sixteen million
/// cells and the walker's chains cannot leave the one they start in — so
/// the design is the sampler's and the walker's together, and every point
/// must hold; and a repair from the all-odd point — every coordinate in the
/// wrong cell, every gradient absent — is the sampling box over
/// twenty-four coordinates, COBYLA's projection over as many flat rows, and
/// the reference chord behind them. All of it must return: the box doubles
/// and shrinks by count, the projection by evaluations.
#[test]
fn a_comb_of_twenty_four_dimensions_is_walked_and_repaired_from_a_witness() -> anyhow::Result<()> {
    const DIMENSIONS: usize = 24;
    let (system, _) = per_coordinate(DIMENSIONS, &["floor({n}) % 2 == 0 +/- 0.1"])?;

    assert_never_proved(&system, solver().solve(&system, &mut rng()));

    // The first even cell above each coordinate's lower bound, at its centre.
    let witness: Vec<f64> = (0..DIMENSIONS)
        .map(|i| {
            let first = offset(i).ceil();
            let even = if first % 2.0 == 0.0 {
                first
            } else {
                first + 1.0
            };
            even + 0.5
        })
        .collect();
    assert!(holds(&system, &witness), "the fixture's own witness fails");
    let region = solver()
        .with_known_feasible(vec![witness.clone()])
        .solve(&system, &mut rng())
        .context("a hint that holds is a witness")?;
    let design = columns(&region.sample(Mat::zeros(0, 0).as_ref(), 8, &mut rng())?);
    assert_eq!(design.len(), 8, "the design ended early");
    assert_all_hold(&system, &design);

    let all_odd: Vec<f64> = witness.iter().map(|w| w + 1.0).collect();
    assert!(!holds(&system, &all_odd));
    let started = Instant::now();
    let landed = region
        .repair(&all_odd, 0.0)
        .context("every neighbouring cell is feasible")?;
    assert!(started.elapsed() < CEILING, "{:?}", started.elapsed());
    assert!(holds(&system, &landed), "{landed:?}");
    Ok(())
}

/// Six staircases as inequalities — `floor(x_i)` above `2.5` and below
/// `3.5` — a region a millionth of the box. `floor` inverts, so the
/// contraction narrows each coordinate to `[2.5, 4.5]` before a proposal is
/// spent, and inside that the region is a sixty-fourth: the probe lands it.
/// What this pins is that the enclosure narrows through a jump where it
/// can, and that what it hands the sampler is judged by the oracle and not
/// by the enclosure — every delivered point is in `[3, 4)` on every
/// coordinate, which the contracted box alone does not say.
#[test]
fn a_staircase_is_narrowed_by_the_enclosure_and_landed_by_the_probe() -> anyhow::Result<()> {
    let (system, _) = per_coordinate(6, &["floor({n}) > 2.5", "floor({n}) < 3.5"])?;

    let region = solver()
        .solve(&system, &mut rng())
        .context("the contraction leaves a sixty-fourth of the box")?;
    let design = columns(&region.sample(Mat::zeros(0, 0).as_ref(), 8, &mut rng())?);
    assert_eq!(design.len(), 8);
    assert_all_hold(&system, &design);
    for point in &design {
        assert!(
            point.iter().all(|x| (3.0..4.0).contains(x)),
            "{point:?} is off the step"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// COBYLA on its own
// ---------------------------------------------------------------------------

/// The same six staircases with only the local solve to find them. From the
/// box centre and every seeded start, every residual is constant across
/// COBYLA's simplex except where a vertex happens to cross a step, so its
/// linear models are zero or one-sided and a method that steps by its model
/// has nowhere useful to step. It concedes: every start converges short of
/// the region within its count — the callback is asked no more times than
/// the announced budget — and the verdict names every constraint, since
/// without a contractor nothing was put to anything that could reason
/// about it. (Two coordinates it finds, by a vertex landing on the step;
/// six it does not, and the count is what ends it either way.)
#[test]
fn the_local_solve_concedes_a_staircase_within_its_count() -> anyhow::Result<()> {
    let (system, sources) = per_coordinate(6, &["floor({n}) > 2.5", "floor({n}) < 3.5"])?;

    let log = Captured::default();
    let started = Instant::now();
    let verdict = tracing::subscriber::with_default(log.subscriber(), || {
        cobyla_alone().solve(&system, &mut rng())
    });
    assert!(started.elapsed() < CEILING, "{:?}", started.elapsed());
    let unexpressed = assert_not_found(verdict);
    assert_eq!(unexpressed, sources);
    assert_within_budget(&log);
    Ok(())
}

/// Residuals that are flat *and* wrong everywhere: a parity of one and a
/// half, a sign above two. Nothing the solver evaluates is ever feasible
/// and nothing it evaluates ever changes, so the models are exactly zero
/// from the first simplex on. It must converge on the spot and concede
/// within its count, on both shapes.
#[test]
fn the_local_solve_concedes_a_flat_impossibility_within_its_count() -> anyhow::Result<()> {
    for shape in ["floor({n}) % 2 == 1.5 +/- 0.1", "sgn({n}) > 2"] {
        let (system, sources) = per_coordinate(6, &[shape])?;
        let log = Captured::default();
        let started = Instant::now();
        let verdict = tracing::subscriber::with_default(log.subscriber(), || {
            cobyla_alone().solve(&system, &mut rng())
        });
        assert!(
            started.elapsed() < CEILING,
            "{shape}: {:?}",
            started.elapsed()
        );
        let unexpressed = assert_not_found(verdict);
        assert_eq!(unexpressed, sources, "{shape}");
        assert_within_budget(&log);
    }
    Ok(())
}

/// Every evaluation faults: `ln` of a negative is not a number, and the box
/// holds only negatives. To COBYLA that is a cost of `INFINITY` and a row of
/// `INFINITY`, at the centre and at every vertex of its simplex, so its
/// models are built from differences of infinities. It must still stop on
/// its count. The half-fault case beside it is the one a modeller actually
/// writes — a log whose argument is a remainder that dips below zero on
/// half the line — and there the fault is a wall the solver should step
/// away from, into the sixth of the line that is feasible; whether it does
/// is its business, but a proof that it cannot is not.
#[test]
fn the_local_solve_returns_when_evaluations_fault() -> anyhow::Result<()> {
    let everywhere = system(
        &[("x", -10.0, -1.0), ("y", -10.0, -1.0)],
        &["ln(x) + ln(y) > 0"],
    )?;
    let log = Captured::default();
    let started = Instant::now();
    let verdict = tracing::subscriber::with_default(log.subscriber(), || {
        cobyla_alone().solve(&everywhere, &mut rng())
    });
    assert!(started.elapsed() < CEILING, "{:?}", started.elapsed());
    assert_not_found(verdict);
    assert_within_budget(&log);

    let half = system(&[("x", 0.0, 12.0)], &["ln(x % 3 - 1.5) > 0"])?;
    assert!(holds(&half, &[2.8]), "the fixture's own witness fails");
    let log = Captured::default();
    let started = Instant::now();
    let verdict = tracing::subscriber::with_default(log.subscriber(), || {
        cobyla_alone().solve(&half, &mut rng())
    });
    assert!(started.elapsed() < CEILING, "{:?}", started.elapsed());
    assert_never_proved(&half, verdict);
    assert_within_budget(&log);
    Ok(())
}

/// Residuals at the edge of what `f64` holds: `x^50` on `[0, 10^6]` is
/// `10^300` at the far corner and `10^285` at the centre, finite and
/// enormous, and one product of two such in COBYLA's model algebra
/// overflows to infinity, one difference of those to `NaN`. The feasible
/// region is the unit square at the origin, a trillionth of the box.
/// Whatever the models become the run must end on its count, and a `NaN`
/// parameter must come out as a rejected evaluation rather than a
/// delivered point.
#[test]
fn the_local_solve_returns_from_residuals_near_overflow() -> anyhow::Result<()> {
    let system = system(&[("x", 0.0, 1e6), ("y", 0.0, 1e6)], &["x^50 + y^50 < 1"])?;
    assert!(holds(&system, &[0.5, 0.5]));

    let log = Captured::default();
    let started = Instant::now();
    let verdict = tracing::subscriber::with_default(log.subscriber(), || {
        cobyla_alone().solve(&system, &mut rng())
    });
    assert!(started.elapsed() < CEILING, "{:?}", started.elapsed());
    assert_never_proved(&system, verdict);
    assert_within_budget(&log);
    Ok(())
}

// ---------------------------------------------------------------------------
// Faults inside the search and at the proposal
// ---------------------------------------------------------------------------

/// A constraint that faults at every point of the box is one no point
/// satisfies, and the enclosure sees it before a proposal is spent: `ln`
/// clips its argument to the positive line and empties, a remainder by a
/// constant zero has an image of one point below its target. Both are
/// proofs and both name the constraint. What matters is that a fault
/// everywhere is a verdict and not a search that samples a million faults
/// and shrugs — and not a panic in the tape.
#[test]
fn a_constraint_that_faults_everywhere_is_proved_empty() -> anyhow::Result<()> {
    for (variables, source) in [
        (&[("x", -10.0, -1.0)][..], "ln(x) > 0"),
        (&[("x", 0.0, 10.0)][..], "x % 0 > 1"),
    ] {
        let system = system(variables, &[source])?;
        let verdict = solver().solve(&system, &mut rng());
        let Err(Infeasibility::Proved { blamed }) = verdict else {
            panic!("{source}: a fault everywhere is a proof, yet it was reported {verdict:?}");
        };
        let named: Vec<&str> = blamed.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(named, vec![source]);
    }
    Ok(())
}

/// `var[floor(n)]` with `n` ranging past both ends of the schema: a
/// subscript of zero or four reads a column that is not there, which is a
/// runtime fault and so a rejected proposal, never a panic. The region is
/// the part of the box where the subscript is a real column *and* that
/// column clears five — fat enough for the probe — and everything
/// delivered must have `n` inside the schema. A repair from either faulting
/// end must land in it.
#[test]
fn a_subscript_off_the_schema_is_a_rejection_not_a_panic() -> anyhow::Result<()> {
    let system = system(
        &[
            ("x1", 0.0, 10.0),
            ("x2", 0.0, 10.0),
            ("x3", 0.0, 10.0),
            ("n", 0.0, 5.0),
        ],
        &["var[floor(n)] > 5"],
    )?;
    let region = solver()
        .solve(&system, &mut rng())
        .context("a third of the box is feasible")?;
    let design = columns(&region.sample(Mat::zeros(0, 0).as_ref(), 16, &mut rng())?);
    assert_eq!(design.len(), 16);
    assert_all_hold(&system, &design);
    for point in &design {
        assert!(
            (1.0..4.0).contains(&point[3]),
            "{point:?} reads a column the schema does not have"
        );
    }

    for faulting in [[1.0, 1.0, 1.0, 0.5], [1.0, 1.0, 1.0, 4.5]] {
        assert!(!holds(&system, &faulting));
        let landed = region
            .repair(&faulting, 0.0)
            .with_context(|| format!("repairing {faulting:?}"))?;
        assert!(holds(&system, &landed), "{faulting:?} landed at {landed:?}");
    }
    Ok(())
}

/// A proposal at which the residual is not a number: `x % y` with `y` at
/// zero. Every stage of repair evaluates the point it was handed first —
/// the clamp reads slices at it, the projection starts from it — and each
/// must treat the fault as "not feasible here" and go on, not as an
/// answer. The region is a quarter of the box, so a landing is owed.
#[test]
fn a_proposal_that_faults_is_repaired_not_returned() -> anyhow::Result<()> {
    let system = system(&[("x", 0.0, 10.0), ("y", -1.0, 1.0)], &["x % y > 0.5"])?;
    let region = solver()
        .solve(&system, &mut rng())
        .context("a quarter of the box is feasible")?;
    for proposal in [[5.0, 0.0], [0.0, 0.0], [10.0, 0.0]] {
        assert!(!holds(&system, &proposal));
        let landed = region
            .repair(&proposal, 0.0)
            .with_context(|| format!("repairing {proposal:?}"))?;
        assert!(holds(&system, &landed), "{proposal:?} landed at {landed:?}");
        assert!(in_box(&system, &landed), "{landed:?}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shapes that must simply work
// ---------------------------------------------------------------------------

/// Every jump the language can spell, nested: a floor over a remainder over
/// a floor, a sign of a floor times a ceiling of a remainder. Each is a fat
/// region — a fifth to a half of its box — so the probe lands it and the
/// pressure is on what comes after: a design walked across cells the chain
/// cannot cross, and a repair from the centre and every corner of the box
/// with no gradient anywhere. Every point delivered holds; every repair
/// lands, since the nearest cell is never more than a box away.
#[test]
fn nested_jumps_are_sampled_walked_and_repaired() -> anyhow::Result<()> {
    type Shape<'a> = (&'a [(&'a str, f64, f64)], &'a [&'a str]);
    let shapes: &[Shape<'_>] = &[
        (
            &[("x", 0.0, 10.0), ("y", 0.0, 10.0)],
            &["floor(x / (floor(y) % 3 + 1)) == 2 +/- 0.1"],
        ),
        (
            &[("x", 0.0, 10.0), ("y", 0.0, 10.0)],
            &["sgn(floor(x) - 5) * ceil(y % 2) == -1 +/- 0.1"],
        ),
        (
            &[("x", -5.0, 5.0), ("y", -5.0, 5.0)],
            &["abs(x) % 2 - abs(y) % 2 > 0.5", "sgn(x) + sgn(y) < 0.5"],
        ),
        (
            &[("x", 0.0, 100.0), ("y", 0.0, 100.0), ("z", 0.0, 100.0)],
            &[
                "(floor(x) + floor(y) + floor(z)) % 3 == 0 +/- 0.1",
                "ceil(x / 10) > ceil(y / 10)",
                "floor(z) % 5 < 2.5",
            ],
        ),
    ];

    for (variables, sources) in shapes {
        let system = system(variables, sources)?;
        let region = solver()
            .solve(&system, &mut rng())
            .with_context(|| format!("{sources:?} is a fat region"))?;
        let design = columns(&region.sample(Mat::zeros(0, 0).as_ref(), 16, &mut rng())?);
        assert_eq!(design.len(), 16, "{sources:?}: the design ended early");
        assert_all_hold(&system, &design);

        let centre: Vec<f64> = variables
            .iter()
            .map(|(_, low, high)| (low + high) / 2.0)
            .collect();
        let mut proposals = vec![centre];
        for corner in 0..(1 << variables.len()) {
            proposals.push(
                variables
                    .iter()
                    .enumerate()
                    .map(
                        |(i, (_, low, high))| {
                            if corner & (1 << i) == 0 { *low } else { *high }
                        },
                    )
                    .collect(),
            );
        }
        for proposal in proposals {
            let started = Instant::now();
            let landed = region
                .repair(&proposal, 0.0)
                .with_context(|| format!("{sources:?} from {proposal:?}"))?;
            assert!(
                started.elapsed() < CEILING,
                "{sources:?} from {proposal:?} took {:?}",
                started.elapsed()
            );
            assert!(
                holds(&system, &landed) && in_box(&system, &landed),
                "{sources:?} from {proposal:?} landed at {landed:?}"
            );
        }
    }
    Ok(())
}

/// A checkerboard of tenth-wide cells, a quarter of the unit square. The
/// walker cannot cross between cells, and a design that only ever reported
/// the seed's cell would be one cell dressed as sixteen points; the pool a
/// design is chosen from includes a round of uniform proposals precisely so
/// that a region in pieces is covered in proportion to its pieces, and
/// sixteen points farthest-first from each other must land in several of
/// the twenty-five. Repair from the centre of an odd cell: with a
/// clearance a cell can hold, a landing whose axis neighbours all hold; with
/// one no cell can hold — `0.06` each way of a `0.1` cell — `Cramped`,
/// carrying a nearest point that holds, and never `Stranded`.
#[test]
fn a_checkerboard_is_covered_by_a_design_and_repaired_to_its_cells() -> anyhow::Result<()> {
    let system = system(
        &[("x", 0.0, 1.0), ("y", 0.0, 1.0)],
        &[
            "floor(x * 10) % 2 == 0 +/- 0.1",
            "floor(y * 10) % 2 == 0 +/- 0.1",
        ],
    )?;
    let region = solver().solve(&system, &mut rng())?;
    let design = columns(&region.sample(Mat::zeros(0, 0).as_ref(), 16, &mut rng())?);
    assert_eq!(design.len(), 16);
    assert_all_hold(&system, &design);
    #[expect(clippy::cast_possible_truncation, reason = "a cell index is small")]
    let cell = |value: f64| (value * 10.0).floor() as i64;
    let mut cells: Vec<(i64, i64)> = design
        .iter()
        .map(|point| (cell(point[0]), cell(point[1])))
        .collect();
    cells.sort_unstable();
    cells.dedup();
    assert!(
        cells.len() >= 6,
        "sixteen points of a twenty-five-cell board sit in {} cells: {cells:?}",
        cells.len()
    );

    let odd = [0.15, 0.15];
    assert!(!holds(&system, &odd));
    const FITS: f64 = 0.02;
    let landed = region
        .repair(&odd, FITS)
        .context("a cell holds a clearance of a fifth of itself")?;
    assert!(holds(&system, &landed), "{landed:?}");
    for coordinate in 0..2 {
        for sign in [-1.0, 1.0] {
            let mut neighbour = landed.clone();
            neighbour[coordinate] += sign * FITS;
            assert!(
                holds(&system, &neighbour),
                "{landed:?} lacks the clearance: {neighbour:?} is outside"
            );
        }
    }
    match region.repair(&odd, 0.06) {
        Err(RepairError::Cramped { nearest, clearance }) => {
            assert_eq!(clearance, 0.06);
            assert!(holds(&system, &nearest), "{nearest:?}");
        }
        Ok(landed) => panic!("no cell is wide enough for a clearance of 0.06, yet {landed:?}"),
        Err(RepairError::Stranded) => panic!("a quarter of the box is feasible"),
    }
    Ok(())
}

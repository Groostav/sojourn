//! The equality taxonomy, measured rather than assumed.
//!
//! Babel admits exactly one equality form — `a == b +/- t`, with `t` a literal —
//! and that one syntax covers at least six structurally different problems. The
//! taxonomy is written up in `docs/todo.md` under *Equality constraints*;
//! these are the cases that decide whether it is real.
//!
//! **These are expected to be red, and the red is the deliverable.** The pool
//! today treats every equality identically: one residual, no idea that a
//! variable is pinned, driven, or one of two branches. The classifier that would
//! tell those apart is being designed against what these measure, so a case that
//! fails here is a specification for it, not a regression.
//!
//! # Why these and not the existing pool cases
//!
//! [`cvg_pools`](../cvg_pools.rs) already carries one case per taxonomy row and
//! every one of them is green. That is not evidence the shapes are handled — the
//! tolerances there are loose enough (`1e-3` to `1e-5`) that the band has volume
//! a walker can move in, so the pipeline never has to know what kind of equality
//! it is looking at. Tighten the tolerance and the distinction starts to matter.
//! Each case here is a pool case with the slack taken out, which is the cheapest
//! honest way to find the floor.
//!
//! **Rows A and B are green as of the classifier** ([`cvg::classify`]) — a
//! variable alone on one side of an equality is computed from the others rather
//! than searched for, which is what lets a walker move along a measure-zero
//! surface instead of jittering beside it. Row E is green through the matching
//! in `classify::plans`, and a product that must vanish through a plan per
//! arm. Rows C and D classify as `Opaque`.
//!
//! Case F — implicit, `sin(x) == x/2` — is deliberately absent. No use case has
//! turned up for `x == f(x)`, and Newton is a lot of machinery to carry for a
//! shape nobody writes. The classifier should recognise it as a self-dependency
//! and refuse it; see `docs/todo.md`.
//!
//! # What is asserted
//!
//! Three claims, and the third is the one that bites. Delivering the requested
//! count and having every point feasible are what the pool fixtures already
//! check. **Coverage** is new: a solver hands back one witness, and a pool that
//! reports that witness five hundred times has satisfied both of the other two
//! claims while exploring nothing. So each case names the coordinates that are
//! genuinely free and the fraction of their range the sample must span.

mod common;

use faer::Mat;
use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{ConstraintSolver, ConstraintSystem, InputVariable, SampleError, SystemError};

/// Pinned so a failure is reproducible, and the same value the other cvg suites
/// use so a point seen in one is the point seen in another.
const SEED: u64 = 0x50_50_1E_5E_ED;

/// A sample matrix back as one `Vec<f64>` per point.
fn columns(samples: &Mat<f64>) -> Vec<Vec<f64>> {
    (0..samples.ncols())
        .map(|column| {
            (0..samples.nrows())
                .map(|row| samples[(row, column)])
                .collect()
        })
        .collect()
}

/// One taxonomy case.
struct Case<'a> {
    /// Taxonomy row and a word on the shape, for the failure message.
    what: &'a str,
    variables: &'a [(&'a str, f64, f64)],
    sources: &'a [&'a str],
    wanted: usize,
    /// Coordinates that are genuinely free under these constraints, each with
    /// the fraction of its declared range the sample is expected to span.
    ///
    /// Stated per case rather than inferred, because inferring it is the
    /// classifier's job and this is the test that classifier gets built against.
    coverage: &'a [(&'a str, f64)],
    /// How much of the feasible set the sample must actually *occupy*, as
    /// opposed to merely reach the ends of. `None` where the case does not make
    /// the claim.
    occupancy: Option<Occupancy<'a>>,
}

/// A grid over some of the free coordinates, and how many of its cells the
/// sample has to land in.
///
/// # Why extent is not enough
///
/// [`Case::coverage`] measures `max - min`, and a handful of points at the
/// extremes satisfies it completely. That is not hypothetical: the pool spends
/// up to a fixed budget of solver calls looking for parts of the box it has not
/// reached, and those seeds land at the *edges* of what is uncovered, by
/// construction. Sixteen well-placed points pass a span assertion on any number
/// of coordinates while occupying almost nothing between them.
///
/// Occupancy cannot be met that way. `n` seeds fill at most `n` cells however
/// cleverly they are placed, so a threshold above the seed budget can only be
/// reached by a search that **moves along the feasible set** — which is what
/// rows C and E are actually about, and what a span assertion could not tell
/// apart from a lucky scattering.
struct Occupancy<'a> {
    /// The coordinates the grid is drawn over.
    over: &'a [&'a str],
    /// Divisions per coordinate, so the grid holds `divisions^over.len()` cells.
    divisions: usize,
    /// How many of those cells must receive at least one point.
    least: usize,
}

/// Asks for `wanted` points and reports, in one message, everything that was
/// wrong with what came back.
///
/// Reporting all three failures together rather than tripping on the first is
/// deliberate: these cases are expected to fail, and "delivered 1 of 500" and
/// "covered 0% of x2" are different diagnoses that a fail-fast assertion would
/// hide behind each other.
fn assert_explores(case: Case<'_>) {
    let inputs: Vec<InputVariable> = case
        .variables
        .iter()
        .map(|(name, low, high)| InputVariable::new_continuous(*name, *low, *high))
        .collect();
    let system = ConstraintSystem::new(inputs.clone(), case.sources.iter().copied())
        .expect("a fixture's constraints should bind to its own box");

    let pool = ConstraintSolver::new()
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))
        .unwrap_or_else(|e| panic!("{}: solving failed: {e}", case.what));

    let design = match pool.sample(
        Mat::zeros(0, 0).as_ref(),
        case.wanted,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    ) {
        Ok(design) => design,
        // Both are a short design carrying what there was; the count check
        // below is what reads them.
        Err(SampleError::Degenerate { found, .. } | SampleError::Exhausted { found, .. }) => found,
    };
    let points = columns(&design);
    let mut complaints: Vec<String> = Vec::new();

    // 1. The count. A design short of what was asked means the region ran
    //    out of distinct points, which is a real outcome — and for a fully
    //    determined system it is the *only* outcome, which is one of the
    //    things these cases are here to show.
    if points.len() != case.wanted {
        complaints.push(format!(
            "delivered {} of {} requested (degenerate)",
            points.len(),
            case.wanted,
        ));
    }

    // 2. Feasibility, re-checked independently of the pool: it filters its own
    //    output, and a test that trusts the thing it is testing is not a test.
    let names: Vec<&str> = inputs.iter().map(|v| v.name.as_str()).collect();
    let mut infeasible = 0usize;
    let mut worst = f64::NEG_INFINITY;
    for point in &points {
        let bindings: Vec<(&str, f64)> = names.iter().copied().zip(point.iter().copied()).collect();
        for expression in case.sources {
            let residual = common::eval_one(expression, &bindings)
                .unwrap_or_else(|e| panic!("{}: evaluating {:?}: {e}", case.what, expression));
            // The pool fixtures' tolerance: a solver-produced point can sit a
            // hair outside where a sampled one never does. A non-finite residual
            // is counted too — it is not a satisfied constraint, and writing this
            // as a bare `>` would silently let NaN through.
            if !residual.is_finite() || residual > 1e-10 {
                infeasible += 1;
                worst = worst.max(residual);
            }
        }
    }
    if infeasible > 0 {
        complaints.push(format!(
            "{infeasible} constraint violations across {} points, worst residual {worst:e}",
            points.len()
        ));
    }

    // 3. Coverage. The claim a solver alone cannot satisfy: one witness repeated
    //    is feasible and countable and explores nothing.
    for (name, wanted_fraction) in case.coverage {
        let index = names
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("{}: no variable named {name}", case.what));
        let (_, low, high) = case.variables[index];

        let values: Vec<f64> = points.iter().map(|point| point[index]).collect();
        let span = match (
            values.iter().copied().reduce(f64::min),
            values.iter().copied().reduce(f64::max),
        ) {
            (Some(min), Some(max)) => (max - min) / (high - low),
            _ => 0.0,
        };
        let distinct = {
            let mut sorted = values.clone();
            sorted.sort_by(f64::total_cmp);
            sorted.dedup();
            sorted.len()
        };

        if span < *wanted_fraction {
            complaints.push(format!(
                "{name} spans {:.4}% of its range, wanted {:.1}% \
                 ({distinct} distinct values in {} points)",
                span * 100.0,
                wanted_fraction * 100.0,
                points.len()
            ));
        }
    }

    // 4. Occupancy. Extent is satisfiable by a few points at the edges of the
    //    box; this is not, and the gap between them is the difference between
    //    having *found* the feasible set and being able to move along it.
    if let Some(grid) = &case.occupancy {
        let Occupancy {
            over,
            divisions,
            least,
        } = *grid;

        let axes: Vec<usize> = over
            .iter()
            .map(|name| {
                names
                    .iter()
                    .position(|candidate| candidate == name)
                    .unwrap_or_else(|| panic!("{}: no variable named {name}", case.what))
            })
            .collect();

        let occupied: std::collections::BTreeSet<Vec<usize>> = points
            .iter()
            .filter_map(|point| {
                axes.iter()
                    .map(|axis| {
                        let (_, low, high) = case.variables[*axis];
                        let scaled = (point[*axis] - low) / (high - low);
                        if !(0.0..=1.0).contains(&scaled) {
                            return None;
                        }
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "scaled is guarded to 0.0..=1.0 on the line above"
                        )]
                        let index = (scaled * divisions as f64) as usize;
                        Some(index.min(divisions - 1))
                    })
                    .collect()
            })
            .collect();

        let cells =
            divisions.pow(u32::try_from(over.len()).expect("a grid is over a handful of axes"));
        if occupied.len() < least {
            complaints.push(format!(
                "occupies {} of {cells} cells over {over:?}, wanted {least}.{}",
                occupied.len(),
                " A search that only finds the feasible set fills about as many cells as it has seeds; one that moves along it fills far more",
            ));
        }
    }

    assert!(
        complaints.is_empty(),
        "{}:\n  - {}",
        case.what,
        complaints.join("\n  - ")
    );
}

// ---------------------------------------------------------------------------
// A — pinned to a constant
// ---------------------------------------------------------------------------

/// One dimension pinned, one free. The pinned dimension should stop being
/// searched at all; the free one should still be explored.
///
/// `cvg_pools::constants` is the loose version of this and passes, because at
/// `+/- 0.001` the band is a thousandth of the box and the walker can move
/// inside it. At `1e-9` there is nothing to move in, so `x1` has to come from
/// somewhere other than sampling — and `x2`, which no constraint mentions, has
/// to keep being explored anyway. A pipeline that solves the whole point at once
/// gets `x1` right and `x2` frozen.
#[test]
fn a_pinned_variable_does_not_freeze_the_free_one() {
    assert_explores(Case {
        what: "A (pinned): x1 == pi at 1e-9, x2 unconstrained",
        variables: &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)],
        sources: &["x1 == pi +/- 0.000000001"],
        wanted: 200,
        occupancy: None,
        coverage: &[("x2", 0.8)],
    });
}

/// Every dimension pinned, which is as close to *fully determined* as babel can
/// currently express — and the gap between those two is the point.
///
/// This was written expecting a short `take`: the notes describe a fully
/// determined system as one where the feasible set is a single point and "give
/// me two hundred distinct points" has no answer. **It passes**, and the reason
/// is that a tolerance is not optional in this language. `+/- 1e-9` leaves a
/// `2e-9` square, which holds an enormous number of representable doubles, so
/// two hundred distinct feasible points genuinely exist and the pool finds them.
///
/// The determined case only appears once the tolerance is *dropped* for the
/// solver, which is the surface-then-band work and cannot be written here yet.
/// So this stands as the guard on the near-miss repair instead: at `1e-9` the
/// band is thin enough that a solver witness rounding a hair outside is a real
/// possibility, and this is what would catch a regression in `cvg::repaired`.
#[test]
fn a_pinned_system_is_still_satisfiable_because_the_tolerance_is_not_optional() {
    assert_explores(Case {
        what: "A (pinned, both dimensions): the closest thing to fully determined",
        variables: &[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)],
        sources: &["x1 == pi +/- 0.000000001", "x2 == e +/- 0.000000001"],
        wanted: 200,
        occupancy: None,
        coverage: &[],
    });
}

// ---------------------------------------------------------------------------
// B — driven
// ---------------------------------------------------------------------------

/// `y` is determined by `x`: choose `x`, evaluate, done. No search in `y` at
/// all, and `x` free across its whole range.
///
/// `cvg_pools::a_constraint_nothing_can_reason_about_still_yields_points_and_says_so`
/// is the same constraint at `1e-6` over a box containing the origin, and it
/// passes for a reason its own doc comment admits was luck: the solver of the
/// day saw only the bounds, returned a point near the origin, and
/// `sin(0) = 0` put that point on the curve by accident.
///
/// This moves the box to `2..3`, where the origin is not available and the luck
/// runs out, and tightens the tolerance by three orders. Driving `y` from `x`
/// makes it trivial; not driving it makes it impossible.
#[test]
fn a_driven_variable_is_evaluated_not_searched() {
    assert_explores(Case {
        what: "B (driven): y == sin(x) at 1e-9, box away from the origin",
        variables: &[("x", 2.0, 3.0), ("y", -1.0, 1.0)],
        sources: &["y == sin(x) +/- 0.000000001"],
        wanted: 200,
        occupancy: None,
        coverage: &[("x", 0.8)],
    });
}

/// Driven through a function with an inverse, so the seed is not the thing
/// standing in the way — the missing classification is.
///
/// `cvg_pools::roots` is this pair at `1e-4`. A driven coordinate is put on
/// its surface when a box's centre is judged, so the seed is immediate; the
/// question is whether anything then explores the curve rather than sitting
/// on the one point.
#[test]
fn a_driven_variable_a_solver_can_reach_is_still_explored() {
    assert_explores(Case {
        what: "B (driven, translatable): x1 == sqrt(x2), x3 == cbrt(x4) at 1e-9",
        variables: &[
            ("x1", 0.0, 10.0),
            ("x2", 0.0, 10.0),
            ("x3", 0.0, 10.0),
            ("x4", 0.0, 10.0),
        ],
        sources: &[
            "x1 == sqrt(x2) +/- 0.000000001",
            "x3 == cbrt(x4) +/- 0.000000001",
        ],
        wanted: 200,
        occupancy: None,
        coverage: &[("x2", 0.5), ("x4", 0.5)],
    });
}

// ---------------------------------------------------------------------------
// C and F — a variable named on both sides, and refused
// ---------------------------------------------------------------------------

/// `x == f(x)` is rejected when the system is built, rather than searched for.
///
/// Rows C and F turned out to be one thing. A variable on both sides of an
/// equality makes it *implicit* in that variable — no reading of it yields
/// `v = ...` — and nothing downstream can drive what it cannot isolate. Row C
/// used to be treated as recoverable, on the grounds that
/// `x2 == x1 + x2/2 - x3/x4` is one rearrangement from driven and the solver
/// of the day answered it anyway. Both true; the rearrangement was never
/// written, so in practice it
/// fell to whatever the sampler managed and read as a capability that did not
/// exist.
///
/// **The refusal is of a phrasing, not a problem.** Every constraint here can be
/// written with the variable on one side, and the diagnostic says so rather than
/// only complaining. `cvg_pools::simple_arithmetic` is the same fixture written
/// the other way round and still passes, which is the evidence that nothing was
/// lost but the implicitness.
///
/// It is also not a claim of emptiness. `sin(x) == x/2` has three solutions and
/// `x == x*x + 2` is an ordinary quadratic, so `SolveError::Unsatisfiable`
/// would be false. `SystemError` is the shape for "we will not try".
#[test]
fn an_implicit_equality_is_refused_by_name() {
    for (source, variable) in [
        ("x == sin(x) +/- 0.001", "x"),
        ("x == x*x + 2 +/- 0.001", "x"),
        ("x2 == x1 + 1/2*x2 - x3 / x4 +/- 0.000000001", "x2"),
    ] {
        let error = ConstraintSystem::new(
            vec![
                InputVariable::new_continuous("x", -2.0, 2.0),
                InputVariable::new_continuous("x1", 0.0, 10.0),
                InputVariable::new_continuous("x2", 0.0, 10.0),
                InputVariable::new_continuous("x3", 0.0, 10.0),
                InputVariable::new_continuous("x4", 1.0, 10.0),
            ],
            [source],
        )
        .expect_err("a variable on both sides should be refused");

        let SystemError::Implicit {
            variable: named, ..
        } = &error
        else {
            panic!("expected an implicit-equality refusal for {source}, got {error:?}");
        };
        assert_eq!(named, variable, "{source}");

        let message = error.to_string();
        for expected in [variable, "implicit", "one side"] {
            assert!(
                message.contains(expected),
                "the message should mention {expected:?}: {message}"
            );
        }
    }
}

/// The rearrangement the diagnostic points at is accepted, and explored.
///
/// Without this the refusal above could be hiding a real loss of capability
/// rather than asking for a different spelling of the same set.
#[test]
fn the_rearranged_form_is_accepted_and_explored() {
    assert_explores(Case {
        what: "C, rearranged: 1/2*x2 - x1 + x3/x4 == 0 at 1e-9",
        variables: &[
            ("x1", 0.0, 10.0),
            ("x2", 0.0, 10.0),
            ("x3", 0.0, 10.0),
            ("x4", 1.0, 10.0),
        ],
        sources: &["1/2*x2 - x1 + x3 / x4 == 0 +/- 0.000000001"],
        wanted: 200,
        coverage: &[("x1", 0.3), ("x3", 0.3)],
        occupancy: None,
    });
}

// ---------------------------------------------------------------------------
// A compound side, isolated
// ---------------------------------------------------------------------------

/// `x1 + x2 == 3` is easier than `y == sin(x)`, and used to be the one that
/// failed.
///
/// Driving needs a variable it can *compute*, and the classifier only saw one
/// when a whole side was a bare variable — a statement about spelling, not about
/// the problem. One subtraction isolates `x1` here, after which the walker
/// sweeps `x2` across its range and computes `x1 = 3 - x2`, which is a uniform
/// sample along the segment rather than a cluster at a seed.
///
/// Measured at `b5` of 60 bins before isolation: the feasible set is a 2e-9-wide
/// line, so essentially every chord misses it and the chains sit where they
/// started. Forty of eighty bins is far above the pool's seed budget, so it can
/// only be met by moving along the line.
#[test]
fn a_compound_equality_is_traversed() {
    assert_explores(Case {
        what: "compound side: x1 + x2 == 3 at 1e-9",
        variables: &[("x1", 0.0, 3.0), ("x2", 0.0, 3.0)],
        sources: &["x1 + x2 == 3 +/- 0.000000001"],
        wanted: 500,
        coverage: &[("x2", 0.8)],
        occupancy: Some(Occupancy {
            over: &["x2"],
            divisions: 80,
            least: 40,
        }),
    });
}

/// Two equalities sharing a variable, which is where the drive has to be
/// *declined* as well as taken.
///
/// `x2` is named by both, and only one of them can define it. `classify::plan`
/// refuses an ambiguous definition rather than resolving it by source order —
/// choosing between them is row E's matching problem — so at most one drive
/// survives here and the other equality stays an ordinary constraint the walker
/// still has to satisfy.
///
/// That makes this the case that would catch a plan quietly taking one and
/// dropping the other's constraint: the points would satisfy one equality and
/// not both, and the feasibility re-check would report it.
///
/// Read `b3 / b4 / b4` of 60 before isolation — the worst of everything
/// measured.
#[test]
fn two_coupled_equalities_are_traversed() {
    assert_explores(Case {
        what: "coupled: x1 + x2 == 3 and x2 + x3 == 2 at 1e-9",
        variables: &[("x1", 0.0, 3.0), ("x2", 0.0, 3.0), ("x3", 0.0, 3.0)],
        sources: &[
            "x1 + x2 == 3 +/- 0.000000001",
            "x2 + x3 == 2 +/- 0.000000001",
        ],
        wanted: 500,
        coverage: &[("x2", 0.5)],
        occupancy: Some(Occupancy {
            over: &["x2"],
            divisions: 80,
            least: 25,
        }),
    });
}

/// Driving assumes the feasible set is a **graph** over the free coordinates,
/// and this is the case where it is not.
///
/// `x1 * x2 == 0` is a cross: the arm `x1 = 0` and the arm `x2 = 0`, of equal
/// measure. Isolation drives `x1` as `0 / x2`, which parametrises the first arm
/// and nothing else — at `x2 = 0` the variable `x1` is unconstrained, so no
/// function `x1 = f(x2)` describes the set. A chain that starts on the second
/// arm can only move `x2`, and moving `x2` off zero leaves the set, so it sits
/// there.
///
/// **Every point is feasible; the sample is still wrong.** What breaks is
/// coverage, and it is the same defect as row D's branches wearing different
/// clothes.
///
/// # Why this is worth a red test rather than a note
///
/// It is not a regression. Before isolation this constraint was `Opaque`, the
/// walker drew chords through both coordinates, missed a measure-zero set every
/// time, and sat on its seed — visibly broken at about five occupied bins.
///
/// Now one arm is sampled well. **Isolation turns a visibly stuck sample into a
/// confidently wrong one**, which is the more dangerous of the two, and an
/// occupancy claim over `x2` alone would pass it without complaint because `x2`
/// does span its whole range. Only looking at the arms together shows it.
///
/// Measured at 394 of 400 points on one arm and 7 on the other, against the
/// even split the geometry says. Green since branch selection by plan: the
/// matching admits a plan per arm, a point takes the plan that pins it
/// hardest — the arm it stands on — and at the crossing, which is the only
/// point the opening finds here, the walker spreads its chains across both.
/// The other choice, refusing to drive where a divisor can vanish, would have
/// given up cases that are perfectly fine.
#[test]
fn both_arms_of_a_product_receive_points() {
    /// The share of the sample each arm must hold. They are of equal measure,
    /// so a fair sample is even; a fifth is a long way below that and still
    /// nowhere near what a single parametrised branch delivers.
    const LEAST: f64 = 0.2;
    /// Membership: on the `x1 = 0` arm the band admits `|x1| <= 5e-10`, so this
    /// is generous by three orders and not a threshold anything turns on.
    const ON_THE_ARM: f64 = 1e-6;

    let inputs = vec![
        InputVariable::new_continuous("x1", -2.0, 2.0),
        InputVariable::new_continuous("x2", -2.0, 2.0),
    ];
    let system = ConstraintSystem::new(inputs, ["x1 * x2 == 0 +/- 0.000000001"])
        .expect("binds to its own box");

    let solution = ConstraintSolver::new()
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))
        .expect("a cross through the origin is satisfiable");
    let samples = solution;

    let points = columns(
        &samples
            .sample(
                Mat::zeros(0, 0).as_ref(),
                400,
                &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
            )
            .expect("a cross has four hundred distinct points"),
    );
    let on_arm = |axis: usize| {
        points
            .iter()
            .filter(|point| point[axis].abs() < ON_THE_ARM)
            .count()
    };
    let (first, second) = (on_arm(0), on_arm(1));
    let wanted = (points.len() as f64 * LEAST) as usize;

    assert!(
        first >= wanted && second >= wanted,
        "x1 * x2 == 0 is a cross of two equal arms, and the sample holds {first} \
        points on x1 = 0 against {second} on x2 = 0, of {} - each arm wants at least \
        {wanted}. Driving parametrises one branch and cannot reach the other",
        points.len()
    );
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// D — multi-valued
// ---------------------------------------------------------------------------

/// Two roots, and both have to receive points.
///
/// `cvg_pools::absolute_value` asks only for feasibility, which one branch
/// satisfies completely. The failure mode a branch-blind pipeline has is
/// invisible to that assertion and obvious to this one: a solver picks a branch,
/// the walker seeds from it, and shrinkage keeps it there forever because the
/// components are disjoint and a chain cannot cross the gap.
///
/// Coverage is the wrong instrument for a two-point set, so this states it
/// directly: `x1` must span at least the distance between the branches.
#[test]
fn both_branches_of_an_absolute_value_receive_points() {
    assert_explores(Case {
        what: "D (multi-valued): abs(x1) == 1 at 1e-9, branches at -1 and +1",
        variables: &[("x1", -2.0, 2.0)],
        sources: &["abs(x1) == 1 +/- 0.000000001"],
        // The region is two points: `x1` is driven onto its band, so nothing
        // is free to move and a design holds exactly one point per branch.
        // Asking for more is `Degenerate` with these two, by design.
        wanted: 2,
        // The branches sit at -1 and +1 in a range of 4. Reaching both spans
        // 0.5 of it and reaching one spans ~0, so anything above ~0.1 is the
        // answer — the threshold is not 0.5 because that would additionally
        // require the extreme point of each band, which is not the claim.
        occupancy: None,
        coverage: &[("x1", 0.4)],
    });
}

/// The same claim on the shape the benchmarks already measure distribution for,
/// so a fix can be checked against `cvg_benchmarks::parabolic_roots_*` for
/// uniformity rather than merely for reach.
#[test]
fn both_bands_of_a_parabola_receive_points() {
    assert_explores(Case {
        what: "D (multi-valued): (x + 2) * (x - 1) == 0 at 1e-9, bands at -2 and 1",
        variables: &[("x", -5.0, 5.0)],
        sources: &["(x + 2) * (x - 1) == 0 +/- 0.000000001"],
        wanted: 200,
        // Bands at -2 and 1 in a range of 10, so reaching both spans 0.3 and
        // reaching one spans ~0. Below that, for the reason above.
        occupancy: None,
        coverage: &[("x", 0.25)],
    });
}

// ---------------------------------------------------------------------------
// E — under-determined
// ---------------------------------------------------------------------------

/// Two equations, three variables, one degree of freedom left — and *which* two
/// variables get driven is a choice, which is where matching earns its keep.
///
/// `cvg_pools::dynamic_variable_lookup` is this at `1e-3`. At `1e-9` the
/// feasible set is a line segment in a three-dimensional box, so points exist in
/// quantity but only along one direction, and finding that direction is the
/// whole problem.
#[test]
fn an_under_determined_system_explores_its_remaining_freedom() {
    assert_explores(Case {
        what: "E (under-determined): two equations, three variables, at 1e-9",
        variables: &[("x1", -1.0, 1.0), ("x2", -2.0, 2.0), ("x3", -2.0, 2.0)],
        sources: &[
            "1.5 == var[1] + var[2] +/- 0.000000001",
            "1.5 == var[2] - var[3] +/- 0.000000001",
        ],
        wanted: 200,
        // One degree of freedom, so the segment runs diagonally and every
        // coordinate moves along it. `x1` is the narrowest and bounds the rest.
        occupancy: None,
        coverage: &[("x1", 0.5)],
    });
}

/// The same system, asked to fill its one degree of freedom rather than reach
/// the ends of it.
///
/// Two equations over three variables leave a line segment, and
/// `an_under_determined_system_explores_its_remaining_freedom` is satisfied by
/// two points at its ends. Matching would pick two variables to drive and leave
/// the third free, after which the segment is traversed by moving that one
/// coordinate — and the histogram over `x2` fills.
///
/// # Where the numbers come from
///
/// They are bounded by the geometry and not chosen for difficulty.
/// `x2 = 1.5 - x1` with `x1` in `-1..1` puts `x2` in `0.5..2.5`, and
/// `x3 = x2 - 1.5` with `x3` in `-2..2` puts it in `-0.5..3.5`; together
/// **`x2` can only reach `0.5..2`**, which is 37.5% of its declared `-2..2`. So
/// a span assertion above `0.375` is unsatisfiable however good the search is,
/// and `0.3` is the honest ask.
///
/// That also sets the grid. Over `-2..2` the reachable stretch is 30 of 80 bins,
/// so `least: 24` is four fifths of what exists — and, being well above the
/// pool's seed budget, is not reachable by scattering seeds. Forty bins would
/// have left only fifteen reachable, *below* the budget, and would have proved
/// nothing.
///
/// `x2` rather than `x1` because `x2` appears in both equations and is the
/// coordinate a wrong matching is most likely to freeze.
#[test]
fn an_under_determined_system_fills_its_remaining_freedom() {
    assert_explores(Case {
        what: "E (under-determined, strict): two equations, three variables, at 1e-9",
        variables: &[("x1", -1.0, 1.0), ("x2", -2.0, 2.0), ("x3", -2.0, 2.0)],
        sources: &[
            "1.5 == var[1] + var[2] +/- 0.000000001",
            "1.5 == var[2] - var[3] +/- 0.000000001",
        ],
        wanted: 500,
        coverage: &[("x2", 0.3)],
        occupancy: Some(Occupancy {
            over: &["x2"],
            divisions: 80,
            least: 24,
        }),
    });
}

// ---------------------------------------------------------------------------
// The floor
// ---------------------------------------------------------------------------

/// Where the current pipeline stops handling an equality, measured rather than
/// guessed.
///
/// Every case above picks `1e-9` because it is obviously past the floor. This
/// walks a shape down from a tolerance that works until it stops, and reports
/// the last one that did. The number in the assertion is a *ratchet*: it records
/// what the pipeline could do when this was written, so tightening it later is a
/// deliberate edit and loosening it is a regression that has to be argued for.
///
/// `x1 == x2 +/- t` is the simplest possible equality — one linear equation,
/// two variables, one degree of freedom, no branches and nothing to invert. If
/// anything holds at a tight tolerance it is this, so it is the fairest place to
/// put the floor.
#[test]
fn the_tolerance_floor_is_where_it_was_left() {
    /// The tightest tolerance `x1 == x2 +/- t` is currently expected to survive.
    ///
    /// Was `1e-4` before the classifier: rejection sampling and a chord drawn
    /// through both coordinates, so the band had to have width for anything to
    /// land in it. With `x1` driven from `x2` the tolerance stops mattering
    /// almost entirely — this passes at `1e-17` too, and the ladder's floor of
    /// `1e-12` is the loop bound rather than a limit anything was found at.
    const EXPECTED_FLOOR: f64 = 1e-12;

    let mut last_good: Option<f64> = None;
    for exponent in 1..=12 {
        let tolerance = 10f64.powi(-exponent);
        // Written out rather than in scientific notation: the `+/-` position
        // takes a `literal`, and `1e-1` is not one — the lexer stops at the `e`.
        let source = format!("x1 == x2 +/- {tolerance:.*}", exponent as usize);
        let inputs = vec![
            InputVariable::new_continuous("x1", 0.0, 10.0),
            InputVariable::new_continuous("x2", 0.0, 10.0),
        ];
        let system = ConstraintSystem::new(inputs.clone(), [source.as_str()])
            .expect("the fixture should bind to its own box");

        // An `Err` here is the region being empty or unfound at this
        // tolerance, which is the case being searched for; anything else a
        // solve can fail with is a bug and would be, at any tolerance.
        let Ok(pool) =
            ConstraintSolver::new().solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))
        else {
            break;
        };
        let design = match pool.sample(
            Mat::zeros(0, 0).as_ref(),
            100,
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        ) {
            Ok(design) => design,
            Err(SampleError::Degenerate { found, .. } | SampleError::Exhausted { found, .. }) => {
                found
            }
        };
        let points = columns(&design);

        let feasible = points.iter().all(|point| {
            let bindings = [("x1", point[0]), ("x2", point[1])];
            common::eval_one(&source, &bindings).is_ok_and(|residual| residual <= 1e-10)
        });
        let spread = points
            .iter()
            .map(|point| point[0])
            .fold(f64::NEG_INFINITY, f64::max)
            - points
                .iter()
                .map(|point| point[0])
                .fold(f64::INFINITY, f64::min);

        if points.len() == 100 && feasible && spread > 1.0 {
            last_good = Some(tolerance);
        } else {
            break;
        }
    }

    let floor = last_good.unwrap_or(f64::INFINITY);
    assert!(
        floor <= EXPECTED_FLOOR,
        "the tightest workable tolerance on `x1 == x2` is {floor:e}, \
         and the floor this test ratchets is {EXPECTED_FLOOR:e}. \
         If that is an improvement, tighten EXPECTED_FLOOR to match."
    );
}

/// **Neither variable can be isolated, and it is traversed anyway.**
///
/// `sqrt(x1) + sqrt(x2) == 3` has no bare side, and peeling either variable out
/// would mean inverting `sqrt` — which the old `isolate` refused, because
/// building a symbolic inverse means choosing a branch. So this classified as
/// `Opaque`, nothing drove, and the walker jittered: **three occupied cells of
/// forty**.
///
/// What changed is that `reaches` answers *whether* rather than *what*, and
/// `interval::invert_unary` does the narrowing — intersecting both branches
/// with the argument's own range rather than picking one. So the two tables were
/// brought into step and this became drivable, with nothing to choose.
#[test]
fn an_equality_under_a_function_is_driven_through_its_inverse() {
    assert_explores(Case {
        what: "driven through an inverse: sqrt(x1) + sqrt(x2) == 3 at 1e-9",
        variables: &[("x1", 0.0, 9.0), ("x2", 0.0, 9.0)],
        sources: &["sqrt(x1) + sqrt(x2) == 3 +/- 0.000000001"],
        wanted: 400,
        coverage: &[("x1", 0.5)],
        occupancy: Some(Occupancy {
            over: &["x1"],
            divisions: 40,
            least: 24,
        }),
    });
}

/// **The case bipartite matching exists for**, green since it was built.
///
/// Both equations name `x1` and each could drive it. `plan` used to refuse
/// to choose — picking by which constraint was written first is not a
/// reading — and drove neither, leaving three variables under two equations
/// entirely free: an axis move then has to hold a point on a line by moving
/// one coordinate, which it cannot, and the walk occupied **twelve cells of
/// forty**. Now `plan` matches equations to the variables each can isolate,
/// so one equation drives `x1` and the other its remaining variable, and
/// the one degree of freedom left is walked along.
///
/// Interval propagation would not have rescued it, for the reason a Gibbs
/// sweep cannot traverse a chain of tight equalities: the conditional slice
/// of one coordinate on a measure-zero set is a point.
#[test]
fn two_equations_wanting_the_same_variable_strand_each_other() {
    assert_explores(Case {
        what: "matching: x1 + x2 == 3 and x1 + x3 == 2, both wanting x1",
        variables: &[("x1", 0.0, 3.0), ("x2", 0.0, 3.0), ("x3", 0.0, 3.0)],
        sources: &[
            "x1 + x2 == 3 +/- 0.000000001",
            "x1 + x3 == 2 +/- 0.000000001",
        ],
        wanted: 400,
        coverage: &[("x1", 0.5)],
        occupancy: Some(Occupancy {
            over: &["x1"],
            divisions: 40,
            least: 24,
        }),
    });
}

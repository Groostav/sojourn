//! Constraint-solving pool cases, ported from `Z3SolvingPoolFixture.kt`.
//!
//! The JVM harness asserts a *property*, not a value: ask for ten points, get
//! ten, and every one satisfies every constraint. That is strategy-agnostic, so
//! the same cases run against whatever the pool is currently made of and
//! red-versus-green tracks *capability* rather than feature-completeness.
//!
//! Which is why these split by constraint shape:
//!
//! * **Inequalities are samplable.** `20 > 2^x5` admits about 43% of its range,
//!   so rejection sampling finds points immediately.
//! * **Equality-with-tolerance is not.** `x1 == sqrt(x2) +/- 0.0001` is a
//!   measure-zero ribbon that uniform sampling will essentially never land on.
//!   Those were red until a seeder existed — that was the honest picture, not
//!   a gap in the port.
//!
//! All sixteen cases from the fixture are here. Several of them exercise babel
//! features — `ln`, `%`, `sgn`, `var[i]` — that nothing else puts through a
//! pool, which is worth more than the solver coverage they were written for.
//!
//! Deliberately not ported: `Z3Fixture` and `Z3ExtensionsFixture` test the Z3
//! API, which this crate no longer links; `LanguageFixture` is half JVM sanity
//! checks and half decimal-to-rational conversion for an SMT emitter this
//! crate no longer has; `IntegrationTests` asserts a list equals an integer
//! and calls `.all()` without a terminal assertion, so it either always fails
//! or asserts nothing.

mod common;

use faer::Mat;

use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{ConstraintSolver, ConstraintSystem, Infeasibility, InputVariable, SampleError};

/// A fixture's [`ConstraintSystem`]; one that does not bind is the test's error.
fn system(variables: Vec<InputVariable>, constraints: &[&str]) -> anyhow::Result<ConstraintSystem> {
    Ok(ConstraintSystem::new(
        variables,
        constraints.iter().copied(),
    )?)
}

fn variables(specs: &[(&str, f64, f64)]) -> Vec<InputVariable> {
    specs
        .iter()
        .map(|(name, low, high)| InputVariable::new(*name, *low, *high))
        .collect()
}

/// A sample matrix back as one `Vec<f64>` per point.
///
/// The pool speaks in matrices because that is what the evaluator eats, but
/// almost every assertion here is about *a point* — its coordinates, its
/// residual, its position in a distribution. Converting once at the boundary
/// keeps those assertions saying what they mean instead of indexing `(row,
/// column)` pairs.
fn columns(samples: &Mat<f64>) -> Vec<Vec<f64>> {
    (0..samples.ncols())
        .map(|column| {
            (0..samples.nrows())
                .map(|row| samples[(row, column)])
                .collect()
        })
        .collect()
}

/// Pinned so a failure is reproducible. The JVM version faked this with
/// `OneHundredBraindeadPoints`, a hard-coded array of 100 doubles.
const SEED: u64 = 0x50_50_1E_5E_ED;

const REQUESTED: usize = 10;

/// The oracle the corpus tests share: ten points came back, every one inside
/// the box, every one satisfying every source — re-checked independently of
/// the pool, which filters its own output, and a test that trusts the thing
/// it is testing is not a test.
fn assert_ten_feasible(system: &ConstraintSystem, sources: &[&str], points: &[Vec<f64>]) {
    assert_eq!(
        points.len(),
        REQUESTED,
        "wanted {REQUESTED} points, got {}",
        points.len()
    );
    for point in points {
        for (variable, value) in system.variables().iter().zip(point) {
            assert!(
                variable.contains(*value),
                "{} = {value} is outside {}..={}",
                variable.name,
                variable.lower_bound,
                variable.upper_bound
            );
        }
        let bindings: Vec<(&str, f64)> = system
            .variables()
            .iter()
            .map(|v| v.name.as_str())
            .zip(point.iter().copied())
            .collect();
        for source in sources {
            let residual = common::eval_one(source, &bindings)
                .unwrap_or_else(|e| panic!("evaluating {source:?} at {point:?}: {e}"));
            // Matching the JVM harness's tolerance: a solver-produced point can
            // sit a hair outside, where a sampled one never does.
            assert!(
                residual <= 1e-10,
                "{point:?} fails {source:?} (residual {residual})"
            );
        }
    }
}

/// `Unknown` is a claim about what we *know*, not about what we can deliver.
///
/// This case was written expecting the system to come up empty: the band is one
/// part in a million of the box, far too thin to sample. It does not come
/// up empty, and the reason is worth keeping.
///
/// `y` is driven by `sin(x)`, so a box's centre is put on the curve when it
/// is judged and the declared box settles at once — where the solver of the
/// day, refusing `sin`, saw only the bounds and landed near the origin, on
/// the curve by the accident of `sin(0) = 0`. Hit-and-run seeds from the
/// point and walks **along** the curve, since shrinkage converges onto the
/// feasible piece containing the current point however thin that piece is.
///
/// So the pool delivers real points for a constraint nothing in the pipeline can
/// reason about, and `Satisfied` is the honest answer: points exist and are in
/// hand. There used to be a third verdict for this — `Unknown` — reporting the
/// epistemic state, and it is gone because the state it described is not one a
/// caller can act on. What replaces it is the `tracing` line the emitter now
/// writes naming the constraint and why it could not be expressed, so the
/// information is still reported rather than dropped, which is the whole
/// difference from the JVM version.
///
/// The known weakness is *coverage*, not correctness: the points cluster around
/// wherever the solver's arbitrary model landed, and no fairness oracle applies,
/// because the region has no closed form and rejection sampling cannot reach it
/// to serve as a reference.
#[test]
fn a_constraint_nothing_can_reason_about_still_yields_points_and_says_so() -> anyhow::Result<()> {
    let source = "y == sin(x) +/- 0.000001";
    let inputs = vec![
        InputVariable::new("x", -1.0, 1.0),
        InputVariable::new("y", -1.0, 1.0),
    ];

    let pool = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(
            &system(inputs.clone(), &[source])?,
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        )
        .expect("solving should not fail");

    // The points are real. Checked here rather than trusted, because the pool
    // filtering its own output is the thing under test: the emitter never put
    // this constraint to a solver, so nothing but the filter stands between a
    // proposed point and the caller.
    let points = columns(&pool.sample(
        Mat::zeros(0, 0).as_ref(),
        5,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);
    assert_eq!(points.len(), 5);
    for point in &points {
        let bindings = [("x", point[0]), ("y", point[1])];
        let residual = common::eval_one(source, &bindings).expect("evaluation should not fail");
        assert!(residual <= 0.0, "{point:?} does not satisfy {source:?}");
    }
    Ok(())
}

// ------------------------------------------------------------ the design

/// A region that is a single point has one point to give, and says so.
///
/// A box of no width is a point: every draw and every walk stays put, so a
/// design of five is one column and a `Degenerate` carrying it. The point
/// is real either way — the error is about count, not feasibility.
#[test]
fn a_region_that_is_one_point_designs_one_point_and_says_so() -> anyhow::Result<()> {
    let system = system(vec![InputVariable::new("x1", 3.0, 3.0)], &["x1 > 0"])?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;

    let verdict = region.sample(
        Mat::zeros(0, 0).as_ref(),
        5,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    );

    let Err(SampleError::Degenerate { found, wanted }) = verdict else {
        panic!("a point-sized region should be degenerate, got {verdict:?}");
    };
    assert_eq!(wanted, 5);
    assert_eq!(found.ncols(), 1, "{found:?}");
    assert!(
        (found[(0, 0)] - 3.0).abs() <= 1e-12,
        "the one point is the pinned one: {found:?}"
    );
    Ok(())
}

/// The design spreads away from what the caller already has.
///
/// A disc; the caller hands in its centre. Every point of the design is
/// feasible, none is the centre, and the nearest pair among the centre and
/// the design is well apart: eight points in a unit disc can all be more
/// than a third of a radius from each other and from the centre, and a
/// design that was merely a sample would put some pair much closer.
#[test]
fn a_design_spreads_away_from_what_the_caller_already_has() -> anyhow::Result<()> {
    let sources = &["x^2 + y^2 < 1"];
    let system = system(variables(&[("x", -1.0, 1.0), ("y", -1.0, 1.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let centre = Mat::zeros(2, 1);

    let design = region.sample(
        centre.as_ref(),
        8,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;

    assert_eq!(design.ncols(), 8);
    let mut all = columns(&design);
    for point in &all {
        assert!(system.is_feasible(point, 0.0), "{point:?}");
    }
    all.push(vec![0.0, 0.0]);
    let mut nearest = f64::INFINITY;
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            let apart = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
            nearest = nearest.min(apart);
        }
    }
    assert!(
        nearest > 0.33,
        "the nearest pair is only {nearest} apart: {all:?}"
    );
    Ok(())
}

/// A design of fewer points than variables is still a design: every point
/// feasible, every pair distinct. Twenty variables, five points — the shape
/// an optimizer's opening design usually has, and the one a Latin hypercube
/// would have nothing to say about.
#[test]
fn a_design_smaller_than_the_dimension_is_still_a_design() -> anyhow::Result<()> {
    let names: Vec<String> = (1..=20).map(|i| format!("x{i}")).collect();
    let inputs: Vec<InputVariable> = names
        .iter()
        .map(|name| InputVariable::new(name.clone(), -1.0, 1.0))
        .collect();
    let sources = &["x1 + x2 + x3 + x4 + x5 < 1", "x6 * x7 > -0.5"];
    let system = system(inputs, sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;

    let design = region.sample(
        Mat::zeros(0, 0).as_ref(),
        5,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;

    assert_eq!(design.ncols(), 5);
    let points = columns(&design);
    for point in &points {
        assert!(system.is_feasible(point, 0.0), "{point:?}");
    }
    for (i, a) in points.iter().enumerate() {
        for b in &points[i + 1..] {
            assert_ne!(a, b, "two points of the design coincide");
        }
    }
    Ok(())
}

/// A region in two pieces gets a point in each: farthest-first reaches the
/// far root, where a walk from the witness never would.
#[test]
fn the_pieces_of_a_region_each_receive_a_point() -> anyhow::Result<()> {
    let sources = &["abs(x) == 1 +/- 0.001"];
    let system = system(variables(&[("x", -5.0, 5.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;

    let design = region.sample(
        Mat::zeros(0, 0).as_ref(),
        4,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;

    let points = columns(&design);
    assert_eq!(points.len(), 4);
    assert!(
        points.iter().any(|point| point[0] > 0.0),
        "no point at the positive root: {points:?}"
    );
    assert!(
        points.iter().any(|point| point[0] < 0.0),
        "no point at the negative root: {points:?}"
    );
    Ok(())
}

/// The same seeds give the same design, run to run.
///
/// One generator state for the opening and one for the design, so the
/// matrix is a function of the system and the two states. If this ever
/// fails, every seeded expectation in the suite is resting on luck.
#[test]
fn the_same_generator_state_designs_the_same_points() -> anyhow::Result<()> {
    let mut runs = Vec::new();
    for _ in 0..2 {
        let pool = ConstraintSolver::new()
            .with_proposal_budget(common::PROPOSAL_BUDGET)
            .solve(
                &system(
                    vec![
                        InputVariable::new("x1", 0.0, 10.0),
                        InputVariable::new("x2", 0.0, 10.0),
                    ],
                    &["x1 < x2"],
                )?,
                &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
            )
            .expect("solving should not fail");
        runs.push(columns(&pool.sample(
            Mat::zeros(0, 0).as_ref(),
            500,
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        )?));
    }

    assert_eq!(runs[0].len(), 500);
    assert_eq!(
        runs[0], runs[1],
        "the same generator state produced different points"
    );
    Ok(())
}

/// One region, several designs: the walker's chains were burnt in at
/// `solve` and every design walks from them, so two calls from one generator
/// state give one matrix — a design is still a function of the region and
/// its arguments — and one generator carried through two calls gives two
/// different, feasible ones: the next batch.
#[test]
fn two_designs_from_one_region_share_their_chains() -> anyhow::Result<()> {
    let sources = &["x^2 + y^2 < 1"];
    let system = system(variables(&[("x", -1.0, 1.0), ("y", -1.0, 1.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;

    let mut rng = Xoshiro256PlusPlus::seed_from_u64(SEED);
    let first = region.sample(Mat::zeros(0, 0).as_ref(), 12, &mut rng)?;
    let next = region.sample(Mat::zeros(0, 0).as_ref(), 12, &mut rng)?;
    let again = region.sample(
        Mat::zeros(0, 0).as_ref(),
        12,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;

    assert_eq!(
        columns(&first),
        columns(&again),
        "one region, one generator state, one design"
    );
    assert_ne!(
        columns(&first),
        columns(&next),
        "the generator carried on is the next design"
    );
    for point in columns(&next) {
        assert!(system.is_feasible(&point, 0.0), "{point:?}");
    }
    Ok(())
}

// ------------------------------------------- only a solver can say this

#[test]
fn contradictory_constraints_are_reported_as_unsatisfiable() -> anyhow::Result<()> {
    // `x > 8` and `x < 2` cannot both hold. Sampling cannot tell that apart from
    // "I did not find one" — it looks identical from the outside — so this is
    // the one path in the whole crate that can produce `Unsatisfiable`, and it
    // exists only because a solver is wired up.
    let solution = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(
            &system(
                vec![InputVariable::new("x", 0.0, 10.0)],
                &["x > 8", "x < 2"],
            )?,
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        );

    let Err(because) = solution else {
        panic!("expected Unsatisfiable, got {solution:?}");
    };

    let Infeasibility::Proved { blamed } = because else {
        panic!("a plain contradiction should be proved, not merely unfound");
    };

    // Both, because a contradiction is a relationship: either constraint alone
    // is perfectly satisfiable, and naming one would be picking arbitrarily.
    let mut sources: Vec<&str> = blamed.iter().map(|c| c.source.as_str()).collect();
    sources.sort_unstable();
    assert_eq!(sources, vec!["x < 2", "x > 8"]);
    Ok(())
}

/// The mistake a user actually makes: three constraints that describe a
/// region, and a fourth with its comparison backwards. Each holds on its own
/// and the four together hold nothing, which sampling cannot distinguish from
/// bad luck and a local solve cannot distinguish from a bad start. What a
/// user needs back is the *set* that conflicts, so they know which line to
/// look at. Interval contraction blames exactly `x1 < 3`, `x2 > 5`,
/// `x1 > x2` — the trace of the coordinate it emptied — and the message reads
/// "no point satisfies these constraints together: `x1 < 3`, `x2 > 5`,
/// `x1 > x2`". This was the test the question "does Z3 earn its build" was
/// answered against, and the answer was that this does it without Z3; see
/// `docs/todo.md`.
#[test]
fn a_backwards_comparison_is_blamed_together_with_what_it_contradicts() -> anyhow::Result<()> {
    let system = system(
        vec![
            InputVariable::new("x1", 0.0, 10.0),
            InputVariable::new("x2", 0.0, 10.0),
        ],
        &[
            "x1 < 3",
            "x2 > 5",
            "x1 + x2 < 20",
            // Meant `x1 < x2`, and every one of the three above agrees with
            // that; this one contradicts the first two.
            "x1 > x2",
        ],
    )?;
    let verdict = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED));

    let Err(because) = verdict else {
        panic!("expected Unsatisfiable, got {verdict:?}");
    };
    let Infeasibility::Proved { blamed } = &because else {
        panic!("a linear contradiction should be proved, not merely unfound: {because}");
    };

    let sources: Vec<&str> = blamed.iter().map(|c| c.source.as_str()).collect();
    assert!(
        sources.contains(&"x1 > x2"),
        "the backwards comparison should be in the blame: {sources:?}"
    );
    assert!(
        !sources.contains(&"x1 + x2 < 20"),
        "a constraint that takes no part in the contradiction should not be blamed: {sources:?}"
    );
    // What the user reads.
    assert_eq!(
        because.to_string(),
        format!(
            "no point satisfies these constraints together: {}",
            sources
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    );
    Ok(())
}

/// The boundary of what interval reasoning proves, stated so it is a
/// documented limit rather than a discovered one. A *thin* contradiction —
/// two half-planes a billionth apart — is one every box encloses a little
/// of until bisection reaches that width, which is beyond any budget; the
/// honest verdict is `NotFound`, and it names nothing, because every
/// constraint narrowed *something* without it adding up to a proof.
#[test]
fn a_thin_contradiction_is_not_found_rather_than_proved() -> anyhow::Result<()> {
    let verdict = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(
            &system(
                vec![
                    InputVariable::new("x", -1.0, 1.0),
                    InputVariable::new("y", -1.0, 1.0),
                ],
                &["x + y <= 1", "x + y >= 1.000000001"],
            )?,
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        );

    let Err(Infeasibility::NotFound { unexpressed }) = verdict else {
        panic!("a contradiction too thin for an enclosure was reported {verdict:?}");
    };
    assert!(
        unexpressed.is_empty(),
        "both constraints narrow boxes; neither is beyond interval reasoning: {unexpressed:?}"
    );
    Ok(())
}

/// The other side of the boundary: an *algebraic* contradiction.
/// `x*x - 2*x*y + y*y` is `(x - y)^2` and never negative, but an enclosure
/// evaluates the three terms separately and holds negatives on every box
/// of any width — the square is never seen as a square. A decision
/// procedure over polynomials proves this; nothing here does, and the
/// verdict says so rather than claiming it.
#[test]
fn an_algebraic_contradiction_is_not_found_rather_than_proved() -> anyhow::Result<()> {
    let verdict = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(
            &system(
                vec![
                    InputVariable::new("x", -1.0, 1.0),
                    InputVariable::new("y", -1.0, 1.0),
                ],
                &["x*x - 2*x*y + y*y < 0"],
            )?,
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        );

    assert!(
        matches!(verdict, Err(Infeasibility::NotFound { .. })),
        "a contradiction no enclosure can see was reported {verdict:?}"
    );
    Ok(())
}

#[test]
fn a_satisfiable_problem_is_not_blamed_on_anything() -> anyhow::Result<()> {
    // The other half of the above: the machinery has to stay quiet when there is
    // nothing wrong, or an `Unsatisfiable` means nothing.
    let solution = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(
            &system(
                vec![InputVariable::new("x", 0.0, 10.0)],
                &["x > 8", "x < 9"],
            )?,
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        );
    assert!(solution.is_ok(), "got {solution:?}");
    Ok(())
}

// ------------------------------------------------- samplable: inequalities

#[test]
fn power_with_variable_as_exponent() -> anyhow::Result<()> {
    // 2^x5 < 20 means x5 < log2(20) ~ 4.32, so ~43% of the range.
    // The JVM comment reads "nope, Z3 wont reason about real-exponents" —
    // rejection sampling has no such trouble, and neither has an enclosure.
    let sources = &["20 > 2^x5"];
    let system = system(variables(&[("x5", 0.0, 10.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn a_deeply_transcendental_constraint() -> anyhow::Result<()> {
    // `x1 > sin(ln(cos(2.1^x1)))`. Feasible for x1 below about 0.61, where
    // cos(2.1^x1) is still positive. The JVM name for this was "should simply
    // drop provided expression" — it could not transcode it at all.
    let sources = &["x1 > sin(ln(cos(2.1^x1)))"];
    let system = system(variables(&[("x1", 0.0, 1.0), ("x2", 0.0, 1.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn sine_over_multiple_periods() -> anyhow::Result<()> {
    // Ported with its assertion *inverted*. The JVM version asserted the
    // infeasible results were `isNotEmpty()`, pinning the fact that its
    // Taylor-series `sin` produced points that did not satisfy the constraint.
    // A correct pool returns feasible points.
    let sources = &["y > sin(theta)"];
    let system = system(
        variables(&[
            ("theta", std::f64::consts::PI, std::f64::consts::PI * 3.0),
            ("y", -1.0, 1.0),
        ]),
        sources,
    )?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn a_simple_inequality() -> anyhow::Result<()> {
    // Ours, not the fixture's: the simplest possible two-variable constraint,
    // here so that a failure everywhere else has something trivial to be
    // contrasted against.
    let sources = &["x1 < x2"];
    let system = system(variables(&[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn logarithms() -> anyhow::Result<()> {
    // `2 < ln(x1)` is `x1 > e^2`, about 26% of the range. The JVM case has two
    // further constraints commented out — `x4 == log(4) +/- 0.0001` and
    // `x6 > log(2.0, x5)` — so those are left out here too rather than invented.
    // `x2` is declared and unused, exactly as over there: a schema may be wider
    // than the constraints that reference it.
    let sources = &["2 < ln(x1)"];
    let system = system(variables(&[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn modulo_with_a_symbolic_divisor() -> anyhow::Result<()> {
    // `10 % x1` where the divisor is the variable. Note `x1 = 0` gives NaN, and
    // a NaN residual is not a pass — so this also pins that the pool rejects
    // rather than propagates it.
    let sources = &["3 > 10 % x1"];
    let system = system(variables(&[("x1", 0.0, 10.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn equality_with_a_loose_tolerance() -> anyhow::Result<()> {
    // The tolerance is what decides whether an equality is samplable. At
    // `+/- 0.1` on a 2x2 square the band is about 9.75% of the area, so this
    // goes green while every other equality case in this file does not — the
    // difference is measure, not kind.
    let sources = &["x1 == x2 +/- 0.1"];
    let system = system(variables(&[("x1", -1.0, 1.0), ("x2", -1.0, 1.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[expect(
    clippy::approx_constant,
    reason = "the fixture's bounds are literally -3.14..3.14, a truncation rather \
         than an attempt at pi — and the difference shows at the endpoints, \
         where sin(3.14) is 0.0016 and sin(pi) is zero"
)]
#[test]
fn sine_below_zero() -> anyhow::Result<()> {
    // Half the range of x1. `y` is unused by the constraint.
    let sources = &["sin(x1) <= 0"];
    let system = system(variables(&[("x1", -3.14, 3.14), ("y", 0.9, 1.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

// ------------------------------ needs a solver: equality with tolerance

#[test]
fn simple_arithmetic() -> anyhow::Result<()> {
    // Was `x2 == x1 + 1/2*x2 - x3/x4`, which is the same set written
    // circularly and is now refused at construction — see
    // `SystemError::Cyclic`. Rearranged rather than dropped, so the
    // fixture still exercises what it was ported for.
    let sources = &["1/2*x2 - x1 + x3 / x4 == 0 +/- 0.00001"];
    let system = system(
        variables(&[
            ("x1", 0.0, 1.0),
            ("x2", 0.0, 1.0),
            ("x3", 0.0, 1.0),
            ("x4", 0.0, 1.0),
        ]),
        sources,
    )?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn roots() -> anyhow::Result<()> {
    let sources = &["x1 == sqrt(x2) +/- 0.0001", "x3 == cbrt(x4) +/- 0.0001"];
    let system = system(
        variables(&[
            ("x1", 0.0, 10.0),
            ("x2", 0.0, 10.0),
            ("x3", 0.0, 10.0),
            ("x4", 0.0, 10.0),
        ]),
        sources,
    )?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn power() -> anyhow::Result<()> {
    let sources = &["x1 == x2^3 +/- 0.0001"];
    let system = system(variables(&[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn absolute_value() -> anyhow::Result<()> {
    // Three variables, each pinned to a magnitude from a different side of zero:
    // x1 from the positive range, x2 from the negative, x3 from a range that
    // excludes the answer's sign entirely. Bands of a thousandth in ranges of
    // one, so about two parts in a billion once combined.
    let sources = &[
        "abs(x1) == 1 +/- 0.001",
        "abs(x2) == 1 +/- 0.001",
        "abs(x3) == 1.5 +/- 0.001",
    ];
    let system = system(
        variables(&[("x1", 0.0, 1.0), ("x2", -1.0, 0.0), ("x3", -2.0, -1.0)]),
        sources,
    )?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn modulo() -> anyhow::Result<()> {
    // Two constraints of different shapes, as the JVM case had them.
    // `x1 % 3.0 >= 2` alone is samplable — a third of the range — but
    // `x3 == x4 % 4.5 +/- 0.0001` is a curve of width 0.0002, and a test is only
    // as green as its hardest constraint. Kept together rather than split, so
    // that what goes green when the solver lands is the case as written.
    let sources = &["x1 % 3.0 >= 2", "x3 == x4 % 4.5 +/- 0.0001"];
    let system = system(
        variables(&[
            ("x1", 0.0, 10.0),
            ("x2", 0.0, 10.0),
            ("x3", 0.0, 10.0),
            ("x4", 0.0, 10.0),
        ]),
        sources,
    )?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn constants() -> anyhow::Result<()> {
    // Two bands 0.002 wide in a 10x10 box: about four parts in a hundred
    // million. Sampling is not going to stumble onto pi.
    let sources = &["x1 == pi +/- 0.001", "x2 == e +/- 0.001"];
    let system = system(variables(&[("x1", 0.0, 10.0), ("x2", 0.0, 10.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn signum() -> anyhow::Result<()> {
    // `sgn` is a step, so x2 has to land within 0.001 of exactly -1 or +1 — two
    // slivers of a range four wide. Also the only place `sgn` meets a pool, and
    // worth having for that alone: Java's `Math.signum` and Rust's `f64::signum`
    // disagree about zero and NaN.
    let sources = &["x2 == sgn(x1) +/- 0.001"];
    let system = system(variables(&[("x1", -1.0, 1.0), ("x2", -2.0, 2.0)]), sources)?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn dynamic_variable_lookup() -> anyhow::Result<()> {
    // The only exercise of `var[i]` under a pool anywhere in the suite. Red for
    // its measure rather than its subject — but it still proves the indexed form
    // compiles, binds against a schema, and evaluates through the pool, which is
    // the part that would otherwise go untested until a solver arrived.
    let sources = &[
        "1.5 == var[1] + var[2] +/- 0.001",
        "1.5 == var[2] - var[3] +/- 0.001",
    ];
    let system = system(
        variables(&[("x1", -1.0, 1.0), ("x2", -2.0, 2.0), ("x3", -2.0, 2.0)]),
        sources,
    )?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

#[test]
fn ceiling_and_floor() -> anyhow::Result<()> {
    let sources = &["x1 > floor(x2)", "x3 > ceil(x4) + floor(x4)"];
    let system = system(
        variables(&[
            ("x1", 0.0, 10.0),
            ("x2", 0.0, 10.0),
            ("x3", 0.0, 10.0),
            ("x4", 0.0, 10.0),
        ]),
        sources,
    )?;
    let region = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?;
    let points = columns(&region.sample(
        Mat::zeros(0, 0).as_ref(),
        REQUESTED,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?);

    assert_ten_feasible(&system, sources, &points);
    Ok(())
}

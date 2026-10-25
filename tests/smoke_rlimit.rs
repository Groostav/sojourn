//! The computation limit ends a spin: each test here runs against a build
//! with one loop broken on purpose, and expects the limit's panic, quickly.
//!
//! `just smoke-rlimit` rebuilds the crate once per stall, with
//! `--cfg sojourn_stall="<name>"`, and runs this file. Every stall defeats
//! its loop's own bound — a cap ignored, progress never recorded, an answer
//! never accepted — so the only thing left to end the call is
//! `rlimit::tick` (src/rlimit.rs). A stall the limit does not reach shows up
//! as the test running into nextest's `smoke-rlimit` timeout, which fails
//! rather than waits. In an ordinary build this file is empty.
//!
//! `newton_guesses_bounds` is not invented: it restores the active-set
//! guess Newton had before 1ad8b39, under which this file's repair of
//! eighty continuous variables and three discrete ones took 140 s with every
//! loop inside its bound. It is the case the limit was built for.

#![cfg(any(
    sojourn_stall = "newton_guesses_bounds",
    sojourn_stall = "newton_never_converges",
    sojourn_stall = "level_search_never_counts",
    sojourn_stall = "walker_never_settles",
    sojourn_stall = "random_direction_never_finite",
    sojourn_stall = "contract_never_settles",
    sojourn_stall = "brute_force_never_ends",
))]

use std::time::{Duration, Instant};

use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{ConstraintSolver, ConstraintSystem, FeasibleRegion, InputVariable};

/// How long a stalled call may run before the limit ends it. Seconds are
/// what the limit promises; a minute would be a hang by another name.
const SMOKE_CEILING: Duration = Duration::from_secs(30);

/// How a stalled call ended: its panic message, and how long it ran.
struct Ended {
    message: String,
    elapsed: Duration,
}

/// Runs `act`, which is expected to panic.
fn stalled(act: impl FnOnce() + std::panic::UnwindSafe) -> Ended {
    let started = Instant::now();
    let payload = std::panic::catch_unwind(act).expect_err("the stalled call should panic");
    let elapsed = started.elapsed();
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .expect("a panic message");
    Ended { message, elapsed }
}

/// The stall ended at `entry`'s computation limit, inside the ceiling.
fn assert_ended_by_limit(entry: &str, ended: &Ended) {
    let Ended { message, elapsed } = ended;
    assert!(
        message.contains(&format!("sojourn's {entry} spent its computation limit")),
        "the stall should end at {entry}'s limit, not in: {message}"
    );
    assert!(
        *elapsed < SMOKE_CEILING,
        "{entry}'s limit took {elapsed:?} to end the stall"
    );
    eprintln!("{entry}: the limit ended the stall in {elapsed:?}");
}

fn system(variables: Vec<InputVariable>, constraints: &[String]) -> ConstraintSystem {
    ConstraintSystem::new(variables, constraints.iter().map(String::as_str))
        .expect("the fixture binds")
}

fn region(system: &ConstraintSystem) -> FeasibleRegion {
    ConstraintSolver::new()
        .with_proposal_budget(1_000_000)
        .solve(system, &mut Xoshiro256PlusPlus::seed_from_u64(1))
        .expect("the fixture is satisfiable")
}

/// `x_i + 0.1·k + 0.001·x_j <= 1.2` over `n` continuous coordinates and
/// `discrete` lattices `0..=9`: the probe the 140 s repair was found on.
#[allow(dead_code, reason = "only the repair stalls use it")]
fn coupled(n: usize, discrete: usize) -> ConstraintSystem {
    let mut variables: Vec<InputVariable> = (1..=n)
        .map(|i| InputVariable::new_continuous(format!("x{i}"), 0.0, 1.0))
        .collect();
    variables.extend(
        (1..=discrete).map(|j| InputVariable::new_discrete(format!("k{j}"), 0.0, 1.0, 9.0)),
    );
    let constraints: Vec<String> = (1..=n)
        .map(|i| {
            format!(
                "x{i} + 0.1 * k{} <= 1.2 - 0.001 * x{}",
                1 + i % discrete,
                1 + i % n
            )
        })
        .collect();
    system(variables, &constraints)
}

/// The point the probe repairs: past every upper bound, every level at its
/// top.
#[allow(dead_code, reason = "only the repair stalls use it")]
fn beyond(n: usize, discrete: usize) -> Vec<f64> {
    (0..n + discrete)
        .map(|i| if i < n { 1.15 } else { 9.0 })
        .collect()
}

#[allow(dead_code, reason = "the brute-force stall uses its own system")]
fn plane() -> ConstraintSystem {
    system(
        vec![
            InputVariable::new_continuous("x1", 0.0, 1.0),
            InputVariable::new_continuous("x2", 0.0, 1.0),
        ],
        &["x1 + x2 < 1".to_owned()],
    )
}

#[cfg(sojourn_stall = "newton_guesses_bounds")]
#[test]
fn a_repair_newton_cannot_project_ends_at_the_limit() {
    // setup
    let system = coupled(80, 3);
    let region = region(&system);

    // act
    let ended = stalled(|| {
        let _ = region.repair(&beyond(80, 3), 0.0);
    });

    // assert
    assert_ended_by_limit("repair", &ended);
}

#[cfg(sojourn_stall = "newton_never_converges")]
#[test]
fn a_projection_that_never_converges_ends_at_the_limit() {
    // setup
    let system = plane();
    let region = region(&system);

    // act
    let ended = stalled(|| {
        let _ = region.repair(&[0.9, 0.9], 0.0);
    });

    // assert
    assert_ended_by_limit("repair", &ended);
}

#[cfg(sojourn_stall = "level_search_never_counts")]
#[test]
fn a_level_search_that_never_counts_ends_at_the_limit() {
    // setup: a million assignments of six lattices, every one feasible
    let system = coupled(12, 6);
    let region = region(&system);

    // act
    let ended = stalled(|| {
        let _ = region.repair(&beyond(12, 6), 0.0);
    });

    // assert
    assert_ended_by_limit("repair", &ended);
}

#[cfg(any(
    sojourn_stall = "walker_never_settles",
    sojourn_stall = "random_direction_never_finite",
))]
#[test]
fn a_walker_that_never_moves_ends_solve_at_the_limit() {
    // setup
    let system = plane();

    // act: the walker's burn-in is the first thing to move a chain
    let ended = stalled(|| {
        let _ = region(&system);
    });

    // assert
    assert_ended_by_limit("solve", &ended);
}

#[cfg(sojourn_stall = "contract_never_settles")]
#[test]
fn a_contraction_that_never_settles_ends_solve_at_the_limit() {
    // setup
    let system = plane();

    // act: contraction is the ladder's first rung
    let ended = stalled(|| {
        let _ = region(&system);
    });

    // assert
    assert_ended_by_limit("solve", &ended);
}

#[cfg(sojourn_stall = "brute_force_never_ends")]
#[test]
fn a_brute_force_that_never_ends_ends_solve_at_the_limit() {
    // setup: a contradiction too thin for an enclosure, which only brute
    // force's budget would otherwise end (tests/cvg_pools.rs pins it)
    let system = system(
        vec![
            InputVariable::new_continuous("x", 0.0, 1.0),
            InputVariable::new_continuous("y", 0.0, 1.0),
        ],
        &["x + y <= 1".to_owned(), "x + y >= 1.000000001".to_owned()],
    );

    // act
    let ended = stalled(|| {
        let _ = ConstraintSolver::new()
            .with_proposal_budget(1_000_000)
            .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(1));
    });

    // assert
    assert_ended_by_limit("solve", &ended);
}

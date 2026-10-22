//! Choosing a design through [`Chooser`]: the region draws the candidates,
//! the chooser picks among them by index, and the region decides what a
//! short design means.
//!
//! The seam is drawn there so that the things a caller would otherwise
//! reinvent stay here: the pool is feasible and distinct, the caller's own
//! points are kept out of it, a small lattice is enumerated, and a design
//! that runs short is `Exhausted` or `Degenerate` by the region's judgement,
//! never the chooser's. A chooser that breaks its contract panics: a wrong
//! index is a bug in the chooser, not a verdict about the region. The plan
//! is `docs/sampling-quality.md`, step 3.

mod common;

use std::cell::RefCell;

use faer::{Mat, MatRef};
use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{
    Candidates, Chooser, ConstraintSolver, ConstraintSystem, Domain, FarthestFirst, FeasibleRegion,
    InputVariable, Point, SampleError,
};

const SEED: u64 = 0xC4_00_5E;

/// What a chooser was shown, copied out.
struct Seen {
    pool: Mat<f64>,
    existing: Mat<f64>,
    whole_region: bool,
    count: usize,
}

/// Takes the front of the pool, asks for `size` candidates, and remembers
/// what it was shown.
struct Front {
    size: usize,
    seen: RefCell<Option<Seen>>,
}

impl Front {
    fn new(size: usize) -> Self {
        Self {
            size,
            seen: RefCell::new(None),
        }
    }

    fn seen(&self) -> Seen {
        self.seen.take().expect("the chooser was asked")
    }
}

impl Chooser for Front {
    fn pool_size(&self, _count: usize) -> usize {
        self.size
    }

    fn choose(
        &self,
        candidates: &Candidates<'_>,
        count: usize,
        _rng: &mut Xoshiro256PlusPlus,
    ) -> Vec<usize> {
        *self.seen.borrow_mut() = Some(Seen {
            pool: candidates.pool.to_owned(),
            existing: candidates.existing.to_owned(),
            whole_region: candidates.whole_region,
            count,
        });
        (0..count).collect()
    }
}

/// Returns whatever it is told to, to break the contract on purpose.
struct Returns(fn(usize) -> Vec<usize>, usize);

impl Chooser for Returns {
    fn pool_size(&self, _count: usize) -> usize {
        self.1
    }

    fn choose(
        &self,
        _candidates: &Candidates<'_>,
        count: usize,
        _rng: &mut Xoshiro256PlusPlus,
    ) -> Vec<usize> {
        (self.0)(count)
    }
}

fn region(variables: Vec<InputVariable>, constraints: &[&str]) -> anyhow::Result<FeasibleRegion> {
    let system = ConstraintSystem::new(variables, constraints.iter().copied())?;
    Ok(ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(SEED))?)
}

fn square() -> anyhow::Result<FeasibleRegion> {
    region(
        vec![
            InputVariable::new_continuous("x1", 0.0, 1.0),
            InputVariable::new_continuous("x2", 0.0, 1.0),
        ],
        &[],
    )
}

fn columns(points: MatRef<'_, f64>) -> Vec<Point> {
    (0..points.ncols())
        .map(|column| {
            (0..points.nrows())
                .map(|row| points[(row, column)])
                .collect()
        })
        .collect()
}

fn none() -> Mat<f64> {
    Mat::zeros(0, 0)
}

/// The message of the panic `act` raises.
fn panic_message(act: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let payload = std::panic::catch_unwind(act).expect_err("the call should panic");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .expect("a panic message")
}

#[test]
fn when_a_chooser_takes_the_front_of_the_pool_should_return_those_candidates() -> anyhow::Result<()>
{
    // setup
    let region = square()?;
    let chooser = Front::new(300);

    // act
    let design = region.sample_with(
        none().as_ref(),
        5,
        &chooser,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;

    // assert
    let seen = chooser.seen();
    assert_eq!(seen.count, 5);
    assert!(!seen.whole_region);
    assert!(
        seen.pool.ncols() >= 300,
        "the chooser asked for 300 candidates and was shown {}",
        seen.pool.ncols()
    );
    assert_eq!(
        columns(design.as_ref()),
        columns(seen.pool.as_ref())[..5].to_vec()
    );
    Ok(())
}

#[test]
fn when_the_pool_is_drawn_should_hold_no_point_twice() -> anyhow::Result<()> {
    // setup: a region of three points, which every draw and walk revisits
    let region = region(
        vec![InputVariable::new(
            "x1",
            Domain::Levels(vec![1.0, 2.0, 4.0]),
        )],
        &[],
    )?;
    let chooser = Front::new(3);

    // act
    let design = region.sample_with(
        none().as_ref(),
        3,
        &chooser,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;

    // assert
    let seen = chooser.seen();
    let mut pool = columns(seen.pool.as_ref());
    pool.sort_by(|a, b| a[0].total_cmp(&b[0]));
    assert_eq!(pool, vec![vec![1.0], vec![2.0], vec![4.0]]);
    assert!(
        seen.whole_region,
        "three levels are a lattice small enough to enumerate"
    );
    assert_eq!(design.ncols(), 3);
    Ok(())
}

#[test]
fn when_the_caller_has_points_should_show_them_and_keep_them_out_of_the_pool() -> anyhow::Result<()>
{
    // setup
    let region = square()?;
    let witness = region.witness().to_vec();
    let existing = Mat::from_fn(2, 1, |row, _| witness[row]);
    let chooser = Front::new(200);

    // act
    region.sample_with(
        existing.as_ref(),
        4,
        &chooser,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;

    // assert
    let seen = chooser.seen();
    assert_eq!(columns(seen.existing.as_ref()), vec![witness.clone()]);
    assert!(
        !columns(seen.pool.as_ref()).contains(&witness),
        "the caller's point is not a candidate"
    );
    Ok(())
}

#[test]
fn when_a_region_is_one_point_should_ask_for_one_and_report_degenerate() -> anyhow::Result<()> {
    // setup
    let region = region(
        vec![InputVariable::new_continuous("x1", 3.0, 3.0)],
        &["x1 > 0"],
    )?;
    let chooser = Front::new(50);

    // act
    let verdict = region.sample_with(
        none().as_ref(),
        5,
        &chooser,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    );

    // assert
    let seen = chooser.seen();
    assert_eq!((seen.pool.ncols(), seen.count), (1, 1));
    let Err(SampleError::Degenerate { found, wanted }) = verdict else {
        panic!("a point-sized region should be degenerate, got {verdict:?}");
    };
    assert_eq!((found.ncols(), wanted), (1, 5));
    Ok(())
}

#[test]
fn when_a_lattice_runs_out_should_report_exhausted_with_every_point() -> anyhow::Result<()> {
    // setup: tenths up to 0.3, four points
    let region = region(
        vec![InputVariable::new_discrete("t", 0.0, 0.1, 1.0)],
        &["t <= 0.3"],
    )?;
    let chooser = Front::new(50);

    // act
    let verdict = region.sample_with(
        none().as_ref(),
        10,
        &chooser,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    );

    // assert
    let seen = chooser.seen();
    assert!(seen.whole_region);
    assert_eq!((seen.pool.ncols(), seen.count), (4, 4));
    let Err(SampleError::Exhausted { found, wanted }) = verdict else {
        panic!("expected Exhausted, got {verdict:?}");
    };
    assert_eq!((found.ncols(), wanted), (4, 10));
    Ok(())
}

#[test]
fn when_farthest_first_is_passed_explicitly_should_design_what_sample_does() -> anyhow::Result<()> {
    // setup
    let region = square()?;

    // act
    let explicit = region.sample_with(
        none().as_ref(),
        12,
        &FarthestFirst,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;
    let implicit = region.sample(
        none().as_ref(),
        12,
        &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
    )?;

    // assert
    assert_eq!(columns(explicit.as_ref()), columns(implicit.as_ref()));
    Ok(())
}

fn broken(chooser: Returns) -> String {
    let region = square().expect("a square solves");
    panic_message(move || {
        let _ = region.sample_with(
            none().as_ref(),
            4,
            &chooser,
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        );
    })
}

#[test]
fn when_a_chooser_repeats_an_index_should_panic_naming_it() {
    // act
    let message = broken(Returns(|_| vec![0, 1, 1, 2], 200));

    // assert
    assert!(message.contains("index 1 twice"), "{message}");
}

#[test]
fn when_a_chooser_returns_an_index_past_the_pool_should_panic_naming_it() {
    // act
    let message = broken(Returns(|_| vec![0, 1, 2, usize::MAX], 200));

    // assert
    assert!(
        message.contains(&format!("index {}", usize::MAX)),
        "{message}"
    );
}

#[test]
fn when_a_chooser_returns_the_wrong_number_should_panic_with_both_counts() {
    // act
    let message = broken(Returns(|count| (0..count - 1).collect(), 200));

    // assert
    assert!(message.contains("3 indices for a design of 4"), "{message}");
}

#[test]
fn when_a_chooser_asks_for_fewer_candidates_than_points_should_panic() {
    // act
    let message = broken(Returns(|count| (0..count).collect(), 3));

    // assert
    assert!(
        message.contains("3 candidates for a design of 4"),
        "{message}"
    );
}

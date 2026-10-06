//! The brute squad's own tests: does the pool find a first feasible point in
//! the regime where it must *sample harder*, and how many candidates a second
//! can it judge while trying?
//!
//! `docs/brute-squad.md` is the plan this file is step 0
//! of. The regime is a feasible fraction of about 1e-6 to 1e-9, where no
//! seeder can help — the region is too thin for a contraction to isolate and
//! too oddly shaped for a local solve to fall into — and the honest answer is
//! wide-batch sampling on every core and any GPU present. These tests are red
//! until that exists, and go green a rung at a time as each tier lands.
//!
//! # Two fixtures
//!
//! **Time to first hit** is empirical: did a point arrive inside the budget or
//! not. It goes through the public [`ConstraintSolver::solve`] path, because
//! the tier being built lives inside it, and it keeps the seeders *out* by
//! passing [`SAMPLING_ONLY`] — a contraction settles `x1 > 0.999999` at once
//! and would turn every rung into a measurement of the contractor.
//!
//! **Checks per second** is the engineering dial: uniform candidates filled
//! straight into a matrix, every constraint of a family evaluated over the
//! batch, and the columns that pass counted. Two rates, `eval-only` against
//! `pipeline`, separate the evaluator from the random-number generation that
//! has to feed it. Recorded into `performance-records/` like the evaluator
//! ledgers, with the same caveats.
//!
//! # The families
//!
//! Four shapes over the unit cube, each with an analytically exact feasible
//! fraction `p`, so a test named `1e6` really is a one-in-a-million region:
//!
//! | family | why `p` is exact |
//! |---|---|
//! | corner: `x_i > 1 - q`, `q = p^(1/3)` | the box `(1-q, 1]^3` has volume `q^3` |
//! | ball: `x1^2 + x2^2 + x3^2 < r^2`, `r = (6p/π)^(1/3)` | one octant of a ball is `π r^3 / 6` |
//! | sine corner: `sin(x_i) > sin(1 - q)` | `sin` is increasing on `[0, 1]`, so this is the corner again |
//! | real power: `x1^1.234 < p^1.234` | `pow` is increasing on `[0, 1]`, so this is `x1 < p` |
//!
//! The sine corner is the one the tier exists for: the same region as the
//! corner, but written so that no solver can be asked about it. The ball is
//! there because a corner is axis-aligned, which is the easy shape for anything
//! that boxes in a region once it has a point; a sphere is not.
//!
//! # Time to first hit is geometric
//!
//! With hit rate `p` the expected number of proposals before the first hit is
//! `1/p`, and the tail is heavy: a quarter of runs need 1.4x the expectation,
//! one in twenty needs 3x. So a single attempt against a budget is a coin with
//! a known bias, and this file runs up to three: **pass on two hits, fail on
//! two misses.** With `m = exp(-budget / expected)` the chance a green rung
//! fails is `3m² - 2m³` — 4.9% at a budget of twice the expectation, 0.7% at
//! three times, 0.013% at five. The rule for the tiers that follow: a rung is
//! *green* only once its expected first-hit time is at most a third of the
//! budget, and anything closer than that is not a reading.
//!
//! The 1e-4 rungs used to be a coin: with the pool giving up after its
//! 10,000-candidate probe, a seeded run either landed in that probe or it did
//! not, and two of three seeds did not. Step 4's loop is what made them a
//! reading — the second batch lands what the first missed.
//!
//! # What "red" looks like
//!
//! Since step 4 the pool keeps sampling after an empty probe until a batch
//! lands or a proposal budget is spent: a billion on the CPU threads
//! ([`DEFAULT_PROPOSAL_BUDGET`](sojourn::DEFAULT_PROPOSAL_BUDGET)), thirty
//! billion on a GPU ([`DEFAULT_GPU_PROPOSAL_BUDGET`](sojourn::DEFAULT_GPU_PROPOSAL_BUDGET))
//! since step 3 put the sieve there. A rung beyond that reach spends the budget
//! and reports `gave up` — three to seven seconds on this laptop's sixteen
//! threads, about fifteen on its iGPU — and a rung whose wall budget is shorter
//! than that reports `timed out` instead. Before step 4 every rung below 1e-4
//! failed in milliseconds. The rung comments below say which tier each one was
//! green from, and on what.
//!
//! Every attempt that reaches brute force pays for the GPU: connecting to the
//! adapter and compiling the shader is about 200 ms here, and the device is
//! released again when brute force returns, because a library should not keep
//! a host's GPU open after it is done with it. That is why a rung the probe
//! cannot land reads as a couple of hundred milliseconds even at 1e-6, where
//! the search itself takes a few.
//!
//! # The budget
//!
//! [`solve`] runs on the calling thread to a verdict, bounded by its
//! proposal budget and nothing else. The wall budget here is read after the
//! fact: a point found inside it is a hit, one found after it is `timed
//! out`, and a search that spent its proposals is `gave up`. An attempt that
//! would miss therefore runs to the proposal budget rather than stopping at
//! the deadline — seconds, on the rungs that are red — which is the price
//! of a library with no clock in it.
//!
//! [`solve`]: ConstraintSolver::solve

mod common;

use std::f64::consts::PI;
use std::fmt;
use std::hint::black_box;
use std::time::{Duration, Instant};

use faer::Mat;
use rand::rngs::Xoshiro256PlusPlus;
use rand::{RngExt, SeedableRng};
use sojourn::CompiledExpression;
use sojourn::{
    ConstraintSolver, ConstraintSystem, DEFAULT_STRATEGIES, Infeasibility, InputVariable, Strategy,
};

use common::{profile_label, throughput};

const SEED: u64 = 0x50_50_1E_5E_ED;
const RIVAL_SEED: u64 = 0x0D_D5_0F_1E_5E;
const THIRD_SEED: u64 = 0xB2_07_E5_90_AD;

/// Three attempts, three unrelated seeds. Pinned so that a failure reproduces.
const SEEDS: [u64; 3] = [SEED, RIVAL_SEED, THIRD_SEED];

/// The production ladder with everything that finds a point by means other
/// than sampling removed — the solver, and the local solve, which would seed
/// every rung from the box centre in milliseconds and measure nothing. Pinned
/// against [`DEFAULT_STRATEGIES`] by
/// [`sampling_only_is_the_default_ladder_minus_the_seeders`], so a tier added
/// to production is measured here unless it is a seeder, which is the one kind
/// this file must not measure.
const SAMPLING_ONLY: &[Strategy] = &[Strategy::BruteSquad, Strategy::HitAndRun];

const VARIABLES: [&str; 3] = ["x1", "x2", "x3"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Corner,
    Ball,
    SineCorner,
    RealPower,
}

impl Family {
    const ALL: [Family; 4] = [
        Family::Corner,
        Family::Ball,
        Family::SineCorner,
        Family::RealPower,
    ];

    /// Constraint sources with feasible fraction exactly `p` over the unit cube.
    ///
    /// Literals are written with `{}`: `Display` never produces exponent
    /// notation for an `f64`, where `{:?}` might, and the grammar's `FLOAT`
    /// only admits an exponent on a number with a decimal point.
    fn sources(self, p: f64) -> Vec<String> {
        assert!(
            p > 0.0 && p <= 0.5,
            "feasible fraction {p} is not a fraction worth testing"
        );
        match self {
            Family::Corner => {
                let q = p.cbrt();
                VARIABLES
                    .iter()
                    .map(|x| format!("{x} > {}", 1.0 - q))
                    .collect()
            }
            Family::Ball => {
                let r = (6.0 * p / PI).cbrt();
                assert!(
                    r <= 1.0,
                    "an octant of radius {r} does not fit in the unit cube"
                );
                vec![format!("x1^2 + x2^2 + x3^2 < {}", r * r)]
            }
            Family::SineCorner => {
                let q = p.cbrt();
                // `sin(1 - q)` folds to a literal at compile time; the `sin`
                // on the left survives, because `invert_monotone` does not know
                // the box and so cannot know `sin` is monotone on it. A unit
                // test in `rewrite.rs` pins that.
                VARIABLES
                    .iter()
                    .map(|x| format!("sin({x}) > sin({})", 1.0 - q))
                    .collect()
            }
            Family::RealPower => {
                // A real exponent is the one power no solver takes and no
                // backend turns into multiplication, so every check here is a
                // libm `powf` on the CPU against the GPU's `pow`: the shape
                // that measures the special-function hardware on each side.
                vec![format!("x1^1.234 < {}", p.powf(1.234))]
            }
        }
    }

    /// The ledger file this family records into, without extension.
    fn slug(self) -> &'static str {
        match self {
            Family::Corner => "brute-corner",
            Family::Ball => "brute-ball",
            Family::SineCorner => "brute-sine-corner",
            Family::RealPower => "brute-real-power",
        }
    }
}

fn inputs() -> Vec<InputVariable> {
    VARIABLES
        .iter()
        .map(|name| InputVariable::new(*name, 0.0, 1.0))
        .collect()
}

/// A validated [`ConstraintSystem`], panicking on a fixture that does not bind.
fn system(constraints: &[String]) -> ConstraintSystem {
    ConstraintSystem::new(inputs(), constraints.iter().cloned())
        .expect("a family's constraints should bind to the unit cube")
}

// ------------------------------------------------------------ time to first hit

enum Outcome {
    /// A point arrived, and it was re-checked against every source.
    Found(Duration),
    /// The pool returned a verdict of "nothing" before the budget ran out.
    GaveUp(Duration, Infeasibility),
    /// A point was found, but after the budget.
    TimedOut(Duration),
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Found(elapsed) => write!(f, "found in {elapsed:.1?}"),
            Outcome::GaveUp(elapsed, because) => {
                write!(f, "gave up after {elapsed:.1?} ({because:?})")
            }
            Outcome::TimedOut(budget) => write!(f, "timed out at {budget:.1?}"),
        }
    }
}

/// One attempt at a first point, with the solver left out and the clock
/// read the moment the region reports it has one.
fn attempt(family: Family, p: f64, seed: u64, budget: Duration) -> Outcome {
    let sources = family.sources(p);
    let solver = ConstraintSolver::new().with_strategies(SAMPLING_ONLY.to_vec());

    let system = system(&sources);
    let start = Instant::now();
    match solver.solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(seed)) {
        Ok(region) => {
            let elapsed = start.elapsed();

            // `Satisfied` promises a point is already in hand; make it show
            // one, and make the point pass every source in `f64`, the way
            // `cvg_pools` does. A region that says "found" and holds nothing
            // would otherwise pass this fixture.
            let point = region.witness();
            let bindings: Vec<(&str, f64)> = VARIABLES
                .iter()
                .enumerate()
                .map(|(row, name)| (*name, point[row]))
                .collect();
            for source in &sources {
                let residual = common::eval_one(source, &bindings)
                    .unwrap_or_else(|e| panic!("{source:?} failed to evaluate: {e}"));
                assert!(
                    residual <= 0.0,
                    "{family:?}: delivered point {bindings:?} violates {source:?} by {residual}"
                );
            }
            if elapsed > budget {
                return Outcome::TimedOut(budget);
            }
            Outcome::Found(elapsed)
        }
        Err(because) => Outcome::GaveUp(start.elapsed(), because),
    }
}

/// Two of three, and stop as soon as the answer is known.
fn first_hit(family: Family, p: f64, budget: Duration) {
    let (mut hits, mut misses) = (0, 0);
    for (attempt_no, seed) in SEEDS.iter().enumerate() {
        let outcome = attempt(family, p, *seed, budget);
        println!(
            "{family:?} at p = {p:e}, attempt {} (seed {seed:#x}): {outcome}",
            attempt_no + 1
        );
        match outcome {
            Outcome::Found(_) => hits += 1,
            Outcome::GaveUp(..) | Outcome::TimedOut(_) => misses += 1,
        }
        if hits == 2 {
            return;
        }
        if misses == 2 {
            panic!(
                "{family:?} at p = {p:e}: {misses} of {} attempts found nothing inside {budget:?}",
                attempt_no + 1
            );
        }
    }
    unreachable!("three attempts always settle two-of-three");
}

/// Runs in debug too: proves the family construction, the polling budget and
/// the two-of-three loop on a region every strategy reaches.
#[test]
fn the_harness_finds_an_easy_region() {
    for family in Family::ALL {
        first_hit(family, 1e-2, Duration::from_secs(2));
    }
}

/// The strategy list this file measures with is production minus the seeders
/// — branch-and-prune and the local solve — and nothing else, so a tier that
/// joins the defaults is measured here too unless it is one more way of
/// finding a point without sampling.
#[test]
fn sampling_only_is_the_default_ladder_minus_the_seeders() {
    let expected: Vec<Strategy> = DEFAULT_STRATEGIES
        .iter()
        .copied()
        .filter(|strategy| !matches!(strategy, Strategy::Prune | Strategy::LocalSolve))
        .collect();
    assert_eq!(SAMPLING_ONLY, expected.as_slice());
}

/// Without branch-and-prune in the list an empty region is `NotFound`, never
/// `Proved`: nothing was asked that could prove anything. This is the property
/// every budgeted test below relies on, checked on a region that is empty by
/// construction so it holds in either profile and at any speed — with the
/// debug-sized budget, because a billion proposals on an unoptimised tape is
/// minutes, and the verdict on an empty region is the same at any budget.
#[test]
fn without_a_prover_an_empty_region_is_not_found_rather_than_proved() {
    let constraints = ["x1 > 2.0".to_owned()];
    let verdict = ConstraintSolver::new()
        .with_proposal_budget(common::PROPOSAL_BUDGET)
        .with_strategies(SAMPLING_ONLY.to_vec())
        .solve(
            &system(&constraints),
            &mut Xoshiro256PlusPlus::seed_from_u64(SEED),
        );

    match verdict {
        Err(Infeasibility::NotFound { unexpressed }) => {
            let sources: Vec<&str> = unexpressed.iter().map(|c| c.source.as_str()).collect();
            assert_eq!(
                sources,
                ["x1 > 2.0"],
                "every constraint is unexpressed when nothing was asked"
            );
        }
        other => panic!("expected NotFound without a prover, got {other:?}"),
    }
}

// Target hit rate 1e-4: one in ten thousand.
//
// The probe alone expects one hit here, which made these a coin until step 4;
// the loop's second batch lands what the probe missed, in a millisecond.

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn corner_1e4() {
    first_hit(Family::Corner, 1e-4, Duration::from_secs(2));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn ball_1e4() {
    first_hit(Family::Ball, 1e-4, Duration::from_secs(2));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn sine_corner_1e4() {
    first_hit(Family::SineCorner, 1e-4, Duration::from_secs(2));
}

// Target hit rate 1e-6: one in a million.
//
// Green since step 4, the loop: a few hundred batches on the step 1 tape,
// tens of milliseconds on any core count.

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn corner_1e6() {
    first_hit(Family::Corner, 1e-6, Duration::from_secs(5));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn ball_1e6() {
    first_hit(Family::Ball, 1e-6, Duration::from_secs(5));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn sine_corner_1e6() {
    first_hit(Family::SineCorner, 1e-6, Duration::from_secs(5));
}

// Target hit rate 1e-8: one in a hundred million.
//
// Green since step 4 on the step 2 pipeline: a hundred million proposals is a
// tenth of the default budget, under a second across this laptop's threads.

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn corner_1e8() {
    first_hit(Family::Corner, 1e-8, Duration::from_secs(10));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn ball_1e8() {
    first_hit(Family::Ball, 1e-8, Duration::from_secs(10));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn sine_corner_1e8() {
    first_hit(Family::SineCorner, 1e-8, Duration::from_secs(10));
}

// Target hit rate 1e-10: one in ten billion.
//
// The GPU sieve's rung (step 3). Three times the expectation is thirty billion
// proposals, the GPU's default budget; this laptop's iGPU draws and sieves two
// billion candidates a second, so the budget takes fifteen seconds and the
// rung reports `timed out` at ten. A desktop card is ten to twenty times that
// and is expected to spend the budget in a second or two, which is what makes
// this green there and measured here. On the CPU alone the billion-proposal
// budget is a tenth of the expectation and the rung gives up after a few
// seconds. The sine corner is included because the whole bet of the GPU tier
// is that transcendentals are the special function units' problem and not
// ours — and on this iGPU it runs at the same rate as the plain corner.

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn corner_1e10() {
    first_hit(Family::Corner, 1e-10, Duration::from_secs(10));
}

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn sine_corner_1e10() {
    first_hit(Family::SineCorner, 1e-10, Duration::from_secs(10));
}

// Target hit rate 1e-12: one in a trillion.
//
// Permanently red. A trillion proposals in a second is beyond any hardware this
// is going to run on; the rung is here so that the wall is visible and so that
// a change to the pool that starts *claiming* success here is caught. Short
// budget, because trying is not the point: it times out before either engine
// would give up.

#[test]
#[cfg_attr(debug_assertions, ignore = "time-budgeted; release only: `just brute`")]
fn corner_1e12() {
    first_hit(Family::Corner, 1e-12, Duration::from_secs(1));
}

// -------------------------------------------------------------- checks per second

/// Columns per batch. Three rows of 1024 `f64` is 24 KB, comfortably in L1 or
/// L2, so this measures evaluation rather than memory; the tape's tile size is
/// a later question and this number is where it starts.
const WIDTH: usize = 1024;

/// How many distinct batches to cycle through, so the optimiser cannot hoist
/// the evaluation.
const ROTATION: usize = 16;

/// The feasible fraction the checks/s cases run at. Cost does not depend on
/// `p`, and at one in a hundred the count check below has real power.
const CHECKS_P: f64 = 1e-2;

/// Fresh columns for the count check: at `p = 1e-2` the expected count is 2,000
/// with a standard deviation of 44.5, so 225 is five sigma.
const COUNT_CHECK_COLUMNS: usize = 200_000;
const COUNT_CHECK_EXPECTED: f64 = COUNT_CHECK_COLUMNS as f64 * CHECKS_P;
const COUNT_CHECK_TOLERANCE: f64 = 225.0;

fn compiled(family: Family, p: f64) -> Vec<CompiledExpression> {
    family
        .sources(p)
        .iter()
        .map(|source| {
            sojourn::compile(source, &VARIABLES).expect("a family binds to its own schema")
        })
        .collect()
}

/// How many columns of `batch` satisfy every constraint.
///
/// One `eval` per constraint over the whole batch, then a column-wise `and`.
/// None of the families can produce a non-finite value on the unit cube, so a
/// failed `eval` is a harness bug rather than a property of the sample.
fn feasible_count(constraints: &[CompiledExpression], batch: &Mat<f64>) -> usize {
    let mut passes = vec![true; batch.ncols()];
    for constraint in constraints {
        let residuals = constraint
            .eval(batch.as_ref())
            .expect("no family produces a non-finite value on the unit cube");
        for (column, pass) in passes.iter_mut().enumerate() {
            *pass &= residuals[column] <= 0.0;
        }
    }
    passes.iter().filter(|pass| **pass).count()
}

/// Overwrites every column with a fresh uniform sample of the unit cube, through
/// the pool's own fill so that `pipeline` measures the production generator.
fn refill(batch: &mut Mat<f64>, rng: &mut Xoshiro256PlusPlus) {
    sojourn::fill_box(batch, &[(0.0, 1.0); 3], rng);
}

struct Measurement {
    family: Family,
    /// Checks per second on batches prepared up front.
    eval_only: f64,
    /// Checks per second including generating each batch first.
    pipeline: f64,
}

fn measure(family: Family) -> Measurement {
    let constraints = compiled(family, CHECKS_P);
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(SEED);
    let mut batches: Vec<Mat<f64>> = (0..ROTATION)
        .map(|_| Mat::from_fn(VARIABLES.len(), WIDTH, |_, _| rng.random_range(0.0..=1.0)))
        .collect();

    // `throughput` counts calls per millisecond; a call is `WIDTH` checks.
    #[expect(
        clippy::cast_precision_loss,
        reason = "batch widths are far below the f64 integer limit"
    )]
    let per_call_to_per_second = WIDTH as f64 * 1000.0;

    let eval_only = per_call_to_per_second
        * throughput(|index| {
            let batch = black_box(&batches[index % ROTATION]);
            feasible_count(&constraints, batch) as f64
        });

    let pipeline = per_call_to_per_second
        * throughput(|index| {
            let batch = &mut batches[index % ROTATION];
            refill(batch, &mut rng);
            feasible_count(&constraints, black_box(batch)) as f64
        });

    Measurement {
        family,
        eval_only,
        pipeline,
    }
}

fn report(measurements: &[Measurement]) {
    println!();
    println!(
        "brute squad, constraint checks per second ({})",
        profile_label()
    );
    println!("{:-<62}", "");
    println!(
        "{:<14} {:>16} {:>16} {:>12}",
        "family", "eval-only", "pipeline", "rng cost"
    );
    for m in measurements {
        println!(
            "{:<14} {:>16.0} {:>16.0} {:>11.0}%",
            format!("{:?}", m.family),
            m.eval_only,
            m.pipeline,
            (1.0 - m.pipeline / m.eval_only) * 100.0
        );
    }
    println!("{:-<62}", "");
    println!("a check is one {WIDTH}-column batch judged against every constraint");
    println!("of the family, divided out per column. `pipeline` refills the batch");
    println!("with fresh uniform samples first; the gap is what the RNG costs.");
    println!();
}

/// Column widths match the row format in [`record_in_ledgers`].
const LEDGER_HEADER: &str = "sep=;\nversion                 ;timestamp               ;host                    ;vars ;batch ;eval-only   ;pipeline    ;\n";

fn record_in_ledgers(measurements: &[Measurement]) {
    let version = env!("CARGO_PKG_VERSION");
    let host = common::host();
    let timestamp = common::timestamp_utc();

    let mut written = 0;
    for m in measurements {
        let row = format!(
            "{version:<24};{timestamp:<24};{host:<24};{:<5};{:<6};{:<12.0};{:<12.0};",
            VARIABLES.len(),
            WIDTH,
            m.eval_only,
            m.pipeline
        );
        if common::record_row(m.family.slug(), LEDGER_HEADER, &row) {
            written += 1;
        }
    }
    if written > 0 {
        println!(
            "recorded {version} into {written} ledgers under {}",
            common::LEDGER_DIR
        );
    }
}

/// The GPU sieve's rates, when there is an adapter. Two, like the CPU's:
/// `given` uploads a batch the CPU drew and sieves it — the evaluator alone,
/// plus the upload — and `generated` has the device draw its own, which is
/// what brute force actually runs. Batches are far wider than the CPU's
/// because a dispatch costs tens of microseconds before it does anything.
#[cfg(feature = "gpu")]
struct GpuMeasurement {
    family: Family,
    adapter: String,
    given: f64,
    generated: f64,
}

/// Columns per `given` dispatch: a million, so the upload and the dispatch
/// overhead are amortised the way brute force amortises them.
#[cfg(feature = "gpu")]
const GPU_GIVEN_WIDTH: usize = 1 << 20;

/// Candidates per `generated` dispatch: the sieve's own batch, four million.
#[cfg(feature = "gpu")]
const GPU_GENERATED_WIDTH: u32 = 1 << 22;

/// The feasible fraction the `generated` rate is measured at: a few survivors
/// per dispatch, which is what brute force sees.
#[cfg(feature = "gpu")]
const GPU_GENERATED_P: f64 = 1e-6;

#[cfg(feature = "gpu")]
fn measure_gpu(family: Family) -> Option<GpuMeasurement> {
    use sojourn::gpu;

    let adapter = gpu::adapter_name()?;
    let sieve = gpu::sieve_for(&system(&family.sources(CHECKS_P)))?;
    // The rate brute force runs at is the rate where survivors are rare: at
    // one in a hundred a four-million dispatch reports forty thousand rows,
    // and reading those back is CPU work that swamps the device's.
    let sparse = gpu::sieve_for(&system(&family.sources(GPU_GENERATED_P)))?;

    let mut rng = Xoshiro256PlusPlus::seed_from_u64(SEED);
    let mut batch = Mat::zeros(VARIABLES.len(), GPU_GIVEN_WIDTH);
    refill(&mut batch, &mut rng);

    #[expect(
        clippy::cast_precision_loss,
        reason = "batch widths are far below the f64 integer limit"
    )]
    let given = GPU_GIVEN_WIDTH as f64
        * 1000.0
        * throughput(|_| {
            sieve
                .survivors_of(black_box(&batch).as_ref())
                .map_or(f64::NAN, |survivors| survivors.len() as f64)
        });

    let generated = f64::from(GPU_GENERATED_WIDTH)
        * 1000.0
        * throughput(|index| {
            sparse
                .survivors_generated(SEED, index as u64, GPU_GENERATED_WIDTH)
                .map_or(f64::NAN, |survivors| survivors.len() as f64)
        });

    Some(GpuMeasurement {
        family,
        adapter,
        given,
        generated,
    })
}

#[cfg(feature = "gpu")]
fn report_gpu(measurements: &[GpuMeasurement]) {
    let Some(first) = measurements.first() else {
        println!("gpu sieve: no adapter, not measured");
        println!();
        return;
    };
    println!("gpu sieve, candidates per second on {}", first.adapter);
    println!("{:-<62}", "");
    println!("{:<14} {:>16} {:>16}", "family", "given", "generated");
    for m in measurements {
        println!(
            "{:<14} {:>16.0} {:>16.0}",
            format!("{:?}", m.family),
            m.given,
            m.generated
        );
    }
    println!("{:-<62}", "");
    println!("`given` uploads a {GPU_GIVEN_WIDTH}-column batch the CPU drew (p = {CHECKS_P}) and");
    println!("sieves it; `generated` has the device draw {GPU_GENERATED_WIDTH} candidates itself");
    println!("at p = {GPU_GENERATED_P}, which is what brute force runs. Survivors are not");
    println!("re-judged here.");
    println!();
}

/// Column widths match the row format in [`record_in_gpu_ledgers`].
#[cfg(feature = "gpu")]
const GPU_LEDGER_HEADER: &str = "sep=;\nversion                 ;timestamp               ;host                    ;adapter                                 ;vars ;given-batch ;generated-batch ;given       ;generated   ;\n";

#[cfg(feature = "gpu")]
fn record_in_gpu_ledgers(measurements: &[GpuMeasurement]) {
    let version = env!("CARGO_PKG_VERSION");
    let host = common::host();
    let timestamp = common::timestamp_utc();

    let mut written = 0;
    for m in measurements {
        let row = format!(
            "{version:<24};{timestamp:<24};{host:<24};{:<40};{:<5};{:<12};{:<16};{:<12.0};{:<12.0};",
            m.adapter,
            VARIABLES.len(),
            GPU_GIVEN_WIDTH,
            GPU_GENERATED_WIDTH,
            m.given,
            m.generated
        );
        if common::record_row(
            &m.family.slug().replacen("brute-", "brute-gpu-", 1),
            GPU_LEDGER_HEADER,
            &row,
        ) {
            written += 1;
        }
    }
    if written > 0 {
        println!(
            "recorded {version} into {written} gpu ledgers under {}",
            common::LEDGER_DIR
        );
    }
}

/// The count check first, because it is what proves the family sources mean
/// what the test names say; then the rates, with shape checks only.
#[test]
fn checks_per_second() {
    // Independent of timing and of profile: a fresh 200,000-column sample of
    // each family must pass at the rate its `p` promises.
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(RIVAL_SEED);
    for family in Family::ALL {
        let constraints = compiled(family, CHECKS_P);
        let sample = Mat::from_fn(VARIABLES.len(), COUNT_CHECK_COLUMNS, |_, _| {
            rng.random_range(0.0..=1.0)
        });
        #[expect(
            clippy::cast_precision_loss,
            reason = "a count of columns is far below the f64 integer limit"
        )]
        let count = feasible_count(&constraints, &sample) as f64;
        assert!(
            (count - COUNT_CHECK_EXPECTED).abs() <= COUNT_CHECK_TOLERANCE,
            "{family:?} at p = {CHECKS_P}: {count} of {COUNT_CHECK_COLUMNS} columns passed, \
             expected {COUNT_CHECK_EXPECTED} ± {COUNT_CHECK_TOLERANCE}; the family's sources \
             do not describe the region its name claims"
        );
    }

    let measurements: Vec<Measurement> = Family::ALL.iter().copied().map(measure).collect();
    report(&measurements);
    record_in_ledgers(&measurements);

    // Release only, like the rungs: `throughput` runs sixty-four calls between
    // clock reads, and a debug build spends seconds per call marshalling a
    // million-column batch, which is the harness and not the device.
    #[cfg(feature = "gpu")]
    if cfg!(debug_assertions) {
        println!("gpu sieve: release only, run `just bench`");
        println!();
    } else {
        let gpu: Vec<GpuMeasurement> = Family::ALL
            .iter()
            .copied()
            .filter_map(measure_gpu)
            .collect();
        report_gpu(&gpu);
        record_in_gpu_ledgers(&gpu);
        for m in &gpu {
            assert!(
                m.given.is_finite()
                    && m.given > 0.0
                    && m.generated.is_finite()
                    && m.generated > 0.0,
                "{:?}: the GPU sieve stopped answering mid-measurement",
                m.family
            );
        }
    }

    common::describe_host();

    for m in &measurements {
        assert!(
            m.eval_only.is_finite() && m.eval_only > 0.0,
            "{:?}: no eval-only rate was measured",
            m.family
        );
        assert!(
            m.pipeline.is_finite() && m.pipeline > 0.0,
            "{:?}: no pipeline rate was measured",
            m.family
        );
    }

    // Generating a batch cannot make evaluating it faster. Release only, for
    // the reason `throughput_benchmarks` learned the hard way: a 20 ms debug
    // window is not long enough to compare two similar rates under load. The
    // 0.9 absorbs the noise floor on a pair that should be equal at worst.
    for m in measurements.iter().filter(|_| !cfg!(debug_assertions)) {
        assert!(
            m.eval_only >= m.pipeline * 0.9,
            "{:?}: the pipeline rate {} beat the eval-only rate {}, so the harness is measuring \
             something other than what it claims",
            m.family,
            m.pipeline,
            m.eval_only
        );
    }
}

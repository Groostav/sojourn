//! How good are the designs `sample` hands out?
//!
//! Each fixture is a region whose distribution is known in closed form, with
//! an exact sampler for it written without sojourn: the reference a design
//! is judged against must not come from the machinery that made the design,
//! or a biased walker grades itself well. `sojourn::quality` does the
//! measuring; this file draws the references, runs the designs, and records
//! the numbers. The plan is `docs/sampling-quality.md`.
//!
//! Every case runs on ten generator seeds and records the mean and the
//! worst, beside a control: the same count of independent draws from the
//! exact sampler. The numbers are deterministic per seed and the same on
//! every machine, so the spread across seeds is the noise here, not the
//! wall clock's thirty percent. A change of generator, or of how the
//! generator is used, should land inside the old band; one that moves the
//! band is a finding.
//!
//! The design gets no floors yet: the first run measures, and the floors are
//! pinned from it. The control does, because the control checks the
//! measures — a random design that scores far from random means a metric is
//! wrong, not sojourn.
//!
//! Release only, by `just quality`: the reference's kernel sum is quadratic
//! in its twenty thousand points, and the twenty-dimensional simplex is
//! walked. The fixtures' own checks run in debug.

mod common;

use faer::Mat;
use rand::rngs::Xoshiro256PlusPlus;
use rand::{RngExt, SeedableRng};
use sojourn::quality::{self, Reference};
use sojourn::{ConstraintSolver, ConstraintSystem, Domain, InputVariable, Point};

/// Points in every reference. The empirical CDF is then within about
/// `1/140` of the true one, which is under a stratum's width at a hundred
/// points; the kernel's pair sum over them is the cost that bounds it.
const REFERENCE_COLUMNS: usize = 20_000;

const REFERENCE_SEED: u64 = 0x5E_F0_CA_57;

/// The control's streams are this xor the design's seed, so the two never
/// share one.
const CONTROL_SEED: u64 = 0xC0_47_20_15;

const SEEDS: u64 = 10;

/// Random designs per case for checking the measures, of which the first
/// [`SEEDS`] are the ledger's control. More than ten because a random
/// design's CL2 ratio is heavy-tailed — a weighted χ² dominated by a few
/// low-order projections, mean one and median well below it — so a mean of
/// ten strayed to 0.47 on the gearbox. Two hundred holds it within a tenth.
const CONTROL_TRIALS: u64 = 200;

/// A region whose distribution is known: the system sojourn is asked to
/// design over, and an exact uniform sampler for its feasible set.
struct Fixture {
    slug: &'static str,
    variables: Vec<InputVariable>,
    constraints: Vec<String>,
    /// One point drawn uniformly from the feasible set.
    draw: fn(&mut Xoshiro256PlusPlus) -> Point,
    /// Which piece of the region a point is in, where the region has pieces.
    piece: Option<fn(&[f64]) -> usize>,
    counts: &'static [usize],
}

impl Fixture {
    fn system(&self) -> anyhow::Result<ConstraintSystem> {
        Ok(ConstraintSystem::new(
            self.variables.clone(),
            self.constraints.iter().map(String::as_str),
        )?)
    }

    fn draws(&self, count: usize, rng: &mut Xoshiro256PlusPlus) -> Mat<f64> {
        let points: Vec<Point> = (0..count).map(|_| (self.draw)(rng)).collect();
        Mat::from_fn(self.variables.len(), count, |row, column| {
            points[column][row]
        })
    }
}

fn continuous(
    names: impl IntoIterator<Item = String>,
    lower: f64,
    upper: f64,
) -> Vec<InputVariable> {
    names
        .into_iter()
        .map(|name| InputVariable::new_continuous(name, lower, upper))
        .collect()
}

fn names(dims: usize) -> impl Iterator<Item = String> {
    (1..=dims).map(|i| format!("x{i}"))
}

fn uniform(rng: &mut Xoshiro256PlusPlus) -> f64 {
    rng.random::<f64>()
}

fn unit_box(
    dims: usize,
    slug: &'static str,
    draw: fn(&mut Xoshiro256PlusPlus) -> Point,
) -> Fixture {
    Fixture {
        slug,
        variables: continuous(names(dims), 0.0, 1.0),
        constraints: Vec::new(),
        draw,
        piece: None,
        counts: &[10, 100],
    }
}

fn box_2d() -> Fixture {
    unit_box(2, "box-2d", |rng| (0..2).map(|_| uniform(rng)).collect())
}

fn box_20d() -> Fixture {
    unit_box(20, "box-20d", |rng| (0..20).map(|_| uniform(rng)).collect())
}

/// Uniform on the disc by `r = √u`: area grows as `r²`. Shrunk by a
/// part in 10¹² so rounding cannot put a point on the strict wall.
fn disc() -> Fixture {
    Fixture {
        slug: "disc",
        variables: continuous(["x".to_owned(), "y".to_owned()], -1.0, 1.0),
        constraints: vec!["x^2 + y^2 < 1".to_owned()],
        draw: |rng| {
            let radius = uniform(rng).sqrt() * (1.0 - 1e-12);
            let angle = std::f64::consts::TAU * uniform(rng);
            vec![radius * angle.cos(), radius * angle.sin()]
        },
        piece: None,
        counts: &[10, 100],
    }
}

/// The disc's marginal CDF in either coordinate: the area left of `x`
/// over `π`.
fn disc_cdf(x: f64) -> f64 {
    0.5 + (x * (1.0 - x * x).sqrt() + x.asin()) / std::f64::consts::PI
}

/// Uniform on the corner simplex `x ≥ 0, Σx ≤ 1`: the first `d` of `d + 1`
/// normalised exponentials, Dirichlet(1, …, 1) with the slack dropped.
fn dirichlet(dims: usize, rng: &mut Xoshiro256PlusPlus) -> Point {
    // `1 - u` is in `(0, 1]`, so the logarithm is finite.
    let exponentials: Vec<f64> = (0..=dims).map(|_| -(1.0 - uniform(rng)).ln()).collect();
    let total: f64 = exponentials.iter().sum();
    exponentials[..dims]
        .iter()
        .map(|e| e / total * (1.0 - 1e-12))
        .collect()
}

fn simplex(
    dims: usize,
    slug: &'static str,
    draw: fn(&mut Xoshiro256PlusPlus) -> Point,
    counts: &'static [usize],
) -> Fixture {
    Fixture {
        slug,
        variables: continuous(names(dims), 0.0, 1.0),
        constraints: vec![format!(
            "{} <= 1",
            names(dims).collect::<Vec<_>>().join(" + ")
        )],
        draw,
        piece: None,
        counts,
    }
}

fn simplex_5d() -> Fixture {
    simplex(5, "simplex-5d", |rng| dirichlet(5, rng), &[10, 100])
}

/// 1/20! of its box: the probe sees nothing, and the design is walked.
fn simplex_20d() -> Fixture {
    simplex(20, "simplex-20d", |rng| dirichlet(20, rng), &[100])
}

/// Each coordinate of the corner simplex is Beta(1, d).
fn simplex_cdf(dims: usize, x: f64) -> f64 {
    1.0 - (1.0 - x).powi(dims as i32)
}

/// `x1 < -0.4` or `x1 > 0.8`, babel having no `or`: pieces of width 0.6 and
/// 0.2, so three quarters of the region is on the left.
fn two_pieces() -> Fixture {
    Fixture {
        slug: "two-pieces",
        variables: vec![
            InputVariable::new_continuous("x1", -1.0, 1.0),
            InputVariable::new_continuous("x2", 0.0, 1.0),
        ],
        constraints: vec!["abs(x1 - 0.2) > 0.6".to_owned()],
        draw: |rng| {
            // Off each wall by a part in 10¹², which the strict `>` needs.
            let along = uniform(rng) * 0.8 * (1.0 - 1e-12);
            let x1 = if along < 0.6 {
                -1.0 + along
            } else {
                0.8 + 1e-12 + (along - 0.6)
            };
            vec![x1, uniform(rng)]
        },
        piece: Some(|point| usize::from(point[0] > 0.0)),
        counts: &[10, 100],
    }
}

const BAND: f64 = 0.001;

/// The band `|x1 - x2| ≤ t` by a shear: `(x1, x2 - x1)` is uniform on
/// `[0, 1] × [-t, t]` before the box clips it, and a shear keeps area. The
/// offset is drawn a thousandth short of the tolerance, so rounding cannot
/// cross it; that trims a thousandth of the band's width.
fn band() -> Fixture {
    Fixture {
        slug: "band",
        variables: continuous(names(2), 0.0, 1.0),
        constraints: vec![format!("x1 == x2 +/- {BAND}")],
        draw: |rng| loop {
            let x1 = uniform(rng);
            let x2 = x1 + BAND * 0.999 * (2.0 * uniform(rng) - 1.0);
            if (0.0..=1.0).contains(&x2) {
                return vec![x1, x2];
            }
        },
        piece: None,
        counts: &[10, 100],
    }
}

const RATIOS: [f64; 3] = [1.0, 2.0, 4.0];

/// `cvg_discrete`'s gearbox at a band of five: each ratio `r` has speeds
/// `[20r, 25r]`, widths 5, 10 and 20, and the uniform measure over levels
/// and speed weighs each piece by its width.
fn gearbox() -> Fixture {
    Fixture {
        slug: "gearbox",
        variables: vec![
            InputVariable::new("ratio", Domain::Levels(RATIOS.to_vec())),
            InputVariable::new_continuous("speed", 0.0, 100.0),
        ],
        constraints: vec![
            "speed / ratio >= 20".to_owned(),
            "speed / ratio <= 25".to_owned(),
        ],
        draw: |rng| {
            let along = uniform(rng) * 35.0;
            let (ratio, offset) = if along < 5.0 {
                (1.0, along)
            } else if along < 15.0 {
                (2.0, along - 5.0)
            } else {
                (4.0, along - 15.0)
            };
            vec![ratio, 20.0 * ratio + offset]
        },
        piece: Some(|point| {
            RATIOS
                .iter()
                .position(|ratio| *ratio == point[0])
                .expect("a gearbox point is at one of its ratios")
        }),
        counts: &[12, 100],
    }
}

fn fixtures() -> Vec<Fixture> {
    vec![
        box_2d(),
        box_20d(),
        disc(),
        simplex_5d(),
        simplex_20d(),
        two_pieces(),
        band(),
        gearbox(),
    ]
}

fn columns(points: &Mat<f64>) -> Vec<Point> {
    (0..points.ncols())
        .map(|column| {
            (0..points.nrows())
                .map(|row| points[(row, column)])
                .collect()
        })
        .collect()
}

// --- The fixtures' own checks: the oracle is checked before it judges --------

#[test]
fn when_a_fixture_is_drawn_exactly_should_land_every_point_in_its_region() -> anyhow::Result<()> {
    for fixture in fixtures() {
        // setup
        let system = fixture.system()?;
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(REFERENCE_SEED);

        // act
        let points = fixture.draws(REFERENCE_COLUMNS, &mut rng);

        // assert
        for point in columns(&points) {
            assert!(
                system.is_feasible(&point, 0.0),
                "{}: the exact sampler drew {point:?}, which the system rejects",
                fixture.slug
            );
        }
    }
    Ok(())
}

/// Kolmogorov–Smirnov of one coordinate's draws against a closed form, at
/// the one-in-a-hundred critical value `1.63 / √m`.
fn assert_marginal(slug: &str, mut values: Vec<f64>, cdf: impl Fn(f64) -> f64) {
    values.sort_by(f64::total_cmp);
    let count = values.len() as f64;
    let statistic = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let below = index as f64 / count;
            let above = (index + 1) as f64 / count;
            (cdf(*value) - below).abs().max((above - cdf(*value)).abs())
        })
        .fold(0.0, f64::max);
    assert!(
        statistic < 1.63 / count.sqrt(),
        "{slug}: KS statistic {statistic} against the closed-form marginal"
    );
}

#[test]
fn when_a_disc_or_simplex_is_drawn_exactly_should_have_its_closed_form_marginals() {
    for (fixture, cdf) in [
        (disc(), (|x| disc_cdf(x)) as fn(f64) -> f64),
        (simplex_5d(), |x| simplex_cdf(5, x)),
        (simplex_20d(), |x| simplex_cdf(20, x)),
    ] {
        // setup
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(REFERENCE_SEED);

        // act
        let points = columns(&fixture.draws(REFERENCE_COLUMNS, &mut rng));

        // assert
        for axis in [0, fixture.variables.len() - 1] {
            assert_marginal(
                fixture.slug,
                points.iter().map(|point| point[axis]).collect(),
                cdf,
            );
        }
    }
}

// --- The measurements ----------------------------------------------------------

/// One design's numbers, reduced to scalars.
struct Scores {
    strata_mean: f64,
    strata_worst: f64,
    levels_worst: f64,
    cl2_ratio: f64,
    min_distance: f64,
    cover: f64,
    missing: usize,
}

fn score(
    reference: &Reference,
    reference_labels: Option<&[usize]>,
    piece: Option<fn(&[f64]) -> usize>,
    design: &Mat<f64>,
) -> Scores {
    let assessed = quality::assess(reference, design.as_ref());
    let strata: Vec<f64> = assessed.strata.iter().flatten().copied().collect();
    let missing = match (reference_labels, piece) {
        (Some(reference_labels), Some(piece)) => {
            let labels: Vec<usize> = columns(design).iter().map(|point| piece(point)).collect();
            quality::allocation(&labels, reference_labels).missing
        }
        (None, None) => 0,
        (Some(_), None) | (None, Some(_)) => unreachable!("labels exist exactly when pieces do"),
    };
    Scores {
        strata_mean: strata.iter().sum::<f64>() / strata.len() as f64,
        strata_worst: strata.iter().copied().fold(f64::INFINITY, f64::min),
        levels_worst: assessed
            .level_shares
            .iter()
            .flatten()
            .copied()
            .fold(0.0, f64::max),
        cl2_ratio: assessed.cl2.ratio,
        min_distance: assessed.min_distance,
        cover: assessed
            .covering_radius
            .expect("a sample reference has a covering radius"),
        missing,
    }
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let values: Vec<f64> = values.collect();
    values.iter().sum::<f64>() / values.len() as f64
}

/// Column widths match [`row`], so the file reads as text and a `git diff`
/// lines up.
const LEDGER_HEADER: &str = "sep=;\nversion                 ;timestamp               ;host                    ;dims ;count ;seeds ;strata-mean ;strata-worst ;levels-worst ;cl2-ratio-mean ;cl2-ratio-worst ;min-dist-mean ;min-dist-worst ;cover-mean ;cover-worst ;missing-worst ;random-cl2-ratio ;random-min-dist ;random-cover ;\n";

/// Every case of `fixture`: ten designs and ten controls per count, scored,
/// printed, recorded, and the controls checked against what random is known
/// to score.
fn measure(fixture: &Fixture) -> anyhow::Result<()> {
    let system = fixture.system()?;
    let reference_points = fixture.draws(
        REFERENCE_COLUMNS,
        &mut Xoshiro256PlusPlus::seed_from_u64(REFERENCE_SEED),
    );
    let reference = Reference::sample(&system, reference_points.as_ref());
    let reference_labels: Option<Vec<usize>> = fixture.piece.map(|piece| {
        columns(&reference_points)
            .iter()
            .map(|point| piece(point))
            .collect()
    });

    for &count in fixture.counts {
        let mut designs = Vec::new();
        for seed in 0..SEEDS {
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
            let region = ConstraintSolver::new()
                .with_proposal_budget(common::PROPOSAL_BUDGET)
                .solve(&system, &mut rng)?;
            let design = region.sample(Mat::zeros(0, 0).as_ref(), count, &mut rng)?;
            for point in columns(&design) {
                assert!(
                    system.is_feasible(&point, 0.0),
                    "{} at seed {seed}: {point:?} is not feasible",
                    fixture.slug
                );
            }
            designs.push(score(
                &reference,
                reference_labels.as_deref(),
                fixture.piece,
                &design,
            ));
        }
        let controls: Vec<Scores> = (0..CONTROL_TRIALS)
            .map(|seed| {
                let control = fixture.draws(
                    count,
                    &mut Xoshiro256PlusPlus::seed_from_u64(CONTROL_SEED ^ seed),
                );
                score(
                    &reference,
                    reference_labels.as_deref(),
                    fixture.piece,
                    &control,
                )
            })
            .collect();

        let design = Summary::of(&designs);
        let control = Summary::of(&controls[..SEEDS as usize]);
        println!(
            "{:<12} n={count:<4} design: {design}\n{:<12} {:<6} random: {control}",
            fixture.slug, "", ""
        );
        record(fixture, count, &design, &control);

        // The controls check the measures, not sojourn.
        let checked = Summary::of(&controls);
        assert!(
            (0.85..=1.15).contains(&checked.cl2_ratio_mean),
            "{} n={count}: random designs average a CL2 ratio of {}, not about one",
            fixture.slug,
            checked.cl2_ratio_mean
        );
        let random_strata = 1.0 - (1.0 - 1.0 / count as f64).powi(count as i32);
        assert!(
            (checked.strata_mean - random_strata).abs() < 0.02,
            "{} n={count}: random designs fill {} of their strata, not about {random_strata}",
            fixture.slug,
            checked.strata_mean
        );
    }
    Ok(())
}

/// Ten designs' scores, as the ledger records them: the mean, and the worst
/// seed in whichever direction is worse for that measure.
struct Summary {
    strata_mean: f64,
    strata_worst: f64,
    levels_worst: f64,
    cl2_ratio_mean: f64,
    cl2_ratio_worst: f64,
    min_distance_mean: f64,
    min_distance_worst: f64,
    cover_mean: f64,
    cover_worst: f64,
    missing_worst: usize,
}

impl Summary {
    fn of(scores: &[Scores]) -> Self {
        Self {
            strata_mean: mean(scores.iter().map(|s| s.strata_mean)),
            strata_worst: scores
                .iter()
                .map(|s| s.strata_worst)
                .fold(f64::INFINITY, f64::min),
            levels_worst: scores.iter().map(|s| s.levels_worst).fold(0.0, f64::max),
            cl2_ratio_mean: mean(scores.iter().map(|s| s.cl2_ratio)),
            cl2_ratio_worst: scores
                .iter()
                .map(|s| s.cl2_ratio)
                .fold(f64::NEG_INFINITY, f64::max),
            min_distance_mean: mean(scores.iter().map(|s| s.min_distance)),
            min_distance_worst: scores
                .iter()
                .map(|s| s.min_distance)
                .fold(f64::INFINITY, f64::min),
            cover_mean: mean(scores.iter().map(|s| s.cover)),
            cover_worst: scores.iter().map(|s| s.cover).fold(0.0, f64::max),
            missing_worst: scores.iter().map(|s| s.missing).max().unwrap_or(0),
        }
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "strata {:.3} (worst {:.3})  levels {:.3}  cl2 {:.3} (worst {:.3})  min-dist {:.4} (worst {:.4})  cover {:.4} (worst {:.4})  missing {}",
            self.strata_mean,
            self.strata_worst,
            self.levels_worst,
            self.cl2_ratio_mean,
            self.cl2_ratio_worst,
            self.min_distance_mean,
            self.min_distance_worst,
            self.cover_mean,
            self.cover_worst,
            self.missing_worst
        )
    }
}

/// One file per fixture and count, upserted on version and host: see
/// `common::record_row`.
fn record(fixture: &Fixture, count: usize, design: &Summary, control: &Summary) {
    let version = env!("CARGO_PKG_VERSION");
    let host = common::host();
    let timestamp = common::timestamp_utc();
    let row = format!(
        "{version:<24};{timestamp:<24};{host:<24};{:<5};{count:<6};{SEEDS:<6};{:<12.4};{:<13.4};{:<13.4};{:<15.4};{:<16.4};{:<14.5};{:<15.5};{:<11.5};{:<12.5};{:<14};{:<17.4};{:<16.5};{:<13.5};",
        fixture.variables.len(),
        design.strata_mean,
        design.strata_worst,
        design.levels_worst,
        design.cl2_ratio_mean,
        design.cl2_ratio_worst,
        design.min_distance_mean,
        design.min_distance_worst,
        design.cover_mean,
        design.cover_worst,
        design.missing_worst,
        control.cl2_ratio_mean,
        control.min_distance_mean,
        control.cover_mean,
    );
    let slug = format!("quality-{}-{count}", fixture.slug);
    if common::record_row(&slug, LEDGER_HEADER, &row) {
        println!("recorded {version} into {}/{slug}.csv", common::LEDGER_DIR);
    }
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: `just quality`")]
fn when_a_box_is_designed_should_record_its_quality() -> anyhow::Result<()> {
    measure(&box_2d())?;
    measure(&box_20d())
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: `just quality`")]
fn when_a_disc_is_designed_should_record_its_quality() -> anyhow::Result<()> {
    measure(&disc())
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: `just quality`")]
fn when_a_simplex_is_designed_should_record_its_quality() -> anyhow::Result<()> {
    measure(&simplex_5d())?;
    measure(&simplex_20d())
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: `just quality`")]
fn when_a_region_in_two_pieces_is_designed_should_record_its_quality() -> anyhow::Result<()> {
    measure(&two_pieces())
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: `just quality`")]
fn when_a_thin_band_is_designed_should_record_its_quality() -> anyhow::Result<()> {
    measure(&band())
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: `just quality`")]
fn when_a_gearbox_is_designed_should_record_its_quality() -> anyhow::Result<()> {
    measure(&gearbox())
}

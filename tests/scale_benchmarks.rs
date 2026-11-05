//! How does `solve` fare on generated systems of thousands of constraints?
//!
//! Every fixture elsewhere has at most a few hundred constraints, and several
//! decisions in `docs/todo.md` were measured at that scale — judging every
//! candidate against the whole system, COBYLA as the local solve, a contraction
//! that sweeps every constraint. This file is the number that says which of
//! them holds at ten thousand, before anything is built on a guess.
//!
//! Run it with `just scale`, release only. In a debug build every case is
//! ignored: the corner at a twentieth of its size still outlived nextest's
//! two-minute watchdog. `--all-targets` compiles the generators in every CI
//! run, which is what keeps them from rotting.
//!
//! # The systems
//!
//! | slug | vars | constraints | shape |
//! |---|---|---|---|
//! | `scale-corner-10k` | 200 | 10 000 | `top_corner_200d`'s `xi > 10.5`, plus 9 800 couplings `xi - xj < 0.25` |
//! | `scale-mutants-10k` | 200 | 10 000 | mutants of a dozen templates, each true at one planted point |
//! | `scale-mutants-200-unplanted` | 50 | 200 | the same mutants, each true at a point of its own |
//!
//! **The corner** is the problem that forced axis moves into the walker,
//! `0.5^200` of its box, made non-trivial: the couplings keep it from being a
//! box, so contraction alone does not finish it, and they are satisfiable
//! together — the diagonal `xi = xj` is in every one.
//!
//! **The mutants** are what a generated formulation looks like when nobody
//! wrote it by hand. A dozen seed templates (a sum, a product, a ratio, a
//! transcendental or two) are mutated one to three times — a variable rebound,
//! a constant perturbed, an operator swapped, a subtree wrapped in a function or
//! grafted onto another template — and each one reads the variables of a
//! window of five, so the system is sparse and banded the way a discretised
//! physical one is. Every mutant's threshold is its value at a planted point,
//! loosened by a margin in a random direction, so the planted point is feasible
//! with room to spare and the region is not empty by construction.
//!
//! **Unplanted**, each mutant is true at a point of its own instead, and two
//! hundred of them over fifty variables almost certainly have no point in
//! common. Nothing knows the answer, so the ledger records what `solve` said
//! and how long it took to say it: a proof, a shrug, or a region.
//!
//! # What a row is
//!
//! One run, not the best of several: a solve here is seconds to minutes, and
//! its time is dominated by which rung of the ladder decided it, which is a
//! function of the seed and not of the scheduler. Everything is seeded, so a
//! second run at the same commit reaches the same outcome by the same path,
//! and the run-to-run noise is the machine's alone — still the ~30% of the
//! throughput ledgers, so a row is not evidence of a change under that.

mod common;

use std::time::{Duration, Instant};

use faer::Mat;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use sojourn::{ConstraintSolver, ConstraintSystem, Infeasibility, InputVariable};

/// Brute force's budget for every case, recorded in the ledger beside the time
/// it bounds.
///
/// Not the library's billion: at ten thousand constraints a proposal is ten
/// thousand evaluations rather than a handful, and a region no rung before
/// brute force could settle would spend hours there to report a shrug. Ten
/// million keeps the slowest outcome inside minutes, and since it is a count
/// the verdict is the same on every machine.
const PROPOSAL_BUDGET: u64 = 10_000_000;

/// Points asked of `sample` once a region is found. Few, because the question
/// is what a design costs per point at this scale, not how good it is.
const SAMPLE_COUNT: usize = 10;

/// A generated system and the ledger it records into.
struct Case {
    slug: &'static str,
    variables: Vec<InputVariable>,
    constraints: Vec<String>,
}

/// `top_corner_200d`, coupled. `xi > 10.5` over `10..11` for every `i`, then
/// `xi - xj < 0.25` for distinct ordered pairs drawn without repetition until
/// there are `total` constraints.
fn corner(variable_count: usize, total: usize, rng: &mut StdRng) -> Case {
    assert!(
        total >= variable_count,
        "the corner's own bounds are {variable_count} constraints"
    );
    let couplings = total - variable_count;
    assert!(
        couplings <= variable_count * (variable_count - 1),
        "{couplings} couplings asked of {variable_count} variables, which have fewer ordered pairs"
    );

    let variables: Vec<InputVariable> = (1..=variable_count)
        .map(|i| InputVariable::new_continuous(format!("x{i}"), 10.0, 11.0))
        .collect();
    let mut constraints: Vec<String> = (1..=variable_count)
        .map(|i| format!("x{i} > 10.5"))
        .collect();

    let mut pairs = std::collections::BTreeSet::new();
    while pairs.len() < couplings {
        let i = rng.random_range(1..=variable_count);
        let j = rng.random_range(1..=variable_count);
        if i != j {
            pairs.insert((i, j));
        }
    }
    // Shuffled rather than in the set's order, so the couplings do not arrive
    // sorted by their first variable — a generated formulation would not.
    let mut pairs: Vec<(usize, usize)> = pairs.into_iter().collect();
    for k in (1..pairs.len()).rev() {
        pairs.swap(k, rng.random_range(0..=k));
    }
    constraints.extend(pairs.iter().map(|(i, j)| format!("x{i} - x{j} < 0.25")));

    assert_eq!(constraints.len(), total);
    Case {
        slug: "scale-corner-10k",
        variables,
        constraints,
    }
}

/// A mutant's expression tree. Every operator is total on the reals — the
/// division is by `1 + abs(..)` and the logarithm of `1 + sqr(..)` — so a
/// mutation can never make a constraint fault where it is evaluated.
#[derive(Clone, Debug)]
enum Expr {
    Var(usize),
    /// Positive, so rendering never has to parenthesise a sign.
    Const(f64),
    Unary(Unary, Box<Expr>),
    Binary(Binary, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug)]
enum Unary {
    Sin,
    Cos,
    Atan,
    Tanh,
    Sqr,
    SqrtAbs,
    LnOnePlusSqr,
}

const UNARIES: [Unary; 7] = [
    Unary::Sin,
    Unary::Cos,
    Unary::Atan,
    Unary::Tanh,
    Unary::Sqr,
    Unary::SqrtAbs,
    Unary::LnOnePlusSqr,
];

#[derive(Clone, Copy, Debug)]
enum Binary {
    Add,
    Sub,
    Mul,
    /// `a / (1 + abs(b))`.
    DivOnePlusAbs,
    Max,
    Min,
}

impl Expr {
    fn render(&self) -> String {
        match self {
            Self::Var(index) => format!("x{}", index + 1),
            Self::Const(value) => format!("{value:.3}"),
            Self::Unary(op, operand) => {
                let a = operand.render();
                match op {
                    Unary::Sin => format!("sin({a})"),
                    Unary::Cos => format!("cos({a})"),
                    Unary::Atan => format!("atan({a})"),
                    Unary::Tanh => format!("tanh({a})"),
                    Unary::Sqr => format!("sqr({a})"),
                    Unary::SqrtAbs => format!("sqrt(abs({a}))"),
                    Unary::LnOnePlusSqr => format!("ln(1 + sqr({a}))"),
                }
            }
            Self::Binary(op, left, right) => {
                let (a, b) = (left.render(), right.render());
                match op {
                    Binary::Add => format!("({a} + {b})"),
                    Binary::Sub => format!("({a} - {b})"),
                    Binary::Mul => format!("({a} * {b})"),
                    Binary::DivOnePlusAbs => format!("({a} / (1 + abs({b})))"),
                    Binary::Max => format!("max({a}, {b})"),
                    Binary::Min => format!("min({a}, {b})"),
                }
            }
        }
    }

    fn size(&self) -> usize {
        match self {
            Self::Var(_) | Self::Const(_) => 1,
            Self::Unary(_, operand) => 1 + operand.size(),
            Self::Binary(_, left, right) => 1 + left.size() + right.size(),
        }
    }

    /// Replaces the `n`th node in preorder with `f` of it.
    fn map_nth(self, n: &mut usize, f: &mut dyn FnMut(Self) -> Self) -> Self {
        if *n == 0 {
            *n = usize::MAX;
            return f(self);
        }
        *n -= 1;
        match self {
            leaf @ (Self::Var(_) | Self::Const(_)) => leaf,
            Self::Unary(op, operand) => Self::Unary(op, Box::new(operand.map_nth(n, f))),
            Self::Binary(op, left, right) => {
                let left = left.map_nth(n, f);
                let right = right.map_nth(n, f);
                Self::Binary(op, Box::new(left), Box::new(right))
            }
        }
    }
}

/// The seed population: a dozen shapes a hand-written constraint takes, over
/// three placeholder variables `a`, `b`, `c` that each mutant binds to its own
/// window.
fn template(index: usize, [a, b, c]: [usize; 3]) -> Expr {
    use Binary::{Add, DivOnePlusAbs, Max, Min, Mul, Sub};
    use Expr::{Const, Var};
    use Unary::{Cos, LnOnePlusSqr, Sin, Sqr, SqrtAbs, Tanh};
    let bin = |op, l, r| Expr::Binary(op, Box::new(l), Box::new(r));
    let un = |op, x| Expr::Unary(op, Box::new(x));
    match index {
        0 => bin(Add, Var(a), Var(b)),
        1 => bin(Sub, bin(Mul, Var(a), Var(b)), Var(c)),
        2 => bin(Add, un(Sqr, Var(a)), un(Sqr, Var(b))),
        3 => bin(Mul, un(Sin, Var(a)), un(Cos, Var(b))),
        4 => bin(DivOnePlusAbs, Var(a), Var(b)),
        5 => un(SqrtAbs, bin(Sub, Var(a), Var(b))),
        6 => bin(Add, un(LnOnePlusSqr, Var(a)), Var(c)),
        7 => bin(Sub, bin(Max, Var(a), Var(b)), bin(Min, Var(b), Var(c))),
        8 => un(Tanh, bin(Add, Var(a), bin(Mul, Const(2.0), Var(b)))),
        9 => bin(Sub, bin(Mul, Const(3.0), Var(a)), bin(Mul, Var(b), Var(c))),
        10 => bin(Add, bin(Mul, Var(a), Var(a)), bin(Mul, Const(0.5), Var(c))),
        11 => bin(Sub, un(Cos, bin(Sub, Var(a), Var(c))), Var(b)),
        _ => unreachable!("there are TEMPLATES templates, and {index} is not one"),
    }
}

const TEMPLATES: usize = 12;

/// How many consecutive variables a mutant may read.
const WINDOW: usize = 5;

/// One mutant over the window starting at a random variable: a template,
/// mutated one to three times.
fn mutant(variable_count: usize, rng: &mut StdRng) -> Expr {
    let base = rng.random_range(0..variable_count.saturating_sub(WINDOW - 1).max(1));
    let window = base..(base + WINDOW).min(variable_count);
    let mut pick = |rng: &mut StdRng| rng.random_range(window.clone());
    let placeholders = |rng: &mut StdRng, pick: &mut dyn FnMut(&mut StdRng) -> usize| {
        [pick(rng), pick(rng), pick(rng)]
    };

    let vars = placeholders(rng, &mut pick);
    let mut expr = template(rng.random_range(0..TEMPLATES), vars);

    for _ in 0..rng.random_range(1..=3) {
        let mut at = rng.random_range(0..expr.size());
        let mutation = rng.random_range(0..6);
        let graft_vars = placeholders(rng, &mut pick);
        let graft = template(rng.random_range(0..TEMPLATES), graft_vars);
        let rebound = pick(rng);
        let factor = rng.random_range(0.5..2.0);
        let wrapper = UNARIES[rng.random_range(0..UNARIES.len())];
        let coin = rng.random_bool(0.5);
        expr = expr.map_nth(&mut at, &mut |node| match mutation {
            // Rebind: a variable becomes another in the window.
            0 => match node {
                Expr::Var(_) => Expr::Var(rebound),
                other => other,
            },
            // Perturb: a constant is scaled.
            1 => match node {
                Expr::Const(value) => Expr::Const(value * factor),
                other => other,
            },
            // Swap: an operator becomes its sibling.
            2 => match node {
                Expr::Binary(op, l, r) => {
                    let swapped = match op {
                        Binary::Add => Binary::Sub,
                        Binary::Sub => Binary::Mul,
                        Binary::Mul => Binary::Add,
                        Binary::Max => Binary::Min,
                        Binary::Min => Binary::Max,
                        Binary::DivOnePlusAbs => Binary::DivOnePlusAbs,
                    };
                    Expr::Binary(swapped, l, r)
                }
                Expr::Unary(op, x) => {
                    let swapped = match op {
                        Unary::Sin => Unary::Cos,
                        Unary::Cos => Unary::Sin,
                        Unary::Atan => Unary::Tanh,
                        Unary::Tanh => Unary::Atan,
                        Unary::Sqr => Unary::SqrtAbs,
                        Unary::SqrtAbs => Unary::Sqr,
                        Unary::LnOnePlusSqr => Unary::LnOnePlusSqr,
                    };
                    Expr::Unary(swapped, x)
                }
                leaf @ (Expr::Var(_) | Expr::Const(_)) => leaf,
            },
            // Wrap: the subtree goes inside a function.
            3 => Expr::Unary(wrapper, Box::new(node)),
            // Graft: the subtree is combined with a fresh template.
            4 => {
                let op = if coin { Binary::Add } else { Binary::Mul };
                Expr::Binary(op, Box::new(node), Box::new(graft.clone()))
            }
            // Scale: the subtree is multiplied by a constant.
            5 => Expr::Binary(Binary::Mul, Box::new(Expr::Const(factor)), Box::new(node)),
            _ => unreachable!("the mutation is drawn from 0..6"),
        });
    }
    expr
}

/// A mutant's value at `point`, by the crate's own evaluator — so the planted
/// threshold is the one `solve` will see, libm and all.
fn value_at(expr: &Expr, names: &[String], point: &[f64]) -> f64 {
    let compiled = sojourn::compile(&expr.render(), names)
        .unwrap_or_else(|e| panic!("mutant {} did not compile: {e:#}", expr.render()));
    let column = Mat::from_fn(point.len(), 1, |row, _| point[row]);
    let value = compiled
        .eval(column.as_ref())
        .unwrap_or_else(|e| panic!("mutant {} did not evaluate: {e:#}", expr.render()))[0];
    assert!(value.is_finite(), "mutant {} is {value}", expr.render());
    value
}

const MUTANT_LOWER: f64 = -2.0;
const MUTANT_UPPER: f64 = 2.0;

/// `total` mutants over `variable_count` variables in `[-2, 2]`, each one
/// true with a margin at a planted point — one point for all of them, or, if
/// `planted` is false, one per mutant.
fn mutants(
    slug: &'static str,
    variable_count: usize,
    total: usize,
    planted: bool,
    rng: &mut StdRng,
) -> Case {
    let names: Vec<String> = (1..=variable_count).map(|i| format!("x{i}")).collect();
    let variables: Vec<InputVariable> = names
        .iter()
        .map(|name| InputVariable::new_continuous(name.clone(), MUTANT_LOWER, MUTANT_UPPER))
        .collect();
    let draw_point = |rng: &mut StdRng| -> Vec<f64> {
        (0..variable_count)
            .map(|_| rng.random_range(MUTANT_LOWER..MUTANT_UPPER))
            .collect()
    };
    let shared = draw_point(rng);

    let constraints: Vec<String> = (0..total)
        .map(|_| {
            let expr = mutant(variable_count, rng);
            let point = if planted {
                shared.clone()
            } else {
                draw_point(rng)
            };
            let value = value_at(&expr, &names, &point);
            // A margin relative to the value's own scale, so a mutant near a
            // thousand is not held to the same hundredth as one near one.
            let margin = rng.random_range(0.02..0.2) * (1.0 + value.abs());
            if rng.random_bool(0.5) {
                format!("{} < {:.6}", expr.render(), value + margin)
            } else {
                format!("{} > {:.6}", expr.render(), value - margin)
            }
        })
        .collect();

    Case {
        slug,
        variables,
        constraints,
    }
}

/// What `solve` concluded, as the ledger spells it.
fn outcome(result: &Result<sojourn::FeasibleRegion, Infeasibility>) -> String {
    match result {
        Ok(_) => "found".to_owned(),
        Err(Infeasibility::Proved { blamed }) => format!("proved/{}", blamed.len()),
        Err(Infeasibility::BetweenLevels { blamed, .. }) => {
            format!("between-levels/{}", blamed.len())
        }
        Err(Infeasibility::NotFound { unexpressed }) => {
            format!("not-found/{}", unexpressed.len())
        }
        // `Infeasibility` is `#[non_exhaustive]`, so from outside the crate a
        // variant added later lands here rather than failing to compile; the
        // ledger says so instead of guessing what it meant.
        Err(other) => format!("unknown: {other}"),
    }
}

struct Measurement {
    slug: &'static str,
    variables: usize,
    constraints: usize,
    compile: Duration,
    solve: Duration,
    outcome: String,
    /// `None` when there was no region to sample.
    sample: Option<Duration>,
}

fn measure(case: Case, seed: u64) -> Measurement {
    let variables = case.variables.len();
    let constraints = case.constraints.len();

    // The engine's own `tracing`, timestamped from the start of the run, on
    // stderr: a solve here takes minutes, and the rung it is spending them
    // in is the finding. `RUST_LOG` overrides the level; `try_init` because
    // every test in the binary calls this.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("sojourn=debug")),
        )
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();

    eprintln!("{}: ConstraintSystem::new", case.slug);
    let started = Instant::now();
    let system = ConstraintSystem::new(case.variables, case.constraints)
        .unwrap_or_else(|e| panic!("{}: the generated system was refused: {e:#}", case.slug));
    let compile = started.elapsed();

    eprintln!("{}: solve, after {compile:?} compiling", case.slug);
    let mut rng = StdRng::seed_from_u64(seed);
    let started = Instant::now();
    let result = ConstraintSolver::new()
        .with_proposal_budget(PROPOSAL_BUDGET)
        .solve(&system, &mut rng);
    let solve = started.elapsed();
    let outcome = outcome(&result);

    let sample = result.ok().map(|region| {
        let started = Instant::now();
        let design = region
            .sample(
                Mat::<f64>::zeros(variables, 0).as_ref(),
                SAMPLE_COUNT,
                &mut rng,
            )
            .unwrap_or_else(|e| panic!("{}: a found region would not sample: {e:#}", case.slug));
        let elapsed = started.elapsed();
        for column in 0..design.ncols() {
            let point: Vec<f64> = (0..variables).map(|row| design[(row, column)]).collect();
            assert!(
                system.is_feasible(&point, 0.0),
                "{}: sample returned an infeasible point",
                case.slug
            );
        }
        elapsed
    });

    Measurement {
        slug: case.slug,
        variables,
        constraints,
        compile,
        solve,
        outcome,
        sample,
    }
}

/// The header every scale ledger carries; widths match the row in
/// [`record`], so the file reads as text and a `git diff` lines up.
const LEDGER_HEADER: &str = "sep=;\nversion                 ;timestamp               ;host                    ;vars ;constraints ;proposal-budget ;compile-ms  ;solve-ms    ;outcome                 ;sample-ms   ;points ;\n";

fn record(m: &Measurement) {
    let version = env!("CARGO_PKG_VERSION");
    let host = common::host();
    let timestamp = common::timestamp_utc();
    let ms = |d: Duration| format!("{:.1}", d.as_secs_f64() * 1000.0);
    let (sample, points) = m.sample.map_or((String::new(), String::new()), |d| {
        (ms(d), SAMPLE_COUNT.to_string())
    });
    let row = format!(
        "{version:<24};{timestamp:<24};{host:<24};{:<5};{:<12};{:<16};{:<12};{:<12};{:<24};{:<12};{:<7};",
        m.variables,
        m.constraints,
        PROPOSAL_BUDGET,
        ms(m.compile),
        ms(m.solve),
        m.outcome,
        sample,
        points,
    );
    if common::record_row(m.slug, LEDGER_HEADER, &row) {
        println!(
            "recorded {version} into {}/{}.csv",
            common::LEDGER_DIR,
            m.slug
        );
    }
}

fn report(m: &Measurement) {
    println!();
    println!("{} ({})", m.slug, common::profile_label());
    println!(
        "  {} variables, {} constraints, proposal budget {PROPOSAL_BUDGET}",
        m.variables, m.constraints
    );
    println!(
        "  ConstraintSystem::new  {:>10.1} ms",
        m.compile.as_secs_f64() * 1000.0
    );
    println!(
        "  solve                  {:>10.1} ms  -> {}",
        m.solve.as_secs_f64() * 1000.0,
        m.outcome
    );
    match m.sample {
        Some(d) => println!(
            "  sample({SAMPLE_COUNT})             {:>10.1} ms",
            d.as_secs_f64() * 1000.0
        ),
        None => println!("  sample                 (no region)"),
    }
}

fn run(case: Case, seed: u64) -> Measurement {
    let measurement = measure(case, seed);
    report(&measurement);
    record(&measurement);
    common::describe_host();
    measurement
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: `just scale`")]
fn when_top_corner_is_coupled_by_ten_thousand_constraints_should_find_it() {
    // setup
    let mut rng = StdRng::seed_from_u64(0x05CA_1EC0);
    let case = corner(200, 10_000, &mut rng);

    // act
    let measurement = run(case, 0x000C_04E7);

    // assert
    assert_eq!(
        measurement.outcome, "found",
        "the diagonal satisfies every coupling, so the region is not empty"
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: `just scale`")]
fn when_ten_thousand_mutants_share_a_planted_point_should_find_a_region() {
    // setup
    let mut rng = StdRng::seed_from_u64(0x003D_7A47);
    let case = mutants("scale-mutants-10k", 200, 10_000, true, &mut rng);

    // act
    let measurement = run(case, 0x003D_7A48);

    // assert
    // The planted point is feasible with margin, so `Proved` would be a
    // soundness bug. A shrug is an honest outcome at this scale and is what
    // the ledger is for.
    assert!(
        !measurement.outcome.starts_with("proved"),
        "a region holding a planted point was proved empty: {}",
        measurement.outcome
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release only: `just scale`")]
fn when_two_hundred_mutants_are_unplanted_should_conclude_something() {
    // setup
    let mut rng = StdRng::seed_from_u64(0x0009_1A47);
    let case = mutants("scale-mutants-200-unplanted", 50, 200, false, &mut rng);

    // act
    let measurement = run(case, 0x0009_1A48);

    // assert
    // Nothing knows the answer; returning at all, inside the budgets, is the
    // property. The outcome is the ledger's.
    assert!(
        !measurement.outcome.starts_with("unknown"),
        "{}",
        measurement.outcome
    );
}

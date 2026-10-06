//! Sojourn — constrained random vector generation over a small expression language, babel.
//!
//! Declare a box and the constraints over it, solve, and design over it:
//!
//! ```no_run
//! use sojourn::{ConstraintSystem, InputVariable};
//!
//! # fn example() -> anyhow::Result<()> {
//! let system = ConstraintSystem::new(
//!     vec![InputVariable::new("x", -2.0, 2.0), InputVariable::new("y", -2.0, 2.0)],
//!     ["x^2 + y^2 < 1", "x + y > 0.5"],
//! )?;
//!
//! // Under the default budgets; for a budget or a strategy list build the
//! // solver yourself, `ConstraintSolver::new()...solve(&system, &mut rng)`.
//! // The generator is yours: entropy here, a seeded one to reproduce a run,
//! // the same one threaded through every call. Bounded by its budgets, all
//! // counts; the region it returns is a value holding the feasible points
//! // the search found.
//! let mut rng = rand::rng();
//! let region = sojourn::solve(&system, &mut rng)?;
//!
//! // A point that is not a sample, brought onto the region at the nearest
//! // feasible point (Euclidean, over box-normalised coordinates), `1e-12`
//! // box widths inside every wall. A function of the region, the point
//! // and the clearance alone.
//! let centre = region.repair(&[0.0, 0.0], 1e-12)?;
//!
//! // A space-filling design: one column per point, one row per variable, in
//! // the order declared, spread away from the points handed in and from
//! // each other. A function of the region, those points, the count and the
//! // generator's state; the same generator again gives the next design.
//! let existing = faer::Mat::from_fn(2, 1, |row, _| centre[row]);
//! let design = region.sample(existing.as_ref(), 9, &mut rng)?;
//! # let _ = design;
//! # Ok(())
//! # }
//! ```
//!
//! One expression can also be compiled and evaluated over a batch on its own,
//! and comes with its gradient wherever every operator in it has a derivative:
//!
//! ```
//! # fn main() -> anyhow::Result<()> {
//! let compiled = sojourn::compile("x1 + x2 > 20 - x3^2", &["x1", "x2", "x3"])?;
//!
//! // One column per sample, one row per variable, in the order given.
//! let samples = faer::Mat::from_fn(3, 2, |row, column| (row + 3 * column) as f64);
//! let residuals = compiled.eval(samples.as_ref())?;
//!
//! // One row per symbol the expression names — `gradient.symbols()` says
//! // which — one column per sample: the constraint's Jacobian.
//! if let Some(gradient) = compiled.gradient() {
//!     let jacobian = gradient.eval(samples.as_ref())?;
//!     assert_eq!(jacobian.nrows(), gradient.symbols().len());
//! }
//! # let _ = residuals;
//! # Ok(())
//! # }
//! ```
//!
//! A document's worth of named expressions — over inputs, over names produced
//! elsewhere, and over each other — is [`compile_system`]: one
//! [`CompiledNode`] per expression, carrying the edges it reads and the row
//! layout its tape takes, checked together for cycles, duplicates and a
//! constraint read as a value, and classified cheap or not. No scheduler
//! comes with it; `tests/dependency_diagram.rs` shows the caller's side.
//!
//! Source text goes in; nothing hands back a syntax tree. Two consumers parse
//! it: the evaluator, which [`compile`]s an expression against a list of
//! variable names and runs it over a batch, and the constrained vector
//! generator, which reads the structure of a set of constraints to search for
//! points that satisfy them — a [`ConstraintSystem`] solved by a
//! [`ConstraintSolver`] into a [`FeasibleRegion`], which designs over the
//! region and repairs a point onto it.
//! `src/README.md` has the picture.
//!
//! Boolean expressions evaluate to a scalar whose *sign* carries the truth
//! value: `<= 0` is true, `> 0` is false. That is the canonical `g(x) <= 0`
//! constraint form, so a violated constraint reports how badly it was violated.
//!
//! # Where is it? — `tracing`
//!
//! Every stage boundary is a [`tracing`] event or span at `debug`: `solve`,
//! `sample` and `repair` open a span each; the walker's burn-in and walk, a
//! design, a contraction and each repair stage report as they go; and every
//! COBYLA run says `cobyla starts` with its dimensions and budget *before* it
//! runs, and reports its evaluations and its reason after. Each COBYLA
//! evaluation is a `trace` event, numbered. So a run that should have
//! finished and has not is located by its last line:
//!
//! ```no_run
//! use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan};
//! tracing_subscriber::fmt()
//!     .with_env_filter(EnvFilter::new("sojourn=trace"))
//!     .with_span_events(FmtSpan::ENTER)
//!     .init();
//! ```
//!
//! The COBYLA in this crate is [`basin`], pure Rust: a stack inside `nlopt`
//! is not this crate's. Every loop in the engine is bounded by a count, never
//! a clock, so a run that does not return is a bug here, and the last line
//! names the stage it is in.

// Crate-private while the shape is still settling; goes public when the
// pluggable rewriter needs it.
mod ast;
mod cvg;
pub mod diagnostics;
mod eval;
mod frontend;
mod nodes;
mod repair;
mod solve;
mod system;

pub(crate) use eval::Schema;
pub(crate) use frontend::{Ast, parse};

pub use eval::{CompiledExpression, CompiledGradient, Compiler, Gradient, compile};
pub use nodes::{CompiledNode, Symbol, compile_system};
pub use repair::RepairError;
pub use solve::{
    ConstraintSolver, DEFAULT_GPU_PROPOSAL_BUDGET, DEFAULT_PROPOSAL_BUDGET, DEFAULT_PRUNE_BUDGET,
    FeasibleRegion, GPU_VARIABLE, GpuTarget, Infeasibility, SampleError,
};
pub use system::{ConstraintRef, ConstraintSystem, InputVariable, Point, SystemError};

/// The `rand` this crate's `solve` and `sample` take their generator through,
/// re-exported so a consumer on another `rand` need not match versions by
/// hand: `sojourn::rand::rng()` for entropy,
/// `sojourn::rand::rngs::Xoshiro256PlusPlus::seed_from_u64(..)` to reproduce.
pub use rand;

// Test plumbing: reachable, undocumented, unpromised. Each exists so that a
// fixture in `tests/` can pin one strategy or measure one stage alone.
#[doc(hidden)]
pub use cvg::sampling::fill_box;

#[cfg(feature = "gpu")]
#[doc(hidden)]
pub use cvg::gpu;

#[doc(hidden)]
pub use solve::{DEFAULT_STRATEGIES, Strategy};

/// Whether `name` is a legal Babel variable name.
///
/// Babel accepts Unicode identifiers, so `π`, `测试` and `☕` are all legal.
#[must_use]
pub fn is_legal_variable_name(name: &str) -> bool {
    !name.is_empty() && frontend::parses_as_variable(name)
}

/// [`ConstraintSolver::solve`] under the defaults.
///
/// # Errors
/// As [`ConstraintSolver::solve`].
pub fn solve<R: rand::Rng + ?Sized>(
    system: &ConstraintSystem,
    rng: &mut R,
) -> Result<FeasibleRegion, Infeasibility> {
    ConstraintSolver::new().solve(system, rng)
}

//! The tape as WGSL: the GPU sieve's kernel, one function per constraint.
//!
//! The third backend over the tape, beside the tile and lane executors. The
//! tape's semantics stay here, in Rust, and the surface syntax lives where
//! it can be read as what it is —
//! askama templates under `templates/wgsl/`, compiled at build time against
//! the views in this module. [`function`] turns a tape into a [`Function`]
//! view, one [`Stmt`] per instruction with its operands already named;
//! `function.wgsl.jinja` renders the statements and `operators.wgsl.jinja` holds the
//! operator table, one macro arm per babel operator. Nothing here writes a
//! brace, a newline or an operator's name.
//!
//! It exists because the kernel comes from a *user's expression at run
//! time*; every compile-Rust-to-GPU project wants the function known at build
//! time, and every driver already ships a JIT for shader source.
//!
//! # What the text promises, and what it does not
//!
//! **The GPU is a sieve, never a judge.** The function computes the residual
//! in `f32` and the harness keeps every candidate whose residual is within
//! [`SIEVE_SLACK`] of feasible; the CPU then re-judges the survivors in `f64`
//! against the real tape. So the text may *miss* a feasible point — a false
//! negative costs hit rate — but nothing it keeps is ever delivered on its
//! say-so. That is the whole reason it can be loose about things the CPU
//! tape is strict about:
//!
//! - A non-finite intermediate is not detected as such. Shader compilers
//!   assume no NaNs (this laptop's driver evaluates `NaN <= 0` as *true*), so
//!   the templates do not rely on propagation: every operator with a domain
//!   — `sqrt`, `ln`, `log`, `acos`, `asin`, division, `%` — is guarded by a
//!   comparison on its finite input, which fast-math respects, and a
//!   candidate outside the domain returns [`FAULT`] exactly as the CPU tape
//!   faults. Overflow to infinity is left to arithmetic: `inf` compares
//!   sanely, and an `inf - inf` NaN is a false positive the CPU re-check
//!   removes. `Check` emits nothing.
//! - A constant outside `f32` range becomes `±f32::MAX`. The CPU tape faults
//!   on the overflow such a constant produces; the sieve drops the candidate
//!   for a different reason and the outcome is the same.
//! - `pow` with a negative base is NaN on the GPU even for an integer
//!   exponent, where the CPU's `powf` is not. The tape lowers a whole exponent
//!   into multiplications and the shader is rendered from the tape, so this
//!   only reaches a real or run-time exponent, where the CPU is NaN too.
//! - `max`/`min` do not carry Java's signed-zero rule. Irrelevant to a sign.
//!
//! **A subscript out of range is a fault**, and a fault is a candidate the
//! constraint does not hold for: the function returns [`FAULT`], a residual
//! nothing keeps.

// Built and tested with or without the `gpu` feature — naga validates the text
// on any machine — but only the sieve calls it.
#![cfg_attr(
    not(feature = "gpu"),
    allow(
        dead_code,
        reason = "the emitter's only production caller is the GPU sieve"
    )
)]

use askama::Template;

use crate::ast::{BinaryOp, CompareOp, UnaryOp};

use super::RowLayout;
use super::tape::{Accumulate, AllocatedTape, Instruction};

/// How far past feasible, relative to the magnitudes compared, the sieve still
/// keeps a candidate.
///
/// `f32` carries 24 bits, so one rounding is about `6e-8` relative; a tape of
/// a few dozen operations and a transcendental or two on the special function
/// units accumulates a few hundred of those at most. A hundredth of a percent
/// is generous — the survivors are re-judged exactly, so generosity costs a
/// little CPU and nothing else, where meanness costs hits. The test that
/// pins it draws a hundred thousand candidates per problem and requires that
/// nothing feasible was dropped and that survivors stay within a small
/// multiple of the feasible count.
pub(crate) const SIEVE_SLACK: f32 = 1e-4;

/// The residual a function returns for a candidate the CPU would fault on.
/// The largest finite `f32`, so it survives `max` with anything finite and
/// fails `<= slack` with everything. Not infinity: WGSL has no literal for it.
pub(crate) const FAULT: f32 = f32::MAX;

/// The helpers every emitted function calls — `templates/wgsl/prelude.wgsl.jinja`.
/// Rendered once per shader, before any function.
#[derive(Template)]
#[template(path = "wgsl/prelude.wgsl.jinja", escape = "none")]
pub(crate) struct Prelude {
    pub(crate) slack: String,
    pub(crate) fault: String,
}

impl Prelude {
    pub(crate) fn new() -> Self {
        Self {
            slack: literal(f64::from(SIEVE_SLACK)),
            fault: literal(f64::from(FAULT)),
        }
    }
}

/// One constraint's tape, in the shape `templates/wgsl/function.wgsl.jinja`
/// renders: a function over a pointer to the candidate's coordinates,
/// returning the f32 residual.
#[derive(Template)]
#[template(path = "wgsl/function.wgsl.jinja", escape = "none")]
pub(crate) struct Function {
    pub(crate) name: String,
    /// The row: the length of the array the function takes.
    pub(crate) rows: usize,
    /// How many of the row a computed subscript may name — the layout's
    /// inputs, equal to `rows` for every tape the sieve runs today, since a
    /// constraint's row is exactly the box; the guard reads this and not the
    /// array length.
    pub(crate) subscriptable: usize,
    /// Size of the local register array.
    pub(crate) registers: u16,
    /// Register and its literal, already spelled: number formatting is
    /// Rust's job, syntax is the template's.
    pub(crate) consts: Vec<(usize, String)>,
    pub(crate) body: Vec<Stmt>,
    pub(crate) result: usize,
}

/// One tape instruction with its operands named — `r[k]` strings from
/// [`reg`], the one place register spelling lives on this side.
pub(crate) enum Stmt {
    /// Register, input row.
    Load(usize, u32),
    /// Destination, source.
    Copy(usize, usize),
    Unary(usize, UnaryOp, String),
    Binary(usize, BinaryOp, String, String),
    /// Already oriented: `lower - higher` is the `<= 0` residual, whichever
    /// way the comparison was written.
    Compare(usize, String, String),
    Combine(usize, Accumulate, String, String),
    /// Destination, the register holding the one-based subscript.
    Gather(usize, String),
}

/// The tape as a [`Function`] named `name` over a row laid out as `layout`.
pub(crate) fn function(tape: &AllocatedTape, name: &str, layout: RowLayout) -> Function {
    let consts = tape
        .consts
        .iter()
        .enumerate()
        .map(|(index, constant)| (index, literal(*constant)))
        .collect();

    let body = tape
        .insns
        .iter()
        .filter_map(|insn| match *insn {
            Instruction::Load { dst, input } => Some(Stmt::Load(dst.index(), input)),
            Instruction::Copy { dst, src } => Some(Stmt::Copy(dst.index(), src.index())),
            Instruction::Unary { dst, op, a } => Some(Stmt::Unary(dst.index(), op, reg(a.index()))),
            Instruction::Binary { dst, op, a, b } => Some(Stmt::Binary(
                dst.index(),
                op,
                reg(a.index()),
                reg(b.index()),
            )),
            Instruction::Compare { dst, op, a, b } => {
                let (a, b) = (reg(a.index()), reg(b.index()));
                let (lower, higher) = match op {
                    CompareOp::Lte | CompareOp::Lt => (a, b),
                    CompareOp::Gte | CompareOp::Gt => (b, a),
                };
                Some(Stmt::Compare(dst.index(), lower, higher))
            }
            Instruction::Combine { dst, how, a, b, .. } => Some(Stmt::Combine(
                dst.index(),
                how,
                reg(a.index()),
                reg(b.index()),
            )),
            // A non-finite value is guarded at the operator that could
            // produce it; there is nothing left to check.
            Instruction::Check { .. } => None,
            Instruction::Gather { dst, index, .. } => {
                Some(Stmt::Gather(dst.index(), reg(index.index())))
            }
        })
        .collect();

    Function {
        name: name.to_owned(),
        rows: layout.total(),
        subscriptable: layout.inputs,
        registers: tape.registers.max(1),
        consts,
        body,
        result: tape.result.index(),
    }
}

fn reg(index: usize) -> String {
    format!("r[{index}]")
}

/// An `f64` constant as an `f32` WGSL literal. Beyond `f32` range it becomes
/// `±f32::MAX`; see the module docs for why that is the same outcome.
fn literal(value: f64) -> String {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the sieve is f32 by design; precision loss is the documented trade"
    )]
    let narrowed = value as f32;
    let finite = if narrowed.is_infinite() {
        f32::MAX.copysign(narrowed)
    } else {
        narrowed
    };
    format!("{finite:?}")
}

#[cfg(test)]
mod tests {
    //! No GPU here: the text is validated by naga, wgpu's own shader
    //! front end, which checks syntax, types, arities and scoping. Whether
    //! the function *computes* the right residual is the sieve's test, which
    //! needs an adapter.

    use askama::Template;

    use super::{Prelude, RowLayout, function};
    use crate::Schema;

    /// Constraints that between them use every instruction: the three rung
    /// families, a fault, a gather, an equality, an aggregate, and a mix of
    /// the transcendentals the GPU is there for.
    const CORPUS: &[&str] = &[
        "x1 > 0.9995",
        "x1^2 + x2^2 + x3^2 < 0.0001",
        "sin(x1) > sin(0.9995)",
        "sqrt(x1 - 5) + x1 < 6",
        "var[ceil(x2 * 2)] > 0.5",
        "x1 == pi +/- 0.001",
        "x1 == x2 +/- 0.5 implies (x3 > 0 implies x1 < x3)",
        "sum(1, 3, i -> var[i] * var[i]) < 1.5",
        "tanh(x3) - sinh(x1) + cosh(x2) < 0.5",
        "abs(floor(x1) - ceil(x2)) < 2",
        "acos(x1) + asin(x2) + atan(x3) + tan(x1) + cos(x2) > 1",
    ];

    /// One source per operator, so that every arm of the operator table is
    /// rendered and validated. The template's `match` is exhaustive — a new
    /// operator without an arm does not compile — so this is about the
    /// spelling being WGSL naga accepts, not about coverage.
    const OPERATORS: &[&str] = &[
        "-x1 < 0.5",
        "cos(x1) < 0.5",
        "sin(x1) < 0.5",
        "tan(x1) < 0.5",
        "acos(x1) < 0.5",
        "asin(x1) < 0.5",
        "atan(x1) < 0.5",
        "cosh(x1) < 0.5",
        "sinh(x1) < 0.5",
        "tanh(x1) < 0.5",
        "cot(x1) < 0.5",
        "ln(x1) < 0.5",
        "log(x1) < 0.5",
        "abs(x1) < 0.5",
        "sqrt(x1) < 0.5",
        "cbrt(x1) < 0.5",
        "sqr(x1) < 0.5",
        "cube(x1) < 0.5",
        "ceil(x1) < 0.5",
        "floor(x1) < 0.5",
        "sgn(x1) < 0.5",
        "x1 + x2 < 0.5",
        "x1 - x2 < 0.5",
        "x1 * x2 < 0.5",
        "x1 / x2 < 0.5",
        "x1 % x2 < 0.5",
        "x1 ^ x2 < 0.5",
        "max(x1, x2) < 0.5",
        "min(x1, x2) < 0.5",
        "log(x1, x2) < 0.5",
        "sum(1, 3, i -> var[i]) < 0.5",
        "prod(1, 3, i -> var[i]) < 0.5",
    ];

    fn schema() -> Schema {
        Schema::new(["x1", "x2", "x3"])
    }

    fn shader(sources: &[&str]) -> String {
        let schema = schema();
        let mut text = Prelude::new().render().expect("the prelude renders");
        for (index, source) in sources.iter().enumerate() {
            let ast = crate::parse(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
            let compiled = crate::eval::bind(&ast, &schema, crate::eval::Gradient::Never)
                .unwrap_or_else(|e| panic!("{source:?}: {e}"));
            text.push('\n');
            text.push_str(
                &function(
                    &compiled.tape,
                    &format!("c{index}"),
                    RowLayout {
                        inputs: 3,
                        intermediates: 0,
                    },
                )
                .render()
                .unwrap_or_else(|e| panic!("{source:?}: {e}")),
            );
        }
        text
    }

    fn validate(text: &str) {
        let module = naga::front::wgsl::parse_str(text)
            .unwrap_or_else(|e| panic!("{}\n---\n{text}", e.emit_to_string(text)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}\n---\n{text}"));
    }

    #[test]
    fn every_corpus_constraint_emits_a_shader_naga_accepts() {
        validate(&shader(CORPUS));
    }

    #[test]
    fn each_constraint_validates_alone_too() {
        // So a failure names the constraint rather than the corpus.
        for source in CORPUS {
            validate(&shader(&[source]));
        }
    }

    #[test]
    fn every_operator_has_a_spelling_naga_accepts() {
        for source in OPERATORS {
            validate(&shader(&[source]));
        }
    }

    #[test]
    fn a_constant_beyond_f32_becomes_the_largest_finite_and_still_validates() {
        let text = shader(&["x1 * 1.0e300 < 1"]);
        assert!(text.contains("3.4028235e38"), "{text}");
        assert!(!text.contains("inf"), "{text}");
        validate(&text);
    }

    /// The rendered function reads as the tape, in the tape's order: the
    /// domain guard before the operator that needs it, the operator, the
    /// slack on the comparison, the result.
    #[test]
    fn the_rendered_function_is_what_the_tape_says() {
        let text = shader(&["sqrt(x1 - 5) + x1 < 6"]);
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle:?} missing from\n{text}"))
        };
        let guard = at("< 0.0 {");
        let fault = at("return EXPR_FAULT;");
        let root = at("= sqrt(r[");
        let slack = at("- expr_slack(r[");
        let result = at("return r[");
        assert!(guard < fault && fault < root, "guard, fault, sqrt: {text}");
        assert!(
            root < slack && slack < result,
            "sqrt, slack, return: {text}"
        );
        validate(&text);
    }

    #[test]
    fn a_comparison_carries_the_slack_and_a_gather_can_fault() {
        let text = shader(&["var[ceil(x2 * 2)] > 0.5"]);
        assert!(text.contains("expr_slack("), "{text}");
        assert!(text.contains("return EXPR_FAULT;"), "{text}");
        assert!(text.contains("let gi = i32(r["), "{text}");
    }
}

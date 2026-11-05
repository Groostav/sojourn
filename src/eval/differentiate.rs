//! Reverse-mode differentiation as a tape-to-tape transformation.
//!
//! # What reverse mode is
//!
//! A tape computes `f` as a straight line of `dst = op(a, b)`. Every value on
//! it has an *adjoint*, `d̄ = ∂f/∂dst`: how much `f` moves when that value
//! does. The result's adjoint is one. Walking the tape backwards, each
//! instruction hands its adjoint down to its operands scaled by the
//! operator's local derivative — `ā += d̄ · ∂op/∂a`, `b̄ += d̄ · ∂op/∂b` —
//! and when the walk reaches the `Load`s, their adjoints are `∂f/∂xⱼ` for
//! every input at once. The cost is a fixed multiple of one evaluation of
//! `f`, whatever the number of inputs, which is why this and not a partial
//! per variable: the beam's two-hundred-variable deflection constraint gets
//! its whole gradient for about three evaluations, and a product of a
//! hundred terms costs a hundred multiplications, not the ten thousand the
//! product rule spells out.
//!
//! # Why on the tape, before allocation
//!
//! The tape is the one artifact every backend is a lowering of, so the
//! derivative rules live beside the instruction set they differentiate — one
//! arm per operator, the same table shape as the WGSL kernel and the interval
//! evaluator. It runs on the executors as they are: the derivative tape *is*
//! a tape, batched over a tile, judged, faultable, with the primal value as
//! its result and the partials as further outputs.
//!
//! Before allocation, because the allocator packs temporaries and a physical
//! register then holds different values at different instructions; an adjoint
//! per physical register would be wrong. Before it, almost every temporary is
//! one value. The exception is a fold's accumulator, which `lower` writes once
//! per term, and a `let` slot a fold lands in; so the sweep first *renames*
//! every write to a fresh temporary, reads resolving to the latest version,
//! and works over that single-assignment form.
//!
//! # What has no derivative
//!
//! `floor`, `ceil`, `sgn` and `%` have a derivative of zero almost everywhere
//! and a jump where it matters, which says nothing about where a boundary is;
//! a computed subscript reads a coordinate the point chooses. A tape holding
//! any of them is declined outright — `None` — and the caller falls back to
//! something that does not need a gradient. Kinks are different: `abs`,
//! `max`, `min`, and the `Worst` fold a conjunction or a tolerance lowers to
//! have a derivative everywhere but on a set of measure zero, and the tape
//! spells it with its own `sgn`: the adjoint of `max(a, b)` goes to `a` with
//! weight `(1 + sgn(a − b)) / 2` and to `b` with the rest, both halves at the
//! kink. A Newton step from a kink is a subgradient step, and the judged
//! landing decides.
//!
//! Every instruction the sweep emits is a checked one, at the span of the
//! primal instruction it derives from: a gradient that is not finite is a
//! fault where the primal was fine — `1/b` where `b` reached zero — and it is
//! reported against the subexpression, like any other.

use std::collections::HashMap;

use crate::ast::{BinaryOp, CompareOp, UnaryOp};
use crate::diagnostics::Span;

use super::tape::{Accumulate, Instruction, VirtualRegister, VirtualTape};

/// The derivative tape of `primal`: its instructions, renamed to single
/// assignment, followed by the reverse sweep. `result` is the primal value;
/// `partials[s]` is `∂f/∂xₛ` for the `s`th entry of `positions`, the
/// expression's symbols as row positions. A symbol the tape never loads has
/// the zero constant for its partial.
///
/// `None` when the tape holds an instruction with no derivative.
pub(crate) fn differentiate(primal: &VirtualTape, positions: &[u32]) -> Option<VirtualTape> {
    if primal.insns.iter().any(|insn| !differentiable(insn)) {
        return None;
    }

    // Forward: the primal in single assignment, copied as it stands. The
    // derivative tape shares its constant pool and continues its temporary
    // numbering.
    let renamed = primal.single_assignment();
    let mut sweep = Sweep {
        tape: VirtualTape::new(
            renamed.consts.clone(),
            renamed.locals,
            renamed.temps(),
            Vec::new(),
            Vec::new(),
            renamed.result,
        ),
        span: Span::new(0, 0),
    };
    let mut loads: Vec<(u32, VirtualRegister)> = Vec::new();
    for (insn, span) in renamed.insns.iter().zip(&renamed.spans) {
        if let Instruction::Load { dst, input } = *insn {
            loads.push((input, dst));
        }
        sweep.span = *span;
        sweep.emit(*insn);
    }
    let result = renamed.result;

    // Backward: adjoints, from the result down to the loads. This is
    // "reverse-mode automatic differentiation" — backpropagation — as in
    // Griewank & Walther, *Evaluating Derivatives* (2008), over a tape whose
    // every value is written once, so one adjoint register per value is
    // exact and an adjoint is complete by the time the sweep reaches its
    // instruction, because every use of a value comes later in the tape.
    let mut adjoint: HashMap<VirtualRegister, VirtualRegister> = HashMap::new();
    let one = sweep.constant(1.0);
    adjoint.insert(result, one);
    for (insn, span) in renamed.insns.iter().zip(&renamed.spans).rev() {
        let Some(dst) = insn.dst() else {
            continue;
        };
        let Some(d) = adjoint.get(&dst).copied() else {
            continue;
        };
        sweep.span = *span;
        for (source, contribution) in sweep.pushed_down(insn, d) {
            if matches!(source, VirtualRegister::Const(_)) {
                continue;
            }
            let total = match adjoint.get(&source) {
                Some(&sofar) => sweep.binary(BinaryOp::Add, sofar, contribution),
                None => contribution,
            };
            adjoint.insert(source, total);
        }
    }

    let zero = sweep.constant(0.0);
    let partials = positions
        .iter()
        .map(|position| {
            loads
                .iter()
                .find(|(input, _)| input == position)
                .and_then(|(_, dst)| adjoint.get(dst).copied())
                .unwrap_or(zero)
        })
        .collect();

    let mut tape = sweep.tape;
    tape.result = result;
    tape.partials = partials;
    Some(tape)
}

/// Whether an instruction has a derivative the sweep can spell.
fn differentiable(insn: &Instruction<VirtualRegister>) -> bool {
    match insn {
        Instruction::Unary { op, .. } => {
            !matches!(op, UnaryOp::Ceil | UnaryOp::Floor | UnaryOp::Sgn)
        }
        Instruction::Binary { op, .. } => *op != BinaryOp::Rem,
        Instruction::Gather { .. } => false,
        Instruction::Load { .. }
        | Instruction::Copy { .. }
        | Instruction::Compare { .. }
        | Instruction::Combine { .. }
        | Instruction::Check { .. } => true,
    }
}

/// The tape under construction: the primal, then the sweep's instructions.
struct Sweep {
    tape: VirtualTape,
    /// The span every emitted instruction takes: the primal instruction's.
    span: Span,
}

impl Sweep {
    fn emit(&mut self, insn: Instruction<VirtualRegister>) {
        self.tape.insns.push(insn);
        self.tape.spans.push(self.span);
    }

    fn constant(&mut self, value: f64) -> VirtualRegister {
        self.tape.consts.intern(value)
    }

    fn unary(&mut self, op: UnaryOp, a: VirtualRegister) -> VirtualRegister {
        let dst = self.tape.fresh_temp();
        self.emit(Instruction::Unary { dst, op, a });
        dst
    }

    fn binary(&mut self, op: BinaryOp, a: VirtualRegister, b: VirtualRegister) -> VirtualRegister {
        let dst = self.tape.fresh_temp();
        self.emit(Instruction::Binary { dst, op, a, b });
        dst
    }

    fn scaled(&mut self, a: VirtualRegister, by: f64) -> VirtualRegister {
        let c = self.constant(by);
        self.binary(BinaryOp::Mul, a, c)
    }

    /// `(1 ± sgn(a − b)) / 2`: the share of an adjoint the larger (`+`) or
    /// smaller (`−`) of two operands receives, halves at a tie.
    fn share(&mut self, a: VirtualRegister, b: VirtualRegister, larger: bool) -> VirtualRegister {
        let difference = self.binary(BinaryOp::Sub, a, b);
        let sign = self.unary(UnaryOp::Sgn, difference);
        let one = self.constant(1.0);
        let shifted = if larger {
            self.binary(BinaryOp::Add, one, sign)
        } else {
            self.binary(BinaryOp::Sub, one, sign)
        };
        self.scaled(shifted, 0.5)
    }

    /// `d̄` pushed through `insn` to each operand: `(operand, d̄ · ∂op/∂operand)`.
    fn pushed_down(
        &mut self,
        insn: &Instruction<VirtualRegister>,
        d: VirtualRegister,
    ) -> Vec<(VirtualRegister, VirtualRegister)> {
        match *insn {
            Instruction::Load { .. } | Instruction::Check { .. } | Instruction::Gather { .. } => {
                Vec::new()
            }
            Instruction::Copy { src, .. } => vec![(src, d)],
            Instruction::Unary { dst: y, op, a } => vec![(a, self.through_unary(op, a, y, d))],
            Instruction::Binary { dst: y, op, a, b } => self.through_binary(op, a, b, y, d),
            Instruction::Compare { op, a, b, .. } => {
                // The residual is `a - b` or `b - a`, plus a constant nudge.
                let negated = self.unary(UnaryOp::Negate, d);
                match op {
                    CompareOp::Lte | CompareOp::Lt => vec![(a, d), (b, negated)],
                    CompareOp::Gte | CompareOp::Gt => vec![(a, negated), (b, d)],
                }
            }
            Instruction::Combine { how, a, b, .. } => match how {
                Accumulate::Sum => vec![(a, d), (b, d)],
                Accumulate::Prod => {
                    let to_a = self.binary(BinaryOp::Mul, d, b);
                    let to_b = self.binary(BinaryOp::Mul, d, a);
                    vec![(a, to_a), (b, to_b)]
                }
                Accumulate::Worst => self.through_extremum(a, b, true, d),
                Accumulate::Best => self.through_extremum(a, b, false, d),
            },
        }
    }

    /// `d̄ · ∂op(a)/∂a`, with `y = op(a)` in hand where it shortens the rule.
    fn through_unary(
        &mut self,
        op: UnaryOp,
        a: VirtualRegister,
        y: VirtualRegister,
        d: VirtualRegister,
    ) -> VirtualRegister {
        match op {
            UnaryOp::Negate => self.unary(UnaryOp::Negate, d),
            UnaryOp::Sqr => {
                let by_a = self.binary(BinaryOp::Mul, d, a);
                self.scaled(by_a, 2.0)
            }
            UnaryOp::Cube => {
                let a2 = self.unary(UnaryOp::Sqr, a);
                let by_a2 = self.binary(BinaryOp::Mul, d, a2);
                self.scaled(by_a2, 3.0)
            }
            UnaryOp::Sqrt => {
                let two_y = self.scaled(y, 2.0);
                self.binary(BinaryOp::Div, d, two_y)
            }
            UnaryOp::Cbrt => {
                let y2 = self.unary(UnaryOp::Sqr, y);
                let three_y2 = self.scaled(y2, 3.0);
                self.binary(BinaryOp::Div, d, three_y2)
            }
            UnaryOp::Ln => self.binary(BinaryOp::Div, d, a),
            UnaryOp::Log10 => {
                let a_ln10 = self.scaled(a, std::f64::consts::LN_10);
                self.binary(BinaryOp::Div, d, a_ln10)
            }
            UnaryOp::Sin => {
                let cos = self.unary(UnaryOp::Cos, a);
                self.binary(BinaryOp::Mul, d, cos)
            }
            UnaryOp::Cos => {
                let sin = self.unary(UnaryOp::Sin, a);
                let by_sin = self.binary(BinaryOp::Mul, d, sin);
                self.unary(UnaryOp::Negate, by_sin)
            }
            UnaryOp::Tan | UnaryOp::Cot => {
                // `1 + y²`, negated for the cotangent.
                let y2 = self.unary(UnaryOp::Sqr, y);
                let one = self.constant(1.0);
                let sec2 = self.binary(BinaryOp::Add, one, y2);
                let by_sec2 = self.binary(BinaryOp::Mul, d, sec2);
                if op == UnaryOp::Tan {
                    by_sec2
                } else {
                    self.unary(UnaryOp::Negate, by_sec2)
                }
            }
            UnaryOp::Asin | UnaryOp::Acos => {
                // `±1 / sqrt(1 − a²)`.
                let a2 = self.unary(UnaryOp::Sqr, a);
                let one = self.constant(1.0);
                let inside = self.binary(BinaryOp::Sub, one, a2);
                let root = self.unary(UnaryOp::Sqrt, inside);
                let over = self.binary(BinaryOp::Div, d, root);
                if op == UnaryOp::Asin {
                    over
                } else {
                    self.unary(UnaryOp::Negate, over)
                }
            }
            UnaryOp::Atan => {
                let a2 = self.unary(UnaryOp::Sqr, a);
                let one = self.constant(1.0);
                let inside = self.binary(BinaryOp::Add, one, a2);
                self.binary(BinaryOp::Div, d, inside)
            }
            UnaryOp::Sinh => {
                let cosh = self.unary(UnaryOp::Cosh, a);
                self.binary(BinaryOp::Mul, d, cosh)
            }
            UnaryOp::Cosh => {
                let sinh = self.unary(UnaryOp::Sinh, a);
                self.binary(BinaryOp::Mul, d, sinh)
            }
            UnaryOp::Tanh => {
                let y2 = self.unary(UnaryOp::Sqr, y);
                let one = self.constant(1.0);
                let sech2 = self.binary(BinaryOp::Sub, one, y2);
                self.binary(BinaryOp::Mul, d, sech2)
            }
            UnaryOp::Abs => {
                let sign = self.unary(UnaryOp::Sgn, a);
                self.binary(BinaryOp::Mul, d, sign)
            }
            UnaryOp::Ceil | UnaryOp::Floor | UnaryOp::Sgn => {
                unreachable!("`differentiable` refused the tape")
            }
        }
    }

    /// `d̄` pushed through `max(a, b)` (`larger`) or `min(a, b)`: to whichever
    /// operand is selected, halves at a tie.
    fn through_extremum(
        &mut self,
        a: VirtualRegister,
        b: VirtualRegister,
        larger: bool,
        d: VirtualRegister,
    ) -> Vec<(VirtualRegister, VirtualRegister)> {
        let share_a = self.share(a, b, larger);
        let share_b = self.share(a, b, !larger);
        let to_a = self.binary(BinaryOp::Mul, d, share_a);
        let to_b = self.binary(BinaryOp::Mul, d, share_b);
        vec![(a, to_a), (b, to_b)]
    }

    /// `d̄` pushed through `y = a op b` to `a` and `b`.
    fn through_binary(
        &mut self,
        op: BinaryOp,
        a: VirtualRegister,
        b: VirtualRegister,
        y: VirtualRegister,
        d: VirtualRegister,
    ) -> Vec<(VirtualRegister, VirtualRegister)> {
        match op {
            BinaryOp::Add => vec![(a, d), (b, d)],
            BinaryOp::Sub => {
                let negated = self.unary(UnaryOp::Negate, d);
                vec![(a, d), (b, negated)]
            }
            BinaryOp::Mul => {
                let to_a = self.binary(BinaryOp::Mul, d, b);
                let to_b = self.binary(BinaryOp::Mul, d, a);
                vec![(a, to_a), (b, to_b)]
            }
            BinaryOp::Div => {
                // `∂/∂a = 1/b`, `∂/∂b = −a/b² = −y/b`.
                let to_a = self.binary(BinaryOp::Div, d, b);
                let y_over_b = self.binary(BinaryOp::Div, y, b);
                let by = self.binary(BinaryOp::Mul, d, y_over_b);
                let to_b = self.unary(UnaryOp::Negate, by);
                vec![(a, to_a), (b, to_b)]
            }
            BinaryOp::Pow => {
                // `∂/∂a = b · a^(b−1)`; `∂/∂b = y · ln a`, only where the
                // exponent is not a constant — the tape knows, and a constant
                // exponent is the polynomial case where `ln a` would fault on
                // a negative base the power itself is fine with.
                let one = self.constant(1.0);
                let b_less_one = self.binary(BinaryOp::Sub, b, one);
                let a_pow = self.binary(BinaryOp::Pow, a, b_less_one);
                let b_a_pow = self.binary(BinaryOp::Mul, b, a_pow);
                let to_a = self.binary(BinaryOp::Mul, d, b_a_pow);
                let mut pushed = vec![(a, to_a)];
                if !matches!(b, VirtualRegister::Const(_)) {
                    let ln_a = self.unary(UnaryOp::Ln, a);
                    let y_ln_a = self.binary(BinaryOp::Mul, y, ln_a);
                    let to_b = self.binary(BinaryOp::Mul, d, y_ln_a);
                    pushed.push((b, to_b));
                }
                pushed
            }
            BinaryOp::Max | BinaryOp::Min => self.through_extremum(a, b, op == BinaryOp::Max, d),
            BinaryOp::LogB => {
                // `y = ln b / ln a`: `∂/∂b = 1 / (b ln a)`, `∂/∂a = −y / (a ln a)`.
                let ln_a = self.unary(UnaryOp::Ln, a);
                let b_ln_a = self.binary(BinaryOp::Mul, b, ln_a);
                let to_b = self.binary(BinaryOp::Div, d, b_ln_a);
                let a_ln_a = self.binary(BinaryOp::Mul, a, ln_a);
                let y_over = self.binary(BinaryOp::Div, y, a_ln_a);
                let by = self.binary(BinaryOp::Mul, d, y_over);
                let to_a = self.unary(UnaryOp::Negate, by);
                vec![(a, to_a), (b, to_b)]
            }
            BinaryOp::Rem => unreachable!("`differentiable` refused the tape"),
        }
    }
}

#[cfg(test)]
mod tests {
    use rand::RngExt;
    use rand::SeedableRng;
    use rand::rngs::Xoshiro256PlusPlus;

    use super::super::{CompiledGradient, Gradient, Schema, bind};

    /// The expression and its gradient, bound to `names`.
    fn gradient_of(source: &str, names: &[&str]) -> Option<CompiledGradient> {
        let ast = crate::parse(source).unwrap_or_else(|e| panic!("{source:?}: {e}"));
        bind(&ast, &Schema::for_names(names), Gradient::BestEffort)
            .unwrap_or_else(|e| panic!("{source:?} against {names:?}: {e:?}"))
            .gradient()
            .cloned()
    }

    /// Central finite difference of the compiled expression itself, so the
    /// sweep is judged against the executor's own arithmetic.
    fn finite_difference(source: &str, names: &[&str], at: &[f64], coordinate: usize) -> f64 {
        let ast = crate::parse(source).expect("compiles");
        let compiled = bind(&ast, &Schema::for_names(names), Gradient::Never).expect("binds");
        let h = 1e-6 * at[coordinate].abs().max(1.0);
        let mut up = at.to_vec();
        up[coordinate] += h;
        let mut down = at.to_vec();
        down[coordinate] -= h;
        let f = |point: &[f64]| compiled.eval_row(point).expect("evaluates");
        (f(&up) - f(&down)) / (2.0 * h)
    }

    /// Every partial against the finite difference at `at`, to a relative
    /// tolerance that a central difference at `h = 1e-6` earns. Partials are
    /// per *symbol* of the expression, in its own order, so each is matched
    /// to its schema position by name.
    fn agrees(source: &str, names: &[&str], at: &[f64]) {
        let gradient = gradient_of(source, names).unwrap_or_else(|| panic!("{source:?} declined"));
        let partials = gradient
            .eval_row(at)
            .unwrap_or_else(|e| panic!("{source:?} at {at:?}: {e}"));
        assert_eq!(partials.len(), gradient.symbols().len(), "{source:?}");
        for (symbolic, name) in partials.iter().zip(gradient.symbols()) {
            let coordinate = names
                .iter()
                .position(|candidate| candidate == name)
                .expect("a symbol is a schema name");
            let numeric = finite_difference(source, names, at, coordinate);
            let symbolic = *symbolic;
            let scale = numeric.abs().max(symbolic.abs()).max(1.0);
            assert!(
                (numeric - symbolic).abs() <= 1e-6 * scale,
                "{source:?}: d/d{name} at {at:?} is {symbolic}, finite difference says {numeric}"
            );
        }
    }

    fn rng() -> Xoshiro256PlusPlus {
        Xoshiro256PlusPlus::seed_from_u64(0xD1FF)
    }

    /// Every operator with a derivative, at points inside its domain.
    #[test]
    fn every_operator_agrees_with_a_finite_difference() {
        let mut rng = rng();
        let cases: &[(&str, f64, f64)] = &[
            ("-x", -3.0, 3.0),
            ("x + y", -3.0, 3.0),
            ("x - y", -3.0, 3.0),
            ("x * y", -3.0, 3.0),
            ("x / y", 0.5, 3.0),
            ("x ^ 3", -3.0, 3.0),
            ("x ^ 2.5", 0.5, 3.0),
            ("x ^ y", 0.5, 3.0),
            ("2 ^ x", -3.0, 3.0),
            ("max(x, y)", -3.0, 3.0),
            ("min(x, y)", -3.0, 3.0),
            ("log(x, y)", 1.5, 4.0),
            ("sqr(x) + cube(y)", -3.0, 3.0),
            ("sqrt(x) * cbrt(y)", 0.5, 4.0),
            ("ln(x) + log(y)", 0.5, 4.0),
            ("sin(x) * cos(y) + tan(x)", -1.0, 1.0),
            ("cot(x) + atan(y)", 0.5, 1.4),
            ("asin(x) - acos(y)", -0.9, 0.9),
            ("sinh(x) + cosh(y) * tanh(x)", -2.0, 2.0),
            ("abs(x) * y", -3.0, 3.0),
            ("sum(1, 3, i -> var[i] ^ 2)", -3.0, 3.0),
            ("prod(1, 3, i -> var[i] + 1)", 0.5, 3.0),
            ("var x = x1 * x2; x + sqr(x) * x3", -2.0, 2.0),
            ("x1 * x1 + x1", -3.0, 3.0),
        ];
        for (source, lo, hi) in cases {
            let names: Vec<&str> = if source.contains("x1") || source.contains("var[") {
                vec!["x1", "x2", "x3"]
            } else {
                vec!["x", "y"]
            };
            for _ in 0..3 {
                let at: Vec<f64> = (0..names.len())
                    .map(|_| rng.random_range(*lo..*hi))
                    .collect();
                agrees(source, &names, &at);
            }
        }
    }

    /// A constraint's gradient is the gradient of its residual, in the
    /// residual's own sign convention: `a < b` is `a - b`, `a > b` is
    /// `b - a`, a tolerance is the worst of its two sides.
    #[test]
    fn a_constraint_differentiates_as_its_residual() {
        let mut rng = rng();
        for source in [
            "x + 2 * y < 1",
            "x + 2 * y > 1",
            "x^2 + y^2 <= 1",
            "x^2 + y^2 >= 1",
            "x == y + 1 +/- 0.01",
            "x * y == 1 +/- 0.1",
        ] {
            for _ in 0..4 {
                let at = [rng.random_range(-2.0..2.0), rng.random_range(-2.0..2.0)];
                agrees(source, &["x", "y"], &at);
            }
        }
    }

    /// A symbol read three times accumulates every use, and the partials are
    /// per symbol the expression names — `y` is in the schema and not here.
    #[test]
    fn a_reused_symbol_accumulates_every_use() {
        let gradient = gradient_of("x * x + x", &["x", "y"]).expect("differentiable");
        assert_eq!(gradient.symbols(), ["x".to_owned()]);
        let partials = gradient.eval_row(&[3.0, 7.0]).expect("evaluates");
        assert_eq!(partials, vec![7.0]);
    }

    /// The instructions without a derivative decline the whole tape.
    #[test]
    fn a_jump_declines() {
        for source in [
            "floor(x) < 3",
            "x % 2 < 1",
            "sgn(x) * y",
            "ceil(x) + y",
            "var[floor(y)] < 3",
        ] {
            assert!(
                gradient_of(source, &["x", "y"]).is_none(),
                "{source:?} should have declined"
            );
        }
    }

    /// The primal value comes with the gradient, so a solver that wants both
    /// pays one run.
    #[test]
    fn the_primal_rides_along() {
        let gradient = gradient_of("x^2 + y^2 < 1", &["x", "y"]).expect("differentiable");
        let (value, partials) = gradient
            .eval_row_with_value(&[0.6, 0.8])
            .expect("evaluates");
        assert!((value - 0.0).abs() < 1e-12, "{value}");
        assert!((partials[0] - 1.2).abs() < 1e-12 && (partials[1] - 1.6).abs() < 1e-12);
    }

    /// Reverse mode's promise: the derivative tape is a fixed multiple of the
    /// primal, whatever the number of inputs.
    #[test]
    fn the_sweep_is_a_fixed_multiple_of_the_primal() {
        let names: Vec<String> = (1..=200).map(|i| format!("x{i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let source = "sum(1, 200, i -> var[i] ^ 2) < 1";
        let ast = crate::parse(source).expect("compiles");
        let compiled = bind(&ast, &Schema::for_names(&names), Gradient::BestEffort).expect("binds");
        let gradient = compiled.gradient().expect("differentiable");
        let primal = compiled.tape().insns.len();
        let sweep = gradient.tape().insns.len();
        assert!(
            sweep <= 6 * primal,
            "{sweep} instructions for a primal of {primal}"
        );
    }

    /// A fault in the sweep is a fault at the primal's span: `ln x` differentiates
    /// to `1 / x`, which is not finite at zero even though the value there is.
    #[test]
    fn a_sweep_fault_names_the_subexpression() {
        let gradient = gradient_of("sqrt(x) + y", &["x", "y"]).expect("differentiable");
        let failure = gradient
            .eval_row(&[0.0, 1.0])
            .expect_err("1 / (2 sqrt 0) is not finite");
        let text = failure.to_string();
        assert!(text.contains("sqrt(x)"), "{text}");
    }

    /// The same batch, one column at a time and all at once, agree.
    #[test]
    fn the_batched_jacobian_matches_the_rows() {
        let gradient = gradient_of("x * y + sin(x)", &["x", "y"]).expect("differentiable");
        let points = [[0.5, 1.5], [-1.0, 2.0], [3.0, -0.5]];
        let samples = faer::Mat::from_fn(2, points.len(), |row, column| points[column][row]);
        let jacobian = gradient.eval(samples.as_ref()).expect("evaluates");
        for (column, point) in points.iter().enumerate() {
            let partials = gradient.eval_row(point).expect("evaluates");
            for (row, partial) in partials.iter().enumerate() {
                assert_eq!(jacobian[(row, column)], *partial);
            }
        }
    }

    /// Determinism: the same source lowers to the same derivative tape.
    #[test]
    fn the_sweep_is_deterministic() {
        let once = gradient_of("x * y + sin(x)", &["x", "y"]).expect("differentiable");
        let twice = gradient_of("x * y + sin(x)", &["x", "y"]).expect("differentiable");
        assert_eq!(once.tape().insns, twice.tape().insns);
    }
}

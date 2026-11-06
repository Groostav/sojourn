//! IR generation: `ast::Program` → [`VirtualTape`], one post-order walk
//! emitting instructions in exactly the order the tree-walker evaluated
//! nodes, so that a fault lands on the same span and a fold accumulates in
//! the same order. The name is Clang's for the same step (`CodeGen`, or
//! "IRGen": the front end's tree walked once, instructions emitted into a
//! function); the tape is this crate's IR.
//!
//! Register classes are decided here and packed by [`super::regalloc`]:
//! literals become constant registers, `LocalSlot`s become local registers
//! one-to-one, and everything else is a temporary.

use crate::ast::{BinaryOp, Block, CompareOp, Expr, Kind, Program};
use crate::diagnostics::Span;

use super::tape::{Accumulate, Constants, Instruction, VirtualRegister, VirtualTape};

/// Emits `program`, with `global_positions[GlobalId]` giving each symbol's
/// row, to the virtual tape — which [`VirtualTape::allocate`] turns into the
/// one the executors run and `differentiate` turns into its gradient's.
pub(crate) fn emit(program: &Program, global_positions: &[u32]) -> VirtualTape {
    let mut emitter = Emitter {
        global_positions,
        consts: Constants::default(),
        next_temp: 0,
        insns: Vec::new(),
        spans: Vec::new(),
        loads: Vec::new(),
        assigned: vec![false; program.frame_size as usize],
    };
    let result = emitter.block(&program.body, None);
    emitter.finish(result, program.frame_size)
}

struct Emitter<'a> {
    global_positions: &'a [u32],
    consts: Constants,
    next_temp: u32,
    insns: Vec<Instruction<VirtualRegister>>,
    spans: Vec<Span>,
    /// Row position → register already holding it. A non-finite input faults
    /// at its *first* read, which is the cached `Load`'s span; later reads of
    /// the same variable need no instruction.
    loads: Vec<(u32, VirtualRegister)>,
    /// Which local slots are definitely written by the time they are read.
    /// A read the emitter cannot prove gets a `Check`, preserving the walker's
    /// NaN-sentinel semantics for a front-end slot bug.
    assigned: Vec<bool>,
}

impl Emitter<'_> {
    fn emit(&mut self, insn: Instruction<VirtualRegister>, span: Span) {
        self.insns.push(insn);
        self.spans.push(span);
    }

    fn temp(&mut self) -> VirtualRegister {
        let t = VirtualRegister::Temp(self.next_temp);
        self.next_temp += 1;
        t
    }

    fn constant(&mut self, value: f64) -> VirtualRegister {
        self.consts.intern(value)
    }

    /// Delivers a value that already lives in `value` to `dst` if one was
    /// asked for. Only a leaf ever needs this: an instruction-producing node
    /// writes `dst` directly.
    fn place(
        &mut self,
        value: VirtualRegister,
        dst: Option<VirtualRegister>,
        span: Span,
    ) -> VirtualRegister {
        match dst {
            Some(dst) if dst != value => {
                self.emit(Instruction::Copy { dst, src: value }, span);
                dst
            }
            _ => value,
        }
    }

    fn load(&mut self, input: u32, span: Span) -> VirtualRegister {
        if let Some(&(_, reg)) = self.loads.iter().find(|(i, _)| *i == input) {
            return reg;
        }
        let dst = self.temp();
        self.emit(Instruction::Load { dst, input }, span);
        self.loads.push((input, dst));
        dst
    }

    fn local(slot: crate::ast::LocalSlot) -> VirtualRegister {
        VirtualRegister::Local(u16::try_from(slot.index()).expect("fewer than 65536 locals"))
    }

    fn block(&mut self, block: &Block, dst: Option<VirtualRegister>) -> VirtualRegister {
        for assignment in &block.assignments {
            let slot = Self::local(assignment.slot);
            self.expr(&assignment.value, Some(slot));
            self.assigned[assignment.slot.index()] = true;
        }
        self.expr(&block.result, dst)
    }

    /// Lowers `expr`, into `dst` if given, and returns the register holding
    /// the value.
    fn expr(&mut self, expr: &Expr, dst: Option<VirtualRegister>) -> VirtualRegister {
        let span = expr.span;
        match &expr.kind {
            Kind::Literal(value) => {
                let c = self.constant(*value);
                self.place(c, dst, span)
            }
            Kind::Global(id) => {
                let reg = self.load(self.global_positions[id.index()], span);
                self.place(reg, dst, span)
            }
            Kind::Local(slot) => {
                let reg = Self::local(*slot);
                if !self.assigned[slot.index()] {
                    self.emit(Instruction::Check { reg }, span);
                }
                self.place(reg, dst, span)
            }
            // Only a computed subscript reaches here: a literal one became a
            // `Global` when `bind` resolved it against the schema.
            Kind::DynamicIndex(subscript) => {
                let index = self.expr(subscript, None);
                let dst = dst.unwrap_or_else(|| self.temp());
                self.emit(
                    Instruction::Gather {
                        dst,
                        index,
                        subscript: subscript.span,
                    },
                    span,
                );
                dst
            }
            Kind::Unary { op, arg } => {
                let a = self.expr(arg, None);
                let dst = dst.unwrap_or_else(|| self.temp());
                self.emit(Instruction::Unary { dst, op: *op, a }, span);
                dst
            }
            // A whole power reaches here as the multiplications
            // `rewrite::unroll_powers` made of it; what is still a `Pow` is a
            // real or variable exponent, and goes through `powf`.
            Kind::Binary { op, lhs, rhs } => {
                let a = self.expr(lhs, None);
                let b = self.expr(rhs, None);
                let dst = dst.unwrap_or_else(|| self.temp());
                self.emit(Instruction::Binary { dst, op: *op, a, b }, span);
                dst
            }
            Kind::Compare { op, lhs, rhs } => {
                let a = self.expr(lhs, None);
                let b = self.expr(rhs, None);
                let dst = dst.unwrap_or_else(|| self.temp());
                self.emit(Instruction::Compare { dst, op: *op, a, b }, span);
                dst
            }
            // `a == b +/- t` is the conjunction it has always meant:
            // `a >= b - t` and `a <= b + t`, worst residual winning. It used to
            // be one fused instruction, which cost every executor an arm — the
            // tree-walker, the SIMD kernel, the tiled path and WGSL — to say
            // what four instructions the backends already had say between them.
            //
            // The arithmetic is unchanged, node for node and in the same order:
            // `Compare::Gte` is `right - left` and `Compare::Lte` is
            // `left - right`, so this computes `max((b - t) - a, a - (b + t))`
            // exactly as the fused version did. `corpus.rs` pins that by value.
            Kind::NearEq {
                lhs,
                rhs,
                tolerance,
            } => {
                let (a, floor, ceiling) = self.band(lhs, rhs, *tolerance, span);

                let at_least = self.temp();
                self.emit(
                    Instruction::Compare {
                        dst: at_least,
                        op: CompareOp::Gte,
                        a,
                        b: floor,
                    },
                    span,
                );
                let at_most = self.temp();
                self.emit(
                    Instruction::Compare {
                        dst: at_most,
                        op: CompareOp::Lte,
                        a,
                        b: ceiling,
                    },
                    span,
                );

                let dst = dst.unwrap_or_else(|| self.temp());
                self.emit(
                    Instruction::Combine {
                        dst,
                        how: Accumulate::Worst,
                        a: at_least,
                        b: at_most,
                        last: true,
                    },
                    span,
                );
                dst
            }
            Kind::Fold { kind, terms } => {
                self.fold(Accumulate::from_aggregate(*kind), terms, dst, span)
            }
            Kind::And { terms } => self.fold(Accumulate::Worst, terms, dst, span),
            Kind::Or { terms } => self.fold(Accumulate::Best, terms, dst, span),
            // `min(not premise, conclusion)`: true where the premise fails or
            // the conclusion holds. The premise is negated by `negated`, which
            // complements comparisons rather than negating a residual.
            Kind::Implies {
                premise,
                conclusion,
            } => {
                let unless = self.negated(premise);
                let then = self.expr(conclusion, None);
                let dst = dst.unwrap_or_else(|| self.temp());
                self.emit(
                    Instruction::Combine {
                        dst,
                        how: Accumulate::Best,
                        a: unless,
                        b: then,
                        last: true,
                    },
                    span,
                );
                dst
            }
            Kind::Block(block) => self.block(block, dst),
            Kind::Aggregate { .. } => {
                unreachable!("`unroll_aggregates` turns every aggregate into a `Fold` or fails")
            }
        }
    }

    /// `a == b +/- t`'s operands and bounds: `a`, then `b - t` and `b + t`,
    /// computed once in that order. What `NearEq` compares `a` against, and
    /// what its negation compares it against from the other side.
    fn band(
        &mut self,
        lhs: &Expr,
        rhs: &Expr,
        tolerance: f64,
        span: Span,
    ) -> (VirtualRegister, VirtualRegister, VirtualRegister) {
        let a = self.expr(lhs, None);
        let b = self.expr(rhs, None);
        let tolerance = self.constant(tolerance);

        let floor = self.temp();
        self.emit(
            Instruction::Binary {
                dst: floor,
                op: BinaryOp::Sub,
                a: b,
                b: tolerance,
            },
            span,
        );
        let ceiling = self.temp();
        self.emit(
            Instruction::Binary {
                dst: ceiling,
                op: BinaryOp::Add,
                a: b,
                b: tolerance,
            },
            span,
        );
        (a, floor, ceiling)
    }

    /// The residual of `not expr`, for a boolean `expr`: the premise of an
    /// implication.
    ///
    /// **By complementing each comparison, never by negating a residual.**
    /// `-r` is wrong at a tie: `x < y` is `x - y + ε`, and its negation `x >=
    /// y` is `y - x`, where `-(x - y + ε)` is `y - x - ε`, true where it
    /// should be false. And the rest of the crate assumes a residual enters
    /// a constraint increasingly: `cvg::hc4` leaves the `ε` out of its
    /// forward intervals and the GPU sieve subtracts a slack from every
    /// comparison, both of which loosen a residual and would *tighten* a
    /// negated one, dropping feasible points. Complemented comparisons and
    /// De Morgan keep every residual under monotone folds, so both stay
    /// supersets.
    fn negated(&mut self, expr: &Expr) -> VirtualRegister {
        let span = expr.span;
        match &expr.kind {
            Kind::Compare { op, lhs, rhs } => {
                let a = self.expr(lhs, None);
                let b = self.expr(rhs, None);
                let dst = self.temp();
                self.emit(
                    Instruction::Compare {
                        dst,
                        op: complement(*op),
                        a,
                        b,
                    },
                    span,
                );
                dst
            }
            // Outside the band: `a < b - t` or `a > b + t`.
            Kind::NearEq {
                lhs,
                rhs,
                tolerance,
            } => {
                let (a, floor, ceiling) = self.band(lhs, rhs, *tolerance, span);
                let below = self.temp();
                self.emit(
                    Instruction::Compare {
                        dst: below,
                        op: CompareOp::Lt,
                        a,
                        b: floor,
                    },
                    span,
                );
                let above = self.temp();
                self.emit(
                    Instruction::Compare {
                        dst: above,
                        op: CompareOp::Gt,
                        a,
                        b: ceiling,
                    },
                    span,
                );
                let dst = self.temp();
                self.emit(
                    Instruction::Combine {
                        dst,
                        how: Accumulate::Best,
                        a: below,
                        b: above,
                        last: true,
                    },
                    span,
                );
                dst
            }
            // An `and` in a premise, written or `invert_monotone`'s domain
            // guard: some term fails.
            Kind::And { terms } => self.chain(
                Accumulate::Best,
                terms.len(),
                |this, k| this.negated(&terms[k]),
                None,
                span,
            ),
            // An `or` in a premise: every term fails.
            Kind::Or { terms } => self.chain(
                Accumulate::Worst,
                terms.len(),
                |this, k| this.negated(&terms[k]),
                None,
                span,
            ),
            // A bracketed implication as a premise: `p and not q`.
            Kind::Implies {
                premise,
                conclusion,
            } => {
                let holds = self.expr(premise, None);
                let fails = self.negated(conclusion);
                let dst = self.temp();
                self.emit(
                    Instruction::Combine {
                        dst,
                        how: Accumulate::Worst,
                        a: holds,
                        b: fails,
                        last: true,
                    },
                    span,
                );
                dst
            }
            Kind::Literal(_)
            | Kind::Global(_)
            | Kind::Local(_)
            | Kind::DynamicIndex(_)
            | Kind::Unary { .. }
            | Kind::Binary { .. }
            | Kind::Aggregate { .. }
            | Kind::Fold { .. }
            | Kind::Block(_) => unreachable!(
                "a premise is a `booleanPrimary` by the grammar, and the rewrites keep a boolean boolean"
            ),
        }
    }

    fn fold(
        &mut self,
        how: Accumulate,
        terms: &[Expr],
        dst: Option<VirtualRegister>,
        span: Span,
    ) -> VirtualRegister {
        self.chain(
            how,
            terms.len(),
            |this, k| this.expr(&terms[k], None),
            dst,
            span,
        )
    }

    /// Left to right from the identity, one `Combine` per term, checked only
    /// on the last — the walker checked the fold's value, not each step.
    ///
    /// `term` lowers the `k`th operand when the chain reaches it rather than
    /// beforehand: lowering them all first would keep every operand's register
    /// live across the whole chain, and a two-hundred-term sum would want two
    /// hundred registers.
    fn chain(
        &mut self,
        how: Accumulate,
        count: usize,
        mut term: impl FnMut(&mut Self, usize) -> VirtualRegister,
        dst: Option<VirtualRegister>,
        span: Span,
    ) -> VirtualRegister {
        let identity = self.constant(how.identity());
        if count == 0 {
            return self.place(identity, dst, span);
        }
        let acc = dst.unwrap_or_else(|| self.temp());
        let last = count - 1;
        for k in 0..count {
            let b = term(self, k);
            let a = if k == 0 { identity } else { acc };
            self.emit(
                Instruction::Combine {
                    dst: acc,
                    how,
                    a,
                    b,
                    last: k == last,
                },
                span,
            );
        }
        acc
    }

    fn finish(self, result: VirtualRegister, frame_size: u32) -> VirtualTape {
        VirtualTape::new(
            self.consts,
            u16::try_from(frame_size).expect("fewer than 65536 locals"),
            self.next_temp,
            self.insns,
            self.spans,
            result,
        )
    }
}

/// The comparison that holds exactly where `op` does not: `<` against `>=`,
/// `<=` against `>`. Strictness flips with the direction, which is what
/// puts the `ε` nudge on the right side of a tie.
const fn complement(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::Lt => CompareOp::Gte,
        CompareOp::Lte => CompareOp::Gt,
        CompareOp::Gt => CompareOp::Lte,
        CompareOp::Gte => CompareOp::Lt,
    }
}

#[cfg(test)]
mod tests {
    //! Lowering: the instruction sequence a source produces.

    use crate::ast::{BinaryOp, Block, CompareOp, Expr, Kind, LocalSlot, Program, UnaryOp};
    use crate::diagnostics::Span;

    use super::super::tape::{Accumulate, AllocatedTape, Instruction, Register};
    use super::super::tape_for;

    const R: fn(u16) -> Register = Register;

    fn loads(tape: &AllocatedTape, input: u32) -> usize {
        tape.insns
            .iter()
            .filter(|i| matches!(i, Instruction::Load { input: x, .. } if *x == input))
            .count()
    }

    #[test]
    fn a_sum_of_two_globals_lowers_to_two_loads_and_an_add() {
        let tape = tape_for("x1 + x2", &["x1", "x2"]);
        assert_eq!(
            tape.insns,
            vec![
                Instruction::Load {
                    dst: R(0),
                    input: 0
                },
                Instruction::Load {
                    dst: R(1),
                    input: 1
                },
                Instruction::Binary {
                    dst: R(2),
                    op: BinaryOp::Add,
                    a: R(0),
                    b: R(1)
                },
            ]
        );
        assert_eq!(tape.result, R(2));
    }

    #[test]
    fn a_repeated_global_is_loaded_once() {
        let tape = tape_for("x1 * x1", &["x1"]);
        assert_eq!(loads(&tape, 0), 1);
        assert_eq!(
            tape.insns[1],
            Instruction::Binary {
                dst: R(1),
                op: BinaryOp::Mul,
                a: R(0),
                b: R(0)
            }
        );
    }

    #[test]
    fn a_literal_is_a_constant_register_and_no_instruction() {
        let tape = tape_for("3 + 4", &[]);
        assert!(tape.insns.is_empty(), "{:?}", tape.insns);
        assert_eq!(tape.consts, vec![7.0]);
        assert_eq!(tape.result, R(0));

        let tape = tape_for("x1 + 2", &["x1"]);
        assert_eq!(tape.consts, vec![2.0]);
        // Constants come first, so the load lands in register 1.
        assert_eq!(
            tape.insns[0],
            Instruction::Load {
                dst: R(1),
                input: 0
            }
        );
    }

    #[test]
    fn equal_constants_share_a_register_but_signed_zeros_do_not() {
        let tape = tape_for("x1 + 2 + 2", &["x1"]);
        assert_eq!(tape.consts, vec![2.0]);

        let tape = tape_for("x1 + 0.0 - -0.0", &["x1"]);
        assert_eq!(tape.consts.len(), 2, "{:?}", tape.consts);
        assert_ne!(tape.consts[0].to_bits(), tape.consts[1].to_bits());
    }

    #[test]
    fn a_valid_literal_subscript_is_a_plain_load() {
        let tape = tape_for("var[2] + x2", &["x1", "x2", "x3"]);
        assert!(
            !tape
                .insns
                .iter()
                .any(|i| matches!(i, Instruction::Gather { .. }))
        );
        // `var[2]` and `x2` are the same row, so one load serves both.
        assert_eq!(loads(&tape, 1), 1);
        assert_eq!(tape.consts, Vec::<f64>::new());
    }

    /// What is left for a gather: a subscript the point decides, which says
    /// how it rounds.
    #[test]
    fn a_computed_subscript_is_a_gather() {
        let tape = tape_for("var[floor(x1)]", &["x1", "x2"]);
        assert_eq!(
            tape.insns,
            vec![
                Instruction::Load {
                    dst: R(0),
                    input: 0
                },
                Instruction::Unary {
                    dst: R(1),
                    op: UnaryOp::Floor,
                    a: R(0)
                },
                // The load is dead once floored, so its register is reused.
                Instruction::Gather {
                    dst: R(0),
                    index: R(1),
                    subscript: Span::new(4, 13)
                }
            ]
        );
    }

    #[test]
    fn a_strict_comparison_is_one_compare_instruction() {
        let tape = tape_for("x1 < x2", &["x1", "x2"]);
        assert_eq!(
            tape.insns[2],
            Instruction::Compare {
                dst: R(2),
                op: CompareOp::Lt,
                a: R(0),
                b: R(1)
            }
        );
        // The nudge is inside the instruction, not a constant.
        assert!(tape.consts.is_empty());
    }

    /// `a == b +/- t` lowers to the conjunction it means, not to an
    /// instruction of its own: bracket `b` by the tolerance, compare `a`
    /// against each end, and take the worse residual.
    ///
    /// Five instructions where there was one. That is the trade — every
    /// executor loses an arm, and `corpus.rs` pins that the arithmetic is
    /// unchanged to the bit.
    #[test]
    fn a_near_equality_lowers_to_two_comparisons_and_a_conjunction() {
        let tape = tape_for("x1 == x2 +/- 0.5", &["x1", "x2"]);
        assert_eq!(tape.consts, vec![0.5]);

        let ops: Vec<_> = tape.insns.iter().skip(2).collect();
        assert!(
            matches!(
                ops.as_slice(),
                [
                    Instruction::Binary {
                        op: crate::ast::BinaryOp::Sub,
                        ..
                    },
                    Instruction::Binary {
                        op: crate::ast::BinaryOp::Add,
                        ..
                    },
                    Instruction::Compare {
                        op: crate::ast::CompareOp::Gte,
                        ..
                    },
                    Instruction::Compare {
                        op: crate::ast::CompareOp::Lte,
                        ..
                    },
                    Instruction::Combine {
                        how: Accumulate::Worst,
                        last: true,
                        ..
                    },
                ]
            ),
            "{ops:?}"
        );
    }

    /// `p implies q` is `Best(not p, q)`, and `not p` is the complemented
    /// comparison, never a negated residual: `x1 > x2` becomes `x1 <= x2`.
    #[test]
    fn when_lowering_an_implication_should_complement_the_premise_under_a_disjunction() {
        // act
        let tape = tape_for("x1 > x2 implies x1 < 1", &["x1", "x2"]);

        // assert
        let ops: Vec<_> = tape
            .insns
            .iter()
            .filter(|insn| !matches!(insn, Instruction::Load { .. }))
            .collect();
        assert!(
            matches!(
                ops.as_slice(),
                [
                    Instruction::Compare {
                        op: crate::ast::CompareOp::Lte,
                        ..
                    },
                    Instruction::Compare {
                        op: crate::ast::CompareOp::Lt,
                        ..
                    },
                    Instruction::Combine {
                        how: Accumulate::Best,
                        last: true,
                        ..
                    },
                ]
            ),
            "{ops:?}"
        );
        assert!(
            !tape
                .insns
                .iter()
                .any(|insn| matches!(insn, Instruction::Unary { .. })),
            "nothing is negated"
        );
    }

    /// De Morgan through `or`: `not (p or q)` is `not p and not q`, each
    /// comparison complemented and folded by `Worst` from its identity.
    #[test]
    fn when_lowering_an_or_premise_should_complement_each_term_under_a_conjunction() {
        // act
        let tape = tape_for("x1 > 0 or x2 > 0 implies x1 < 1", &["x1", "x2"]);

        // assert
        let ops: Vec<_> = tape
            .insns
            .iter()
            .filter(|insn| !matches!(insn, Instruction::Load { .. }))
            .collect();
        assert!(
            matches!(
                ops.as_slice(),
                [
                    Instruction::Compare {
                        op: crate::ast::CompareOp::Lte,
                        ..
                    },
                    Instruction::Combine {
                        how: Accumulate::Worst,
                        last: false,
                        ..
                    },
                    Instruction::Compare {
                        op: crate::ast::CompareOp::Lte,
                        ..
                    },
                    Instruction::Combine {
                        how: Accumulate::Worst,
                        last: true,
                        ..
                    },
                    Instruction::Compare {
                        op: crate::ast::CompareOp::Lt,
                        ..
                    },
                    Instruction::Combine {
                        how: Accumulate::Best,
                        last: true,
                        ..
                    },
                ]
            ),
            "{ops:?}"
        );
    }

    #[test]
    fn a_fold_seeds_from_the_identity_and_checks_only_its_last_step() {
        let tape = tape_for("sum(1, 3, i -> x1 * i)", &["x1"]);
        let identity = tape
            .consts
            .iter()
            .position(|c| c.to_bits() == 0.0f64.to_bits())
            .expect("the identity is a constant");
        let identity = R(u16::try_from(identity).unwrap());
        let combines: Vec<(Register, Register, bool)> = tape
            .insns
            .iter()
            .filter_map(|i| match *i {
                Instruction::Combine {
                    dst,
                    how: Accumulate::Sum,
                    a,
                    last,
                    ..
                } => Some((dst, a, last)),
                _ => None,
            })
            .collect();
        assert_eq!(combines.len(), 3);
        let acc = combines[0].0;
        assert_eq!(combines[0].1, identity, "the first step reads the identity");
        assert!(combines[1..].iter().all(|(d, a, _)| *d == acc && *a == acc));
        assert_eq!(
            combines.iter().map(|c| c.2).collect::<Vec<_>>(),
            [false, false, true]
        );
    }

    /// `f * f * f * f` and `f^4` are the same tape: the base once, four
    /// multiplications. The count is the cost, and it must not depend on the
    /// spelling — without the canonical form the product spells the base out
    /// four times.
    #[test]
    fn a_repeated_factor_costs_what_its_power_costs() {
        const F: &str = "(cos(x1) * ln(x1 + 2) + sqrt(x1) / (1 + x1))";
        let base = tape_for(F, &["x1"]).insns.len();
        let power = tape_for(&format!("{F}^4"), &["x1"]).insns.len();
        let product = tape_for(&format!("{F} * {F} * {F} * {F}"), &["x1"])
            .insns
            .len();
        assert_eq!(product, power, "the spelling changed the tape's length");
        assert!(
            power < 2 * base,
            "the base was lowered more than once: {power} vs {base}"
        );
    }

    /// A whole power is a multiplication chain with the base lowered once:
    /// `(x1 + 1)^3` adds once and multiplies twice.
    #[test]
    fn a_whole_power_lowers_its_base_once() {
        let tape = tape_for("(x1 + 1)^3", &["x1"]);
        let adds = tape
            .insns
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    Instruction::Binary {
                        op: BinaryOp::Add,
                        ..
                    }
                )
            })
            .count();
        let multiplies = tape
            .insns
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    Instruction::Binary {
                        op: BinaryOp::Mul,
                        ..
                    }
                )
            })
            .count();
        assert_eq!((adds, multiplies), (1, 2), "{:?}", tape.insns);
        assert!(
            !tape.insns.iter().any(|i| matches!(
                i,
                Instruction::Binary {
                    op: BinaryOp::Pow,
                    ..
                }
            )),
            "a whole power reached the tape as `powf`: {:?}",
            tape.insns
        );
    }

    #[test]
    fn a_zeroth_power_is_one_and_never_reads_its_base() {
        let tape = tape_for("x1^0", &["x1"]);
        assert!(tape.insns.is_empty(), "{:?}", tape.insns);
        assert_eq!(tape.consts, vec![1.0]);
    }

    #[test]
    fn a_negative_power_is_one_over_the_positive_one() {
        let tape = tape_for("x1^-2", &["x1"]);
        assert!(
            matches!(
                tape.insns.last(),
                Some(Instruction::Binary {
                    op: BinaryOp::Div,
                    ..
                })
            ),
            "{:?}",
            tape.insns
        );
    }

    #[test]
    fn a_real_or_oversized_exponent_stays_a_power() {
        for source in ["x1^2.5", "x1^65", "x1^x2"] {
            let tape = tape_for(source, &["x1", "x2"]);
            assert!(
                tape.insns.iter().any(|i| matches!(
                    i,
                    Instruction::Binary {
                        op: BinaryOp::Pow,
                        ..
                    }
                )),
                "{source}: {:?}",
                tape.insns
            );
        }
    }

    /// Values, the half that could go wrong quietly. `powf` and repeated
    /// multiplication differ in the last place for about one case in five at
    /// `n >= 3`, so the assertion is against the multiplication, bit for bit.
    #[test]
    fn a_whole_power_multiplies_rather_than_calling_powf() {
        let x = 2.3_f64;
        let at =
            |source: &str| super::super::eval_one(source, &[("x1", x)]).expect("should evaluate");
        assert_eq!(at("x1^3"), x * x * x);
        // The case that motivates saying so out loud.
        assert_ne!(x * x * x, x.powf(3.0));
        assert_eq!(at("x1^-2"), 1.0 / (x * x));
        // Squaring is where libm and multiplication agree, so this one can be
        // asserted both ways.
        assert_eq!(at("x1^2"), x.powf(2.0));
        assert_eq!(at("x1^0"), 1.0);
    }

    #[test]
    fn an_and_seeds_from_negative_infinity() {
        // `invert_monotone` turns this into `x1 < e^2 and x1 > 0`.
        let tape = tape_for("ln(x1) < 2", &["x1"]);
        assert!(tape.consts.contains(&f64::NEG_INFINITY));
        assert!(tape.insns.iter().any(|i| matches!(
            i,
            Instruction::Combine {
                how: Accumulate::Worst,
                ..
            }
        )));
    }

    #[test]
    fn a_local_is_written_in_place_and_read_without_an_instruction() {
        let tape = tape_for("var x = x1 * 2;\nx + x", &["x1"]);
        let local = R(tape.first_local());
        assert_eq!(tape.locals, 1);
        assert!(tape.insns.iter().any(|i| matches!(
            i,
            Instruction::Binary {
                dst,
                op: BinaryOp::Mul,
                ..
            } if *dst == local
        )));
        assert!(
            !tape
                .insns
                .iter()
                .any(|i| matches!(i, Instruction::Copy { .. }))
        );
        assert!(
            !tape
                .insns
                .iter()
                .any(|i| matches!(i, Instruction::Check { .. }))
        );
    }

    #[test]
    fn a_leaf_assignment_becomes_a_copy() {
        let tape = tape_for("var x = x1;\nx + 1", &["x1"]);
        let local = R(tape.first_local());
        assert!(
            tape.insns
                .iter()
                .any(|i| matches!(i, Instruction::Copy { dst, .. } if *dst == local))
        );
    }

    /// An aggregate reaches the tape as a flat fold: three terms, three combines,
    /// and nothing that jumps.
    #[test]
    fn an_aggregate_is_a_flat_fold() {
        let tape = tape_for("sum(1, 3, i -> i * x1)", &["x1"]);
        let combines = tape
            .insns
            .iter()
            .filter(|i| matches!(i, Instruction::Combine { .. }))
            .count();
        assert_eq!(combines, 3);
    }

    #[test]
    fn every_checked_instruction_carries_the_span_of_its_node() {
        let tape = tape_for("sin(x1) + 2", &["x1"]);
        assert_eq!(
            tape.spans,
            vec![Span::new(4, 6), Span::new(0, 7), Span::new(0, 11)]
        );
        assert!(matches!(
            tape.insns[1],
            Instruction::Unary {
                op: UnaryOp::Sin,
                ..
            }
        ));
    }

    /// A front-end slot bug is the only way to reach this; the tape still reports
    /// it as the walker did rather than reading a stale register.
    #[test]
    fn an_unassigned_local_read_gets_a_check_at_the_local() {
        let program = Program {
            body: Block {
                assignments: Vec::new(),
                result: Expr::new(Kind::Local(LocalSlot::from_index(0)), Span::new(0, 1)),
            },
            frame_size: 1,
        };
        let tape = super::emit(&program, &[]).allocate();
        assert_eq!(tape.insns, vec![Instruction::Check { reg: R(0) }]);
        assert_eq!(tape.spans, vec![Span::new(0, 1)]);
        assert_eq!(tape.result, R(0));
    }
}

//! HC4-revise over a constraint's tape: what one variable may be if the
//! constraint is to hold, with every other variable at a given interval —
//! and [`slice`], which asks that of every constraint naming a coordinate.
//!
//! The evaluator lowers a constraint to a straight-line tape — its
//! intermediate representation, `eval::tape` — and this runs that tape over
//! intervals, twice. **Forward**, in order, computing every instruction's
//! interval into a slot: what each subexpression *can* be. **Backward**, in
//! reverse, from the requirement that the result be `<= 0` — a constraint
//! tape's result is its residual, and the crate's convention is that a
//! residual at or below zero is the constraint holding — pushing each
//! instruction's requirement onto its operands through the operator's
//! inverse ([`interval::invert_binary`] and friends), intersecting. The
//! requirement that reaches the wanted variable's `Load` is the answer.
//!
//! Every value is computed once and read from its slot: the tape is in single
//! assignment ([`VirtualTape::single_assignment`]), so a slot is one value for
//! the life of a sweep, which is what lets the backward pass read the operand
//! intervals it needs without evaluating anything again. That, and a bitset
//! per slot of the variables it depends on — so the backward sweep skips every
//! instruction that cannot reach the wanted one, which on a sum of two hundred
//! terms is nearly all of them — is the whole of the speed over the tree
//! walk this replaced: 8.5× on that sum, measured, with the same answers.
//!
//! Whole powers arrive as one instruction — the tree is lowered as parsed,
//! where the evaluator unrolls them first (`rewrite::unroll_powers`): `x·x·x`
//! over intervals is wider than `x³`, and a multiplication chain inverts by
//! dividing where a power inverts by its root.
//!
//! The superset rule of [`interval`] holds throughout: an operator with no
//! inverse leaves its operands' requirements at `ENTIRE`, a computed
//! subscript reads as `ENTIRE`, and the answer is a superset of the values
//! that satisfy the constraint — too wide costs a rejected proposal, never a
//! wrong point.

use crate::Ast;
use crate::ast::{BinaryOp, CompareOp, GlobalId};
use crate::cvg::incidence::Row;
use crate::cvg::interval::{self, Interval};
use crate::eval::irgen;
use crate::eval::tape::{Accumulate, Instruction, VirtualRegister, VirtualTape};
use crate::{ConstraintSystem, Point};

/// A constraint's tape over intervals, compiled once: the forward image and
/// the backward narrowing.
#[derive(Debug, Clone)]
pub(crate) struct IntervalTape {
    /// The tape over slots, in single assignment.
    steps: Vec<Instruction<u32>>,
    /// Slot `i < consts.len()` holds `consts[i]`.
    consts: Vec<f64>,
    /// The result's slot: the residual.
    result: u32,
    slots: usize,
    /// Per slot, the symbols its value depends on: `words` `u64`s each, one
    /// bit per symbol of the constraint.
    depends: Vec<u64>,
    words: usize,
}

/// The two frames a sweep writes, owned by the caller and reused across
/// calls: the forward intervals and the backward requirements.
#[derive(Debug, Default)]
pub(crate) struct Frames {
    forward: Vec<Interval>,
    need: Vec<Interval>,
}

/// Compiles `constraint` to its interval tape. Its `Load`s read the
/// constraint's own symbols by index — the order a caller's `globals` come in
/// — and the tree is lowered as parsed, whole powers intact.
pub(crate) fn compile(constraint: &Ast) -> IntervalTape {
    let symbol_count = constraint.symbols.len();
    let positions: Vec<u32> = (0..symbol_count)
        .map(|index| u32::try_from(index).expect("fewer than four billion symbols"))
        .collect();
    let tape: VirtualTape = irgen::emit(&constraint.program, &positions).single_assignment();

    let consts = tape.consts.values().to_vec();
    let locals = usize::from(tape.locals);
    let temps = usize::try_from(tape.temps()).expect("a tape's temporaries fit in memory");
    let slots = consts.len() + locals + temps;
    let slot = |reg: VirtualRegister| -> u32 {
        let index = match reg {
            VirtualRegister::Const(i) => usize::from(i),
            VirtualRegister::Local(j) => consts.len() + usize::from(j),
            VirtualRegister::Temp(k) => {
                consts.len() + locals + usize::try_from(k).expect("bounded by `temps`")
            }
        };
        u32::try_from(index).expect("bounded by `slots`")
    };
    let steps: Vec<Instruction<u32>> = tape.insns.iter().map(|insn| insn.map(slot)).collect();
    let result = slot(tape.result);

    let words = symbol_count.div_ceil(64).max(1);
    let mut depends = vec![0u64; slots * words];
    for step in &steps {
        let dst = match step.dst() {
            Some(dst) => dst as usize,
            None => continue,
        };
        if let Instruction::Load { input, .. } = *step {
            let input = input as usize;
            if input < symbol_count {
                depends[dst * words + input / 64] |= 1u64 << (input % 64);
            }
            continue;
        }
        for source in step.sources() {
            let source = source as usize;
            for word in 0..words {
                let bits = depends[source * words + word];
                depends[dst * words + word] |= bits;
            }
        }
    }

    IntervalTape {
        steps,
        consts,
        result,
        slots,
        depends,
        words,
    }
}

impl IntervalTape {
    fn depends_on(&self, slot: u32, symbol: u32) -> bool {
        let symbol = symbol as usize;
        self.depends[slot as usize * self.words + symbol / 64] & (1u64 << (symbol % 64)) != 0
    }

    /// The forward sweep: every slot's interval over `globals`, the
    /// constraint's symbols in order.
    fn forward(&self, globals: &[Interval], frames: &mut Frames) {
        let forward = &mut frames.forward;
        forward.clear();
        forward.resize(self.slots, Interval::ENTIRE);
        for (slot, value) in self.consts.iter().enumerate() {
            forward[slot] = Interval::point(*value);
        }
        for step in &self.steps {
            match *step {
                Instruction::Load { dst, input } => {
                    forward[dst as usize] = globals
                        .get(input as usize)
                        .copied()
                        .unwrap_or(Interval::ENTIRE);
                }
                Instruction::Copy { dst, src } => forward[dst as usize] = forward[src as usize],
                Instruction::Unary { dst, op, a } => {
                    forward[dst as usize] = interval::unary(op, forward[a as usize]);
                }
                Instruction::Binary { dst, op, a, b } => {
                    forward[dst as usize] = match self.whole_exponent(op, b) {
                        Some(n) => interval::power(forward[a as usize], n),
                        None => interval::binary(op, forward[a as usize], forward[b as usize]),
                    };
                }
                // The residual, as the walker computes it: the difference in
                // the order the comparison reads, without the strict nudge —
                // one point wider than the truth, rejected downstream.
                Instruction::Compare { dst, op, a, b } => {
                    let (minuend, subtrahend) = residual(op, a, b);
                    forward[dst as usize] = interval::binary(
                        BinaryOp::Sub,
                        forward[minuend as usize],
                        forward[subtrahend as usize],
                    );
                }
                Instruction::Combine { dst, how, a, b, .. } => {
                    forward[dst as usize] =
                        interval::binary(how_op(how), forward[a as usize], forward[b as usize]);
                }
                // A computed subscript reads a coordinate chosen by the point,
                // so there is no static answer.
                Instruction::Gather { dst, .. } => forward[dst as usize] = Interval::ENTIRE,
                Instruction::Check { .. } => {}
            }
        }
    }

    /// The expression's interval over `globals`: the forward sweep alone.
    /// For a constraint that is its residual. What the containment tests
    /// state their property against.
    #[cfg(test)]
    pub(crate) fn evaluate(&self, globals: &[Interval], frames: &mut Frames) -> Interval {
        self.forward(globals, frames);
        frames.forward[self.result as usize]
    }

    /// The interval `wanted` may take if the constraint is to hold, with
    /// every other symbol at the interval `globals` gives it.
    ///
    /// [`Interval::ENTIRE`] means nothing was concluded, and is always safe:
    /// the caller intersects with the declared box. [`Interval::EMPTY`] means
    /// the constraint holds nowhere on these globals — some instruction's
    /// requirement missed everything it can be — which is the one conclusion
    /// distinct from "nothing concluded". Where `wanted` is read more than
    /// once each read contributes a necessary condition, and their
    /// intersection is still one: sound, and weaker than a solve.
    pub(crate) fn narrow(
        &self,
        globals: &[Interval],
        wanted: GlobalId,
        frames: &mut Frames,
    ) -> Interval {
        self.forward(globals, frames);
        let wanted = u32::try_from(wanted.index()).expect("fewer than four billion symbols");
        let Frames { forward, need } = frames;
        need.clear();
        need.resize(self.slots, Interval::ENTIRE);
        need[self.result as usize] = Interval::new(f64::NEG_INFINITY, 0.0);

        let mut found = Interval::ENTIRE;
        for step in self.steps.iter().rev() {
            let Some(dst) = step.dst() else {
                continue;
            };
            if !self.depends_on(dst, wanted) || need[dst as usize] == Interval::ENTIRE {
                continue;
            }
            // What this value must be, and can be. Nothing in the overlap
            // means no point of the box satisfies the constraint.
            let target = need[dst as usize].intersect(forward[dst as usize]);
            if target.is_empty() {
                return Interval::EMPTY;
            }
            match *step {
                Instruction::Load { input, .. } => {
                    if input == wanted {
                        found = found.intersect(target);
                    }
                }
                Instruction::Copy { src, .. } => {
                    need[src as usize] = need[src as usize].intersect(target);
                }
                Instruction::Unary { op, a, .. } => {
                    let required = interval::invert_unary(op, target, forward[a as usize]);
                    need[a as usize] = need[a as usize].intersect(required);
                }
                Instruction::Binary { op, a, b, .. } => match self.whole_exponent(op, b) {
                    Some(n) => {
                        let required = interval::invert_power(target, n, forward[a as usize]);
                        need[a as usize] = need[a as usize].intersect(required);
                    }
                    None => {
                        let (ra, rb) = interval::invert_binary(
                            op,
                            target,
                            forward[a as usize],
                            forward[b as usize],
                        );
                        need[a as usize] = need[a as usize].intersect(ra);
                        need[b as usize] = need[b as usize].intersect(rb);
                    }
                },
                Instruction::Compare { op, a, b, .. } => {
                    let (minuend, subtrahend) = residual(op, a, b);
                    let (ra, rb) = interval::invert_binary(
                        BinaryOp::Sub,
                        target,
                        forward[minuend as usize],
                        forward[subtrahend as usize],
                    );
                    need[minuend as usize] = need[minuend as usize].intersect(ra);
                    need[subtrahend as usize] = need[subtrahend as usize].intersect(rb);
                }
                Instruction::Combine { how, a, b, .. } => match how {
                    Accumulate::Sum | Accumulate::Prod => {
                        let (ra, rb) = interval::invert_binary(
                            how_op(how),
                            target,
                            forward[a as usize],
                            forward[b as usize],
                        );
                        need[a as usize] = need[a as usize].intersect(ra);
                        need[b as usize] = need[b as usize].intersect(rb);
                    }
                    // The conjunction: the worst residual lands in `target`
                    // only if every residual is at most its top. Nothing is
                    // said about the bottom, since only one of them need
                    // reach it.
                    Accumulate::Worst => {
                        let cap = Interval::new(f64::NEG_INFINITY, target.hi());
                        need[a as usize] = need[a as usize].intersect(cap);
                        need[b as usize] = need[b as usize].intersect(cap);
                    }
                },
                Instruction::Gather { .. } | Instruction::Check { .. } => {}
            }
        }
        found
    }

    /// `Some(n)` where `op` is a power against the constant slot `b` holding
    /// a whole `n` within the cap, read the way the AST's `whole_exponent`
    /// reads its literal.
    fn whole_exponent(&self, op: BinaryOp, b: u32) -> Option<i64> {
        if op != BinaryOp::Pow {
            return None;
        }
        let value = *self.consts.get(b as usize)?;
        crate::ast::to_index(value).filter(|n| n.abs() <= crate::ast::POWER_LIMIT)
    }
}

/// Which operand a comparison's residual subtracts from which: `a < b` is
/// `a - b`, `a > b` is `b - a`.
const fn residual(op: CompareOp, a: u32, b: u32) -> (u32, u32) {
    match op {
        CompareOp::Lt | CompareOp::Lte => (a, b),
        CompareOp::Gt | CompareOp::Gte => (b, a),
    }
}

const fn how_op(how: Accumulate) -> BinaryOp {
    match how {
        Accumulate::Sum => BinaryOp::Add,
        Accumulate::Prod => BinaryOp::Mul,
        Accumulate::Worst => BinaryOp::Max,
    }
}

// ------------------------------------------------------------- over a system

/// The interval `coordinate` may take with every other coordinate held at its
/// value in `point`.
///
/// The declared box, narrowed by each constraint naming the coordinate in turn
/// through its [`IntervalTape`]. **A superset of the feasible slice**, so a
/// value drawn from it is still judged by [`ConstraintSystem::is_feasible`]
/// like any other candidate — see [`interval`]'s module doc for why that
/// leaves the distribution alone.
///
/// Constraints are applied in order and each sees what the previous ones
/// concluded, so a system narrows further than any one of its constraints
/// would. Nothing here iterates to a fixpoint: every coordinate but this one
/// is a point, which leaves a second pass with nothing to tighten.
pub(crate) fn slice(system: &ConstraintSystem, point: &Point, coordinate: usize) -> Interval {
    slice_conditioned(system, point, coordinate, None)
}

/// [`slice`], optionally ignoring constraints that mention a coordinate whose
/// value is about to change.
///
/// `settled[row]` says the point's value there is one to condition on. A
/// constraint naming an unsettled coordinate is skipped, because narrowing
/// against a value that is about to be overwritten conditions on a stale
/// number — see [`retract`](crate::cvg::classify::retract), which is the only
/// caller that passes a mask.
pub(crate) fn slice_conditioned(
    system: &ConstraintSystem,
    point: &Point,
    coordinate: usize,
    settled: Option<&[bool]>,
) -> Interval {
    let input = &system.variables[coordinate];
    let mut interval = Interval::new(input.lower(), input.upper());
    let mut frames = Frames::default();

    let coordinate = Row(coordinate);
    for id in system.incidence.naming(coordinate) {
        let rows = system.incidence.rows_of(*id);
        if let Some(settled) = settled
            && rows
                .iter()
                .any(|row| *row != coordinate && !settled[row.index()])
        {
            continue;
        }
        let wanted = rows
            .iter()
            .position(|row| *row == coordinate)
            .expect("`naming` lists only constraints that name the coordinate");

        // The constraint's symbols, in its own order, as intervals: a point
        // for everything held, and the running narrowing for the one asked
        // about.
        let globals: Vec<Interval> = rows
            .iter()
            .enumerate()
            .map(|(symbol, row)| {
                if symbol == wanted {
                    interval
                } else {
                    Interval::point(point[row.index()])
                }
            })
            .collect();

        let wanted = u32::try_from(wanted).expect("fewer than four billion symbols");
        interval = interval.intersect(system.constraints[id.index()].intervals.narrow(
            &globals,
            GlobalId::from_index(wanted),
            &mut frames,
        ));
        if interval.is_empty() {
            break;
        }
    }

    interval
}

#[cfg(test)]
mod tests {
    use super::*;

    fn narrower(source: &str) -> (IntervalTape, Ast) {
        let ast = crate::parse(source).expect("the source should compile");
        (compile(&ast), ast)
    }

    fn symbol(ast: &Ast, name: &str) -> GlobalId {
        let index = ast
            .symbols
            .iter()
            .position(|symbol| symbol == name)
            .unwrap_or_else(|| panic!("{name} is not a symbol of {}", ast.source));
        GlobalId::from_index(u32::try_from(index).expect("small"))
    }

    /// A whole power is one instruction and inverts through its root, where
    /// a multiplication chain would divide by the base's whole range.
    #[test]
    fn a_whole_power_is_kept_and_inverted_through_its_root() {
        let (narrower, ast) = narrower("x^3 < 27");
        let mut frames = Frames::default();
        let got = narrower.narrow(&[Interval::new(0.0, 10.0)], symbol(&ast, "x"), &mut frames);
        assert!(
            got.lo() <= 0.0 && got.hi() >= 3.0 && got.hi() < 3.0 + 1e-9,
            "{got:?}"
        );
    }

    /// `a == b +/- t` is two comparisons under the conjunction, and the
    /// conjunction's inverse recovers the band exactly.
    #[test]
    fn an_equality_narrows_to_its_band() {
        let (narrower, ast) = narrower("x == 2 +/- 0.5");
        let mut frames = Frames::default();
        let got = narrower.narrow(
            &[Interval::new(-10.0, 10.0)],
            symbol(&ast, "x"),
            &mut frames,
        );
        assert!(
            (got.lo() - 1.5).abs() < 1e-12 && (got.hi() - 2.5).abs() < 1e-12,
            "{got:?}"
        );
    }

    /// A requirement that misses everything a value can be is the constraint
    /// holding nowhere: `EMPTY`, not `ENTIRE`.
    #[test]
    fn an_impossible_constraint_is_empty() {
        let (narrower, ast) = narrower("x^2 < -1");
        let mut frames = Frames::default();
        let got = narrower.narrow(
            &[Interval::new(-10.0, 10.0)],
            symbol(&ast, "x"),
            &mut frames,
        );
        assert!(got.is_empty(), "{got:?}");
    }

    /// A computed subscript reads as everything, and the constraint still
    /// narrows what it can around it.
    #[test]
    fn a_computed_subscript_concludes_nothing_about_itself() {
        let (narrower, ast) = narrower("x + var[floor(n)] < 5");
        let mut frames = Frames::default();
        let globals = [Interval::new(0.0, 10.0), Interval::point(1.0)];
        let got = narrower.narrow(&globals, symbol(&ast, "x"), &mut frames);
        assert_eq!(got, Interval::ENTIRE, "{got:?}");
    }

    /// The wanted symbol in one term of a long sum: the backward sweep skips
    /// everything that cannot reach it and still lands the right bound.
    #[test]
    fn a_term_of_a_sum_is_narrowed_against_the_rest() {
        let terms: Vec<String> = (1..=50).map(|i| format!("x{i}")).collect();
        let source = format!("{} < 100", terms.join(" + "));
        let (narrower, ast) = narrower(&source);
        let mut frames = Frames::default();
        let mut globals = vec![Interval::point(1.0); 50];
        let wanted = symbol(&ast, "x7");
        globals[wanted.index()] = Interval::new(0.0, 1000.0);
        let got = narrower.narrow(&globals, wanted, &mut frames);
        // 49 others at 1 leave 51 for this one.
        assert!(got.hi() >= 51.0 && got.hi() < 51.0 + 1e-6, "{got:?}");
    }
}

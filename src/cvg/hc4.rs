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

use std::collections::HashMap;

use crate::Ast;
use crate::ast::{BinaryOp, CompareOp, GlobalId, UnaryOp};
use crate::cvg::incidence::{ConstraintId, Row};
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
    let symbol_count = constraint.symbols().len();
    let positions: Vec<u32> = (0..symbol_count)
        .map(|index| u32::try_from(index).expect("fewer than four billion symbols"))
        .collect();
    let tape: VirtualTape = irgen::emit(constraint.program(), &positions).single_assignment();

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
        self.backward(Some(wanted), frames)
    }

    /// What the constraint requires of each slot in `held` — the shared
    /// subexpressions it computes — with every symbol at the interval
    /// `globals` gives it: per slot, the interval the slot *can* take over
    /// the box, and the part of that it *must* take for the constraint to
    /// hold. `must` is [`Interval::EMPTY`] everywhere when the constraint
    /// holds nowhere on the box.
    pub(crate) fn require(
        &self,
        globals: &[Interval],
        held: &[(usize, u32)],
        frames: &mut Frames,
    ) -> Vec<(Interval, Interval)> {
        self.forward(globals, frames);
        let holds_nowhere = self.backward(None, frames).is_empty();
        held.iter()
            .map(|(_, slot)| {
                let can = frames.forward[*slot as usize];
                let must = if holds_nowhere {
                    Interval::EMPTY
                } else {
                    can.intersect(frames.need[*slot as usize])
                };
                (can, must)
            })
            .collect()
    }

    /// The backward sweep over `frames.forward`, leaving every slot's
    /// requirement in `frames.need`. With a `wanted` symbol it skips what
    /// cannot reach it and returns what that symbol may be; without one it
    /// visits everything the residual's requirement reaches and returns
    /// [`Interval::ENTIRE`]. Either way [`Interval::EMPTY`] is the constraint
    /// holding nowhere.
    fn backward(&self, wanted: Option<u32>, frames: &mut Frames) -> Interval {
        let Frames { forward, need } = frames;
        need.clear();
        need.resize(self.slots, Interval::ENTIRE);
        need[self.result as usize] = Interval::new(f64::NEG_INFINITY, 0.0);

        let mut found = Interval::ENTIRE;
        for step in self.steps.iter().rev() {
            let Some(dst) = step.dst() else {
                continue;
            };
            let unreachable = wanted.is_some_and(|wanted| !self.depends_on(dst, wanted));
            if unreachable || need[dst as usize] == Interval::ENTIRE {
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
                    if Some(input) == wanted {
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
                    // The disjunction, `Worst`'s dual: every residual is at
                    // least the best's bottom, and the best reaches the top
                    // only through a residual that can. Where one side is
                    // certainly above the top, the other has to carry it: a
                    // premise true over the whole box narrows the
                    // conclusion. Where neither is, nothing is said about
                    // either, since either might be the one that holds.
                    Accumulate::Best => {
                        let floor = Interval::new(target.lo(), f64::INFINITY);
                        need[a as usize] = need[a as usize].intersect(floor);
                        need[b as usize] = need[b as usize].intersect(floor);
                        let cap = Interval::new(f64::NEG_INFINITY, target.hi());
                        if forward[a as usize].lo() > target.hi() {
                            need[b as usize] = need[b as usize].intersect(cap);
                        }
                        if forward[b as usize].lo() > target.hi() {
                            need[a as usize] = need[a as usize].intersect(cap);
                        }
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

    /// Every slot an operator computes, paired with its subexpression's
    /// interned [`Term`], the constraint's symbols read as `rows`. Loads and
    /// constants are left out: a coordinate is already shared through the
    /// box, and a constant needs no sharing.
    fn terms(&self, rows: &[Row], interned: &mut HashMap<Term, u32>) -> Vec<(u32, u32)> {
        let mut keys: Vec<Option<u32>> = vec![None; self.slots];
        for (slot, value) in self.consts.iter().enumerate() {
            keys[slot] = Some(intern(interned, Term::Const(value.to_bits())));
        }
        let mut computed = Vec::new();
        for step in &self.steps {
            let (dst, key, operator) = match *step {
                Instruction::Load { dst, input } => (
                    dst,
                    rows.get(input as usize)
                        .map(|row| intern(interned, Term::Row(*row))),
                    false,
                ),
                Instruction::Copy { dst, src } => (dst, keys[src as usize], false),
                Instruction::Unary { dst, op, a } => (
                    dst,
                    keys[a as usize].map(|a| intern(interned, Term::Unary(op, a))),
                    true,
                ),
                Instruction::Binary { dst, op, a, b } => (
                    dst,
                    binary_term(interned, op, keys[a as usize], keys[b as usize]),
                    true,
                ),
                Instruction::Combine {
                    dst,
                    how: how @ (Accumulate::Sum | Accumulate::Prod),
                    a,
                    b,
                    ..
                } => (
                    dst,
                    binary_term(interned, how_op(how), keys[a as usize], keys[b as usize]),
                    true,
                ),
                // A residual and a conjunction or disjunction of residuals are
                // one constraint's verdict, not a value another computes; a
                // gather reads a column the point picks, so it has no
                // structure to match.
                Instruction::Compare { dst, .. }
                | Instruction::Combine {
                    dst,
                    how: Accumulate::Worst | Accumulate::Best,
                    ..
                }
                | Instruction::Gather { dst, .. } => (dst, None, false),
                Instruction::Check { .. } => continue,
            };
            keys[dst as usize] = key;
            if operator && let Some(key) = key {
                computed.push((key, dst));
            }
        }
        computed
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
        Accumulate::Best => BinaryOp::Min,
    }
}

// ----------------------------------------------------- shared subexpressions

/// The subexpressions more than one constraint computes, and where in each
/// constraint's tape they are.
///
/// HC4 narrows coordinates, one constraint at a time, so a contradiction
/// that lives in a subexpression rather than in any one coordinate is
/// invisible to it: `x + y <= 1` against `x + y >= 1 + 1e-9` takes a
/// billionth off `x` and `y` a visit and never empties either. The sum is
/// one value both constraints compute, though, and what each requires of it
/// — at most 1, at least `1 + 1e-9` — has nothing in common. So a box
/// carries an interval per shared subexpression as well as per coordinate
/// (`prune::Node`), and a contraction intersects every constraint's
/// requirement on it into that interval: empty is a proof. This is HC4 over
/// the constraints' common DAG, in the literature's terms, done only where
/// the DAG has a node two constraints share.
///
/// Matching is structural over the canonical tree as lowered: the same
/// operators over the same coordinates and constants, with `+`, `*`, `max`
/// and `min` taking their operands in either order, and `sum`/`prod` folds
/// read as the `+`/`*` they are. **There is deliberately no simplification
/// to a canonical polynomial form, nor any more general one**, though that
/// is what would make `2*x + 2*y >= 2.000000002` meet `x + y <= 1` and stays
/// `NotFound` instead. Expanding, collecting coefficients and certifying a
/// linear combination (Farkas' lemma, an LP with outward rounding) would add
/// a great deal of machinery to the front of every contraction for a class of
/// systems — contradictions thinner than a sample can tell from empty — with
/// no clear win in the regime this crate serves, which is sampling regions
/// that exist. `tests/cvg_pools.rs` pins both sides of that line.
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedTerms {
    /// How many there are: a box carries one interval for each.
    count: usize,
    /// Per constraint, `(term, slot)` for each slot of its tape that computes
    /// a shared term — twice where it computes one twice.
    held: Vec<Vec<(usize, u32)>>,
}

impl SharedTerms {
    pub(crate) const fn count(&self) -> usize {
        self.count
    }

    /// The shared terms constraint `id` computes, as `(term, slot)`.
    pub(crate) fn of(&self, id: ConstraintId) -> &[(usize, u32)] {
        &self.held[id.index()]
    }
}

/// Finds the subexpressions two or more of `constraints` compute, each
/// constraint given as its tape and the rows its symbols read, in order.
/// Terms are numbered in the order they are first met, so the numbering is a
/// function of the system alone.
pub(crate) fn shared(constraints: &[(&IntervalTape, &[Row])]) -> SharedTerms {
    let mut interned: HashMap<Term, u32> = HashMap::new();
    let computed: Vec<Vec<(u32, u32)>> = constraints
        .iter()
        .map(|(tape, rows)| tape.terms(rows, &mut interned))
        .collect();

    let mut owners: HashMap<u32, Vec<usize>> = HashMap::new();
    for (constraint, terms) in computed.iter().enumerate() {
        for (key, _) in terms {
            let owner = owners.entry(*key).or_default();
            if owner.last() != Some(&constraint) {
                owner.push(constraint);
            }
        }
    }

    let mut numbered: HashMap<u32, usize> = HashMap::new();
    let held = computed
        .iter()
        .map(|terms| {
            terms
                .iter()
                .filter(|(key, _)| owners[key].len() >= 2)
                .map(|(key, slot)| {
                    let next = numbered.len();
                    (*numbered.entry(*key).or_insert(next), *slot)
                })
                .collect()
        })
        .collect();
    SharedTerms {
        count: numbered.len(),
        held,
    }
}

/// One subexpression's structure with its symbols read as rows, so that the
/// same arithmetic in two constraints is the same key. Operands are interned
/// ids, so a key is the size of one instruction however deep the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Term {
    Row(Row),
    /// By bits: `0.0` and `-0.0` are two keys, which costs a match and never
    /// makes a wrong one.
    Const(u64),
    Unary(UnaryOp, u32),
    Binary(BinaryOp, u32, u32),
}

fn intern(interned: &mut HashMap<Term, u32>, term: Term) -> u32 {
    let next = u32::try_from(interned.len()).expect("fewer than four billion subexpressions");
    *interned.entry(term).or_insert(next)
}

/// The key of `a op b`, operands ordered where the operator commutes.
fn binary_term(
    interned: &mut HashMap<Term, u32>,
    op: BinaryOp,
    a: Option<u32>,
    b: Option<u32>,
) -> Option<u32> {
    let (a, b) = (a?, b?);
    let (a, b) = if commutes(op) {
        (a.min(b), a.max(b))
    } else {
        (a, b)
    };
    Some(intern(interned, Term::Binary(op, a, b)))
}

const fn commutes(op: BinaryOp) -> bool {
    match op {
        BinaryOp::Add | BinaryOp::Mul | BinaryOp::Max | BinaryOp::Min => true,
        BinaryOp::Sub | BinaryOp::Div | BinaryOp::Rem | BinaryOp::Pow | BinaryOp::LogB => false,
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
/// would. A discrete coordinate's slice is rounded inward onto its levels,
/// which keeps it a superset of the values it may take (see `cvg::support`).
/// Nothing here iterates to a fixpoint: every coordinate but this one is a
/// point, which leaves a second pass with nothing to tighten.
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
    let support = &system.supports[coordinate];
    let mut interval = Interval::new(support.lower(), support.upper());
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
        // Onto the coordinate's levels, if it has any, after every narrowing
        // rather than once at the end: the next constraint then narrows from
        // levels, and a slice between two levels is found empty at once.
        interval = support.round_inward(interval.intersect(
            system.constraints[id.index()].intervals.narrow(
                &globals,
                GlobalId::from_index(wanted),
                &mut frames,
            ),
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
            .symbols()
            .iter()
            .position(|symbol| symbol == name)
            .unwrap_or_else(|| panic!("{name} is not a symbol of {}", ast.source()));
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

    /// The disjunction's rule: where the premise holds over the whole box,
    /// its negation is certainly above zero, so the conclusion has to carry
    /// the implication and narrows like a constraint of its own.
    #[test]
    fn when_a_premise_holds_over_the_box_should_narrow_the_conclusion() {
        // setup
        let (narrower, ast) = narrower("x > 2 implies y < 1");
        let mut frames = Frames::default();
        let x_above_the_premise = [Interval::new(3.0, 10.0), Interval::new(0.0, 10.0)];

        // act
        let got = narrower.narrow(&x_above_the_premise, symbol(&ast, "y"), &mut frames);

        // assert
        assert!(
            got.lo() == 0.0 && got.hi() >= 1.0 && got.hi() < 1.0 + 1e-9,
            "{got:?}"
        );
    }

    /// Where the premise may or may not hold, either arm may be the one that
    /// does, and the conclusion says nothing about `y`.
    #[test]
    fn when_a_premise_may_fail_should_not_narrow_the_conclusion() {
        // setup
        let (narrower, ast) = narrower("x > 2 implies y < 1");
        let mut frames = Frames::default();
        let x_across_the_premise = [Interval::new(0.0, 10.0), Interval::new(0.0, 10.0)];

        // act
        let got = narrower.narrow(&x_across_the_premise, symbol(&ast, "y"), &mut frames);

        // assert
        assert!(got.lo() <= 0.0 && got.hi() >= 10.0, "{got:?}");
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

    /// A sum written either way round is one subexpression, and the same
    /// half-plane scaled is not: matching is structural, with no algebra.
    #[test]
    fn when_two_constraints_compute_one_sum_in_either_order_should_share_it_and_nothing_else() {
        // setup
        let variables = vec![
            crate::InputVariable::new_continuous("x", -1.0, 1.0),
            crate::InputVariable::new_continuous("y", -1.0, 1.0),
        ];

        // act
        let system = crate::system::tests::system(
            variables,
            &["x + y <= 1", "y + x >= -1", "2*x + 2*y >= 2"],
        );

        // assert
        assert_eq!(system.shared.count(), 1);
        assert_eq!(system.shared.of(ConstraintId(0)).len(), 1);
        assert_eq!(system.shared.of(ConstraintId(1)).len(), 1);
        assert!(system.shared.of(ConstraintId(2)).is_empty());
    }

    /// A discrete coordinate's slice is rounded onto its levels: `n > 2.5`
    /// leaves `n` the integers from 3, and `n > 10.2` leaves it none, which
    /// the interval alone would not have said.
    #[test]
    fn a_discrete_slice_is_rounded_onto_its_levels() {
        // setup
        let system = crate::system::tests::system(
            vec![
                crate::InputVariable::new_discrete("n", 0.0, 1.0, 10.0),
                crate::InputVariable::new_continuous("x", 0.0, 10.0),
            ],
            &["n > x + 0.5"],
        );

        // act
        let above = slice(&system, &vec![0.0, 2.0], 0);
        let past = slice(&system, &vec![0.0, 9.7], 0);

        // assert
        assert_eq!(above, Interval::new(3.0, 10.0));
        assert!(past.is_empty(), "{past:?}");
    }
}

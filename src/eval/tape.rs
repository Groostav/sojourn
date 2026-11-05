//! The evaluator's **intermediate representation** (IR): a flat list of
//! three-address instructions, in two forms. [`VirtualTape`] is the IR as
//! lowering produces it, over virtual registers, which a tape-to-tape pass
//! (`differentiate`) transforms; [`AllocatedTape`] is the same IR after
//! register allocation, over physical ones, which the executors run.
//!
//! A tree walk pays its dispatch once per node per sample. A tape pays it once
//! per instruction per *tile* of samples: each instruction is one loop over a
//! slice of lanes, which is the shape the compiler vectorises. The same tape,
//! read one lane at a time, is also the per-row evaluator the sampler uses.
//!
//! Three-address form rather than a stack, because every consumer wants
//! names: the batched executor wants a destination slice and operand slices,
//! the register allocator wants intervals, and a shader emitter wants
//! `let t7 = t3 * t4;`. A stack machine is a register machine with a fixed
//! implicit allocation, and the implicitness is what would hurt.
//!
//! The semantics are the tree-walker's, to the bit. Every operator goes
//! through [`UnaryOp::apply`] and [`BinaryOp::apply`]; comparisons compute
//! exactly the expressions the walker did, in the same order; and the
//! non-finite check that the walker ran on every node runs here on every
//! checked instruction. `tests/corpus.rs`, `tests/runtime_errors.rs` and
//! `tests/special_values.rs` are the spec.

use std::collections::HashMap;

use crate::ast::{AggregateKind, BinaryOp, CompareOp, UnaryOp};
use crate::diagnostics::{Fault, ProblemKind, Span};

use super::regalloc;

/// A physical register: an index into the frame (per lane) or the register
/// file (per tile).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Register(pub(crate) u16);

impl Register {
    pub(crate) const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A register before allocation, by class.
///
/// Constants and locals are *pinned*: a constant lives in one register for
/// the life of the tape, and a local keeps the frame slot the front end gave
/// it, so that an unwritten local reads the NaN it was primed with — the
/// walker's sentinel, preserved. Only temporaries are packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum VirtualRegister {
    Const(u16),
    Local(u16),
    Temp(u32),
}

/// The constant pool of a tape under construction: one register per distinct
/// bit pattern, so `0.0` and `-0.0` stay distinct and a value used twice is
/// loaded once.
#[derive(Debug, Clone, Default)]
pub(crate) struct Constants {
    values: Vec<f64>,
    index: HashMap<u64, u16>,
}

impl Constants {
    /// The register holding `value`, minted on first use.
    pub(crate) fn intern(&mut self, value: f64) -> VirtualRegister {
        let bits = value.to_bits();
        if let Some(&index) = self.index.get(&bits) {
            return VirtualRegister::Const(index);
        }
        let index = u16::try_from(self.values.len()).expect("fewer than 65536 constants");
        self.values.push(value);
        self.index.insert(bits, index);
        VirtualRegister::Const(index)
    }

    pub(crate) fn values(&self) -> &[f64] {
        &self.values
    }
}

/// The intermediate representation before register allocation: instructions
/// over virtual registers, with the constant pool they refer to.
///
/// The form a tape-to-tape transformation wants. Allocation packs
/// temporaries, so a physical register holds different values at different
/// instructions; before it, a temporary is one value, which is what the
/// reverse sweep in `differentiate` relies on.
#[derive(Debug, Clone)]
pub(crate) struct VirtualTape {
    pub(crate) consts: Constants,
    pub(crate) locals: u16,
    /// One past the highest temporary in use; [`fresh_temp`](Self::fresh_temp)
    /// is the only way to mint one, so it stays true.
    temps: u32,
    pub(crate) insns: Vec<Instruction<VirtualRegister>>,
    pub(crate) spans: Vec<Span>,
    pub(crate) result: VirtualRegister,
    /// The partial derivatives a differentiated tape also computes, one per
    /// symbol; empty for a plain expression.
    pub(crate) partials: Vec<VirtualRegister>,
}

impl VirtualTape {
    /// An emitted program's tape: `temps` is the count the emitter minted.
    pub(crate) fn new(
        consts: Constants,
        locals: u16,
        temps: u32,
        insns: Vec<Instruction<VirtualRegister>>,
        spans: Vec<Span>,
        result: VirtualRegister,
    ) -> Self {
        Self {
            consts,
            locals,
            temps,
            insns,
            spans,
            result,
            partials: Vec::new(),
        }
    }

    /// One past the highest temporary in use: where a tape built on top of
    /// this one continues numbering.
    pub(crate) const fn temps(&self) -> u32 {
        self.temps
    }

    /// A temporary no instruction has written yet.
    pub(crate) fn fresh_temp(&mut self) -> VirtualRegister {
        let t = VirtualRegister::Temp(self.temps);
        self.temps += 1;
        t
    }

    /// The same computation with every write to a fresh temporary and every
    /// read naming the write it sees, so that each value has one register
    /// for the life of the tape: what a sweep that reads intermediates after
    /// the fact — the reverse sweep of a derivative, the backward pass of a
    /// narrowing — relies on. The emitter writes a fold's accumulator once
    /// per term and a `let` slot where the front end put it; this undoes
    /// both. Straight-line code needs no more than renaming — "SSA
    /// construction" (Cytron et al. 1991) is the general form, for code with
    /// branches. Constants stay pinned; the temporary numbering continues
    /// past this tape's own.
    pub(crate) fn single_assignment(&self) -> Self {
        let mut renamed = Self::new(
            self.consts.clone(),
            self.locals,
            self.temps,
            Vec::with_capacity(self.insns.len()),
            self.spans.clone(),
            self.result,
        );
        let mut version: HashMap<VirtualRegister, VirtualRegister> = HashMap::new();
        for insn in &self.insns {
            let read = insn.map(|reg| version.get(&reg).copied().unwrap_or(reg));
            let written = match read {
                Instruction::Check { .. } => read,
                _ => {
                    let fresh = renamed.fresh_temp();
                    let dst = insn.dst().expect("every instruction but a check writes");
                    version.insert(dst, fresh);
                    read.with_dst(fresh)
                }
            };
            renamed.insns.push(written);
        }
        let current = |reg: VirtualRegister| version.get(&reg).copied().unwrap_or(reg);
        renamed.result = current(self.result);
        renamed.partials = self.partials.iter().map(|reg| current(*reg)).collect();
        renamed
    }

    /// Packs the temporaries and pins everything else: the tape the
    /// executors run.
    pub(crate) fn allocate(self) -> AllocatedTape {
        let consts = u16::try_from(self.consts.values().len()).expect("fewer than 65536 constants");
        let mut outputs = vec![self.result];
        outputs.extend_from_slice(&self.partials);
        let (insns, outputs, registers) =
            regalloc::allocate(self.insns, &outputs, consts, self.locals);
        let mut outputs = outputs.into_iter();
        let result = outputs.next().expect("the result is the first output");
        AllocatedTape {
            consts: self.consts.values().to_vec(),
            locals: self.locals,
            registers,
            insns,
            spans: self.spans,
            result,
            partials: outputs.collect(),
        }
    }
}

/// How a fold step combines, each arm deferring to the AST's own definition so
/// that the tape cannot drift from the language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Accumulate {
    Sum,
    Prod,
    /// Conjunction under the `<= 0` convention: the worst residual wins.
    /// typically NaN-propagating `max`, via `BinaryOp::Max`.
    Worst,
    /// Disjunction, `Worst`'s dual: the best residual wins, via `BinaryOp::Min`.
    /// What an implication lowers to, `min(not premise, conclusion)`.
    ///
    /// A fold of its own rather than a `Binary { op: Min }` for two reasons.
    /// It is a boolean connective, and reads as one beside `Worst` in every
    /// executor. And `cvg::hc4` gives it a backward rule of its own, the
    /// disjunction's, where `Min` as arithmetic has no inverse
    /// (`interval::invert_binary`) and must keep having none, since
    /// `classify::reaches` declines `max`/`min` by that same table.
    Best,
}

impl Accumulate {
    pub(crate) fn apply(self, accumulated: f64, term: f64) -> f64 {
        match self {
            Self::Sum => AggregateKind::Sum.combine(accumulated, term),
            Self::Prod => AggregateKind::Prod.combine(accumulated, term),
            Self::Worst => BinaryOp::Max.apply(accumulated, term),
            Self::Best => BinaryOp::Min.apply(accumulated, term),
        }
    }

    pub(crate) const fn identity(self) -> f64 {
        match self {
            Self::Sum => AggregateKind::Sum.identity(),
            Self::Prod => AggregateKind::Prod.identity(),
            Self::Worst => f64::NEG_INFINITY,
            Self::Best => f64::INFINITY,
        }
    }

    pub(crate) const fn from_aggregate(kind: AggregateKind) -> Self {
        match kind {
            AggregateKind::Sum => Self::Sum,
            AggregateKind::Prod => Self::Prod,
        }
    }
}

/// One instruction. Generic over the register type so that a tape whose
/// registers have not been allocated cannot be run by mistake.
///
/// "Checked" below means the destination is tested for a finite value and a
/// non-finite one is a [`ProblemKind::NonFiniteValue`] at the instruction's
/// span — the walker's rule, applied per instruction rather than per node,
/// which is the same set of places.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Instruction<R> {
    /// `row[input]` into `dst`. Checked, so a non-finite input fails at the
    /// variable that carried it.
    Load {
        dst: R,
        input: u32,
    },
    /// `var x = <leaf>`: a leaf has no instruction of its own to write into
    /// the local, so it is copied. Unchecked — the leaf was.
    Copy {
        dst: R,
        src: R,
    },
    Unary {
        dst: R,
        op: UnaryOp,
        a: R,
    },
    /// `dst` never aliases `a` or `b`; the allocator guarantees it and the
    /// batched executor's slice split depends on it.
    Binary {
        dst: R,
        op: BinaryOp,
        a: R,
        b: R,
    },
    /// The residual of `a op b` under the `<= 0` convention, computed exactly
    /// as the walker computes it, `EPSILON` nudge included. Checked.
    Compare {
        dst: R,
        op: CompareOp,
        a: R,
        b: R,
    },
    /// One step of a fold: `dst = how(a, b)`. `dst == a` is allowed and usual,
    /// for in-place accumulation. Checked only when `last`, because the walker
    /// checks a fold's final value and not its intermediate ones: a product
    /// that overflows to infinity at term three and reaches NaN at term five
    /// reports the NaN.
    Combine {
        dst: R,
        how: Accumulate,
        a: R,
        b: R,
        last: bool,
    },
    /// Test `reg` for a finite value and fault at this instruction's span if it
    /// is not. Emitted for a local the emitter cannot prove was assigned — the
    /// tree-walker's NaN-sentinel read, preserved.
    Check {
        reg: R,
    },
    /// `var[index]`: one-based into the whole row. The index faults name
    /// `subscript`; a non-finite value read names this instruction. Checked.
    Gather {
        dst: R,
        index: R,
        subscript: Span,
    },
}

impl<R: Copy> Instruction<R> {
    /// The register this instruction writes, if any.
    pub(crate) fn dst(&self) -> Option<R> {
        match *self {
            Instruction::Load { dst, .. }
            | Instruction::Copy { dst, .. }
            | Instruction::Unary { dst, .. }
            | Instruction::Binary { dst, .. }
            | Instruction::Compare { dst, .. }
            | Instruction::Combine { dst, .. }
            | Instruction::Gather { dst, .. } => Some(dst),
            Instruction::Check { .. } => None,
        }
    }

    /// The registers this instruction reads.
    pub(crate) fn sources(&self) -> Vec<R> {
        match *self {
            Instruction::Load { .. } => Vec::new(),
            Instruction::Copy { src, .. } => vec![src],
            Instruction::Unary { a, .. } => vec![a],
            Instruction::Binary { a, b, .. } | Instruction::Compare { a, b, .. } => vec![a, b],
            Instruction::Combine { a, b, .. } => vec![a, b],
            Instruction::Check { reg } => vec![reg],
            Instruction::Gather { index, .. } => vec![index],
        }
    }

    /// The same instruction writing `dst` instead; a `Check` is unchanged.
    pub(crate) fn with_dst(self, dst: R) -> Self {
        match self {
            Instruction::Load { input, .. } => Instruction::Load { dst, input },
            Instruction::Copy { src, .. } => Instruction::Copy { dst, src },
            Instruction::Unary { op, a, .. } => Instruction::Unary { dst, op, a },
            Instruction::Binary { op, a, b, .. } => Instruction::Binary { dst, op, a, b },
            Instruction::Compare { op, a, b, .. } => Instruction::Compare { dst, op, a, b },
            Instruction::Combine {
                how, a, b, last, ..
            } => Instruction::Combine {
                dst,
                how,
                a,
                b,
                last,
            },
            Instruction::Check { reg } => Instruction::Check { reg },
            Instruction::Gather {
                index, subscript, ..
            } => Instruction::Gather {
                dst,
                index,
                subscript,
            },
        }
    }

    /// The same instruction over another register type.
    pub(crate) fn map<S>(self, mut f: impl FnMut(R) -> S) -> Instruction<S> {
        match self {
            Instruction::Load { dst, input } => Instruction::Load { dst: f(dst), input },
            Instruction::Copy { dst, src } => Instruction::Copy {
                dst: f(dst),
                src: f(src),
            },
            Instruction::Unary { dst, op, a } => Instruction::Unary {
                dst: f(dst),
                op,
                a: f(a),
            },
            Instruction::Binary { dst, op, a, b } => Instruction::Binary {
                dst: f(dst),
                op,
                a: f(a),
                b: f(b),
            },
            Instruction::Compare { dst, op, a, b } => Instruction::Compare {
                dst: f(dst),
                op,
                a: f(a),
                b: f(b),
            },
            Instruction::Combine {
                dst,
                how,
                a,
                b,
                last,
            } => Instruction::Combine {
                dst: f(dst),
                how,
                a: f(a),
                b: f(b),
                last,
            },
            Instruction::Check { reg } => Instruction::Check { reg: f(reg) },
            Instruction::Gather {
                dst,
                index,
                subscript,
            } => Instruction::Gather {
                dst: f(dst),
                index: f(index),
                subscript,
            },
        }
    }
}

/// What went wrong on one lane, in terms the tape can turn into a [`Fault`].
///
/// Recorded rather than raised, because the batched executor keeps going: NaN
/// flows on through the remaining instructions and the lowest faulted lane is
/// reported at the end, which is the walker's "first failing column".
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LaneFault {
    pub(crate) insn: u32,
    pub(crate) kind: FaultKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FaultKind {
    NonFinite(f64),
    OutOfBounds {
        requested_1index: i64,
        available: usize,
    },
}

/// The intermediate representation after register allocation: what the
/// executors run.
#[derive(Debug, Clone)]
pub(crate) struct AllocatedTape {
    /// Register `i` holds `consts[i]`. Deduplicated by bit pattern, so `0.0`
    /// and `-0.0` are distinct.
    pub(crate) consts: Vec<f64>,
    /// The front end's `frame_size`. Registers `consts.len()..` are the locals,
    /// primed to NaN.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the tests read the register class boundaries")
    )]
    pub(crate) locals: u16,
    /// Constants, locals and packed temporaries together.
    pub(crate) registers: u16,
    pub(crate) insns: Vec<Instruction<Register>>,
    /// `spans[i]` is the node `insns[i]` computes: the span a check reports.
    pub(crate) spans: Vec<Span>,
    pub(crate) result: Register,
    /// The partial derivatives a differentiated tape also computes, one per
    /// symbol of the expression; empty for a plain expression.
    pub(crate) partials: Vec<Register>,
}

impl AllocatedTape {
    /// Fills a frame or one lane's worth of a register file: constants in
    /// place, everything else NaN. The NaN is the walker's unwritten-slot
    /// sentinel and is what makes a `Check` on an unassigned local fire.
    pub(crate) fn prime(&self, frame: &mut [f64]) {
        debug_assert_eq!(frame.len(), self.registers as usize);
        frame.fill(f64::NAN);
        frame[..self.consts.len()].copy_from_slice(&self.consts);
    }

    /// The register class boundaries, for the tests.
    #[cfg(test)]
    pub(crate) fn first_local(&self) -> u16 {
        u16::try_from(self.consts.len()).expect("constant count fits u16")
    }

    /// Renders a lane's fault against the tape's spans.
    pub(crate) fn fault(&self, lane: LaneFault) -> Fault {
        let at = lane.insn as usize;
        let subscript = match self.insns[at] {
            Instruction::Gather { subscript, .. } => subscript,
            _ => self.spans[at],
        };
        match lane.kind {
            FaultKind::NonFinite(value) => Fault {
                kind: ProblemKind::NonFiniteValue { value },
                span: self.spans[at],
            },
            FaultKind::OutOfBounds {
                requested_1index,
                available,
            } => Fault {
                kind: ProblemKind::DynamicIndexOutOfBounds {
                    requested_1index,
                    available,
                },
                span: subscript,
            },
        }
    }
}

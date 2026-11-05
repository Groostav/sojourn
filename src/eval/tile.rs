//! The batched executor: one straight-line tape over a tile of samples, each
//! instruction one kernel across the lanes.
//!
//! The instruction set is chosen once per tile by `pulp::Arch` — AVX2 where
//! the machine has it, the scalar backend otherwise — and the whole walk runs
//! inside that choice. Each instruction picks a kernel from `simd.rs` by name:
//! a vector kernel for the operators that have one, a `*_scalar` kernel for
//! the rest, so which is which is written down rather than decided by the
//! compiler.
//!
//! Faults are recorded, not raised. Each kernel reports whether any lane went
//! non-finite; only then does the slow path walk the lanes and record which.
//! NaN flows on through the remaining instructions, every lane keeps its own
//! first fault, and the lowest faulted lane is reported at the end — the
//! walker's "first failing column", with the walker's innermost span, because
//! instruction order is post-order.

use faer::MatRef;
use pulp::{Simd, WithSimd};

use crate::ast::{BinaryOp, UnaryOp};

use super::RowLayout;
use super::lane;
use super::simd;
use super::tape::{Accumulate, AllocatedTape, FaultKind, Instruction, LaneFault, Register};

/// Lanes per tile.
///
/// A register is `TILE × 8` bytes: 2 KB, so sixteen live registers are 32 KB,
/// inside L1D on every machine in `performance-records/hosts`. It is also the
/// ledgers' batch width, so a batch of 256 is exactly one tile and the
/// 1024-wide brute-squad batches are four; and it is a multiple of every lane
/// count pulp has, so a full tile leaves no scalar tail. Chosen, not measured;
/// the tile sweep is a later item.
pub(crate) const TILE: usize = 256;

/// `registers × width` doubles, register-major, so a register is one
/// contiguous slice.
pub(crate) struct RegisterFile {
    data: Vec<f64>,
    width: usize,
}

impl RegisterFile {
    /// Primed once: constants broadcast, everything else NaN. A straight-line
    /// tape writes every register it reads before reading it, except a local
    /// the emitter could not prove assigned — which must read NaN, and does,
    /// because nothing ever writes it.
    pub(crate) fn new(tape: &AllocatedTape, width: usize) -> Self {
        let mut data = vec![f64::NAN; tape.registers as usize * width];
        for (index, &value) in tape.consts.iter().enumerate() {
            data[index * width..(index + 1) * width].fill(value);
        }
        Self { data, width }
    }

    pub(crate) fn reg(&self, reg: Register, lanes: usize) -> &[f64] {
        &self.data[reg.index() * self.width..][..lanes]
    }

    fn reg_mut(&mut self, reg: Register, lanes: usize) -> &mut [f64] {
        &mut self.data[reg.index() * self.width..][..lanes]
    }

    /// `dst` mutable alongside `a` shared. Requires `dst != a`.
    fn dst_a(&mut self, dst: Register, a: Register, lanes: usize) -> (&mut [f64], &[f64]) {
        let width = self.width;
        let (d, before, after) = self.split(dst);
        (
            &mut d[..lanes],
            &pick(before, after, width, dst, a)[..lanes],
        )
    }

    /// `dst` mutable alongside `a` and `b` shared. Requires `dst ∉ {a, b}`;
    /// `a == b` is fine.
    fn dst_a_b(
        &mut self,
        dst: Register,
        a: Register,
        b: Register,
        lanes: usize,
    ) -> (&mut [f64], &[f64], &[f64]) {
        let width = self.width;
        let (d, before, after) = self.split(dst);
        (
            &mut d[..lanes],
            &pick(before, after, width, dst, a)[..lanes],
            &pick(before, after, width, dst, b)[..lanes],
        )
    }

    /// The destination register split out of the file, with the registers
    /// before and after it as shared slices. The allocator guarantees a
    /// destination never aliases an operand, which is what makes this sound.
    fn split(&mut self, dst: Register) -> (&mut [f64], &[f64], &[f64]) {
        let start = dst.index() * self.width;
        let (before, rest) = self.data.split_at_mut(start);
        let (d, after) = rest.split_at_mut(self.width);
        (d, before, after)
    }
}

/// Register `reg`'s slice from the two halves a [`RegisterFile::split`] left.
fn pick<'a>(
    before: &'a [f64],
    after: &'a [f64],
    width: usize,
    dst: Register,
    reg: Register,
) -> &'a [f64] {
    assert_ne!(reg, dst, "a destination register aliased an operand");
    if reg < dst {
        &before[reg.index() * width..][..width]
    } else {
        &after[(reg.index() - dst.index() - 1) * width..][..width]
    }
}

fn record(faults: &mut [Option<LaneFault>], lane: usize, pc: usize, kind: FaultKind) {
    if faults[lane].is_none() {
        faults[lane] = Some(LaneFault {
            insn: u32::try_from(pc).expect("fewer than 2^32 instructions"),
            kind,
        });
    }
}

/// The slow path, taken only after a kernel said some lane went bad.
fn record_non_finite(faults: &mut [Option<LaneFault>], pc: usize, values: &[f64]) {
    for (lane, &value) in values.iter().enumerate() {
        if !value.is_finite() {
            record(faults, lane, pc, FaultKind::NonFinite(value));
        }
    }
}

/// One instruction's unary operator, chosen outside the loop. The vector arms
/// name their kernel; the rest are libm, rounding and sign, and stay scalar.
/// Exhaustive, so a new operator fails to compile here rather than silently
/// taking a slow path.
macro_rules! dispatch_unary {
    ($simd:expr, $op:expr, $d:expr, $a:expr) => {
        match $op {
            UnaryOp::Negate => simd::unary(
                $simd,
                $d,
                $a,
                |s, x| s.neg_f64s(x),
                |x| UnaryOp::Negate.apply(x),
            ),
            UnaryOp::Abs => simd::unary(
                $simd,
                $d,
                $a,
                |s, x| s.abs_f64s(x),
                |x| UnaryOp::Abs.apply(x),
            ),
            UnaryOp::Sqrt => simd::unary(
                $simd,
                $d,
                $a,
                |s, x| s.sqrt_f64s(x),
                |x| UnaryOp::Sqrt.apply(x),
            ),
            UnaryOp::Sqr => simd::unary(
                $simd,
                $d,
                $a,
                |s, x| s.mul_f64s(x, x),
                |x| UnaryOp::Sqr.apply(x),
            ),
            UnaryOp::Cube => simd::unary(
                $simd,
                $d,
                $a,
                |s, x| s.mul_f64s(s.mul_f64s(x, x), x),
                |x| UnaryOp::Cube.apply(x),
            ),
            UnaryOp::Cos => simd::unary_scalar($d, $a, |x| UnaryOp::Cos.apply(x)),
            UnaryOp::Sin => simd::unary_scalar($d, $a, |x| UnaryOp::Sin.apply(x)),
            UnaryOp::Tan => simd::unary_scalar($d, $a, |x| UnaryOp::Tan.apply(x)),
            UnaryOp::Acos => simd::unary_scalar($d, $a, |x| UnaryOp::Acos.apply(x)),
            UnaryOp::Asin => simd::unary_scalar($d, $a, |x| UnaryOp::Asin.apply(x)),
            UnaryOp::Atan => simd::unary_scalar($d, $a, |x| UnaryOp::Atan.apply(x)),
            UnaryOp::Cosh => simd::unary_scalar($d, $a, |x| UnaryOp::Cosh.apply(x)),
            UnaryOp::Sinh => simd::unary_scalar($d, $a, |x| UnaryOp::Sinh.apply(x)),
            UnaryOp::Tanh => simd::unary_scalar($d, $a, |x| UnaryOp::Tanh.apply(x)),
            UnaryOp::Cot => simd::unary_scalar($d, $a, |x| UnaryOp::Cot.apply(x)),
            UnaryOp::Ln => simd::unary_scalar($d, $a, |x| UnaryOp::Ln.apply(x)),
            UnaryOp::Log10 => simd::unary_scalar($d, $a, |x| UnaryOp::Log10.apply(x)),
            UnaryOp::Cbrt => simd::unary_scalar($d, $a, |x| UnaryOp::Cbrt.apply(x)),
            UnaryOp::Ceil => simd::unary_scalar($d, $a, |x| UnaryOp::Ceil.apply(x)),
            UnaryOp::Floor => simd::unary_scalar($d, $a, |x| UnaryOp::Floor.apply(x)),
            UnaryOp::Sgn => simd::unary_scalar($d, $a, |x| UnaryOp::Sgn.apply(x)),
        }
    };
}

/// One instruction's binary operator, chosen outside the loop. `Max` and `Min`
/// go through the NaN-propagating kernels, never pulp's own.
macro_rules! dispatch_binary {
    ($simd:expr, $op:expr, $d:expr, $a:expr, $b:expr) => {
        match $op {
            BinaryOp::Add => simd::binary(
                $simd,
                $d,
                $a,
                $b,
                |s, x, y| s.add_f64s(x, y),
                |x, y| BinaryOp::Add.apply(x, y),
            ),
            BinaryOp::Sub => simd::binary(
                $simd,
                $d,
                $a,
                $b,
                |s, x, y| s.sub_f64s(x, y),
                |x, y| BinaryOp::Sub.apply(x, y),
            ),
            BinaryOp::Mul => simd::binary(
                $simd,
                $d,
                $a,
                $b,
                |s, x, y| s.mul_f64s(x, y),
                |x, y| BinaryOp::Mul.apply(x, y),
            ),
            BinaryOp::Div => simd::binary(
                $simd,
                $d,
                $a,
                $b,
                |s, x, y| s.div_f64s(x, y),
                |x, y| BinaryOp::Div.apply(x, y),
            ),
            BinaryOp::Max => simd::binary($simd, $d, $a, $b, simd::max_nan, simd::max_scalar),
            BinaryOp::Min => simd::binary($simd, $d, $a, $b, simd::min_nan, simd::min_scalar),
            BinaryOp::Rem => simd::binary_scalar($d, $a, $b, |x, y| BinaryOp::Rem.apply(x, y)),
            BinaryOp::Pow => simd::binary_scalar($d, $a, $b, |x, y| BinaryOp::Pow.apply(x, y)),
            BinaryOp::LogB => simd::binary_scalar($d, $a, $b, |x, y| BinaryOp::LogB.apply(x, y)),
        }
    };
}

/// A fold step's combine, chosen outside the loop, in the in-place or the
/// three-address form.
macro_rules! dispatch_combine {
    ($simd:expr, $how:expr, in_place: $d:expr, $b:expr) => {
        match $how {
            Accumulate::Sum => simd::in_place(
                $simd,
                $d,
                $b,
                |s, x, y| s.add_f64s(x, y),
                |x, y| Accumulate::Sum.apply(x, y),
            ),
            Accumulate::Prod => simd::in_place(
                $simd,
                $d,
                $b,
                |s, x, y| s.mul_f64s(x, y),
                |x, y| Accumulate::Prod.apply(x, y),
            ),
            Accumulate::Worst => simd::in_place($simd, $d, $b, simd::max_nan, |x, y| {
                Accumulate::Worst.apply(x, y)
            }),
            Accumulate::Best => simd::in_place($simd, $d, $b, simd::min_nan, |x, y| {
                Accumulate::Best.apply(x, y)
            }),
        }
    };
    ($simd:expr, $how:expr, three_address: $d:expr, $a:expr, $b:expr) => {
        match $how {
            Accumulate::Sum => simd::binary(
                $simd,
                $d,
                $a,
                $b,
                |s, x, y| s.add_f64s(x, y),
                |x, y| Accumulate::Sum.apply(x, y),
            ),
            Accumulate::Prod => simd::binary(
                $simd,
                $d,
                $a,
                $b,
                |s, x, y| s.mul_f64s(x, y),
                |x, y| Accumulate::Prod.apply(x, y),
            ),
            Accumulate::Worst => simd::binary($simd, $d, $a, $b, simd::max_nan, |x, y| {
                Accumulate::Worst.apply(x, y)
            }),
            Accumulate::Best => simd::binary($simd, $d, $a, $b, simd::min_nan, |x, y| {
                Accumulate::Best.apply(x, y)
            }),
        }
    };
}

/// One tile's worth of work, dispatched once onto the chosen instruction set.
struct TileRun<'a> {
    tape: &'a AllocatedTape,
    samples: MatRef<'a, f64>,
    first_column: usize,
    lanes: usize,
    /// The shape of `samples`' rows: a gather is bounded by its inputs.
    layout: RowLayout,
    file: &'a mut RegisterFile,
    faults: &'a mut [Option<LaneFault>],
}

impl WithSimd for TileRun<'_> {
    type Output = Option<(usize, LaneFault)>;

    #[inline(always)]
    fn with_simd<S: Simd>(self, simd: S) -> Self::Output {
        let TileRun {
            tape,
            samples,
            first_column,
            lanes,
            layout,
            file,
            faults,
        } = self;
        debug_assert_eq!(samples.nrows(), layout.total());

        for (pc, insn) in tape.insns.iter().enumerate() {
            match *insn {
                Instruction::Load { dst, input } => {
                    let d = file.reg_mut(dst, lanes);
                    if simd::load_strided(d, samples, input as usize, first_column) {
                        record_non_finite(faults, pc, d);
                    }
                }
                Instruction::Copy { dst, src } => {
                    let (d, s) = file.dst_a(dst, src, lanes);
                    d.copy_from_slice(s);
                }
                Instruction::Unary { dst, op, a } => {
                    let (d, a) = file.dst_a(dst, a, lanes);
                    if dispatch_unary!(simd, op, d, a) {
                        record_non_finite(faults, pc, d);
                    }
                }
                Instruction::Binary { dst, op, a, b } => {
                    let (d, a, b) = file.dst_a_b(dst, a, b, lanes);
                    if dispatch_binary!(simd, op, d, a, b) {
                        record_non_finite(faults, pc, d);
                    }
                }
                Instruction::Compare { dst, op, a, b } => {
                    let (d, a, b) = file.dst_a_b(dst, a, b, lanes);
                    let bad = simd::binary(
                        simd,
                        d,
                        a,
                        b,
                        |s, x, y| simd::compare(s, op, x, y),
                        |x, y| super::lane::compare(op, x, y),
                    );
                    if bad {
                        record_non_finite(faults, pc, d);
                    }
                }
                Instruction::Combine {
                    dst,
                    how,
                    a,
                    b,
                    last,
                } => {
                    let bad = if dst == a {
                        let (d, b) = file.dst_a(dst, b, lanes);
                        dispatch_combine!(simd, how, in_place: d, b)
                    } else {
                        let (d, a, b) = file.dst_a_b(dst, a, b, lanes);
                        dispatch_combine!(simd, how, three_address: d, a, b)
                    };
                    if last && bad {
                        record_non_finite(faults, pc, file.reg(dst, lanes));
                    }
                }
                Instruction::Check { reg } => {
                    let values = file.reg(reg, lanes);
                    if simd::any_non_finite(simd, values) {
                        record_non_finite(faults, pc, values);
                    }
                }
                Instruction::Gather { dst, index, .. } => {
                    // Per lane by nature: each lane reads its own row. Scalar.
                    let (d, index) = file.dst_a(dst, index, lanes);
                    for (lane, (v, &i)) in d.iter_mut().zip(index).enumerate() {
                        match lane::resolve_index(i, layout.inputs) {
                            Ok(position) => {
                                let x = samples[(position, first_column + lane)];
                                *v = x;
                                if !x.is_finite() {
                                    record(faults, lane, pc, FaultKind::NonFinite(x));
                                }
                            }
                            Err(kind) => {
                                *v = f64::NAN;
                                record(faults, lane, pc, kind);
                            }
                        }
                    }
                }
            }
        }

        faults
            .iter()
            .enumerate()
            .find_map(|(lane, fault)| fault.map(|f| (lane, f)))
    }
}

/// Runs a straight-line tape over `lanes` columns of `samples` starting at
/// `first_column`, on the best instruction set this machine has. Returns the
/// lowest faulted lane, if any. `layout` is the shape of `samples`' rows: a
/// computed subscript may name its inputs and nothing after them.
pub(crate) fn run_tile(
    tape: &AllocatedTape,
    samples: MatRef<'_, f64>,
    layout: RowLayout,
    first_column: usize,
    lanes: usize,
    file: &mut RegisterFile,
    faults: &mut [Option<LaneFault>],
) -> Option<(usize, LaneFault)> {
    run_tile_on(
        pulp::Arch::new(),
        tape,
        samples,
        layout,
        first_column,
        lanes,
        file,
        faults,
    )
}

/// [`run_tile`] on a given backend — the seam the tests use to force the
/// scalar one and compare.
#[expect(
    clippy::too_many_arguments,
    reason = "the tape, the batch and its row layout, the tile's place in it, and the two buffers reused across tiles: every one is a different thing, and a struct of them would be this signature with a name"
)]
pub(crate) fn run_tile_on(
    arch: pulp::Arch,
    tape: &AllocatedTape,
    samples: MatRef<'_, f64>,
    layout: RowLayout,
    first_column: usize,
    lanes: usize,
    file: &mut RegisterFile,
    faults: &mut [Option<LaneFault>],
) -> Option<(usize, LaneFault)> {
    debug_assert_eq!(faults.len(), lanes);
    arch.dispatch(TileRun {
        tape,
        samples,
        first_column,
        lanes,
        layout,
        file,
        faults,
    })
}

#[cfg(test)]
mod tests {
    //! The batched executor: tiling, fault attribution, and agreement with the
    //! per-lane executor.

    use faer::Mat;

    use super::super::tape_for;
    use super::{RegisterFile, RowLayout, TILE, run_tile, run_tile_on};
    use crate::diagnostics::EvaluationFailure;
    use crate::eval::Gradient;
    use crate::{Schema, eval};

    fn runtime(
        result: Result<faer::Col<f64>, EvaluationFailure>,
    ) -> Box<crate::diagnostics::RuntimeProblem> {
        match result {
            Err(EvaluationFailure::Runtime(problem)) => problem,
            other => panic!("expected a runtime problem, got {other:?}"),
        }
    }

    /// Sebastiano Vigna's SplitMix64, so the random rows below are the same on
    /// every machine and every `rand` version.
    struct SplitMix64(u64);

    impl SplitMix64 {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        #[expect(clippy::cast_precision_loss, reason = "53 bits fit a mantissa")]
        fn uniform(&mut self, low: f64, high: f64) -> f64 {
            low + (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 * (high - low)
        }
    }

    /// One expression per instruction shape, over `x1..x4`.
    const SOURCES: &[&str] = &[
        "x1 + x2",
        "x1 * x1 - x2 / x3",
        "x1 + x2 > 20 - x3^2",
        "sin(x1) * cos(x2) + sqrt(abs(x3))",
        "(((((x1 + x2) * x3 - x4) / (x1 + 1) + x2) * x3 - x4) / (x1 + 1) + x2) * x3 - x4",
        "max(x1, x2) - min(x3, x4)",
        "x1 == x2 +/- 0.5",
        "ln(x1) < 2",
        "x1 % x2",
        "x1 ^ x3",
        "sgn(x1) + floor(x2) + ceil(x3)",
        "sum(1, 4, i -> var[i] * i)",
        "var x = x1 * 2;\nvar y = x + x2;\ny * y - x",
        "prod(1, 3, i -> x1 + i)",
    ];

    /// Permanent: the two executors are two loops over one instruction set, and
    /// this is what says they stayed that way.
    #[test]
    fn the_lane_and_tile_executors_agree_bit_for_bit_on_random_inputs() {
        let mut rng = SplitMix64(0x7113_1A4E_0000_0001);
        let names = ["x1", "x2", "x3", "x4"];
        for source in SOURCES {
            let ast = crate::parse(source).expect("sources compile");
            let expression = eval::bind(&ast, &Schema::new(names), Gradient::Never).expect("binds");
            let rows: Vec<Vec<f64>> = (0..64)
                .map(|_| -> Vec<f64> { (0..names.len()).map(|_| rng.uniform(0.5, 10.0)).collect() })
                .filter(|row| expression.eval_row(row).is_ok())
                .collect();
            if rows.is_empty() {
                continue;
            }
            let batch = Mat::from_fn(names.len(), rows.len(), |r, c| rows[c][r]);
            let tiled = expression
                .eval(batch.as_ref())
                .expect("every row passed per lane");
            for (column, row) in rows.iter().enumerate() {
                let lane = expression.eval_row(row).unwrap();
                assert_eq!(
                    tiled[column].to_bits(),
                    lane.to_bits(),
                    "{source:?} on {row:?}"
                );
            }
        }
    }

    #[test]
    fn a_batched_fault_names_the_lowest_column_and_the_innermost_node() {
        let ast = crate::parse("ln(x1) + x2").unwrap();
        let expression = eval::bind(&ast, &Schema::new(["x1", "x2"]), Gradient::Never).unwrap();
        // Column 3 faults at `ln`; column 5 faults at the addition's input.
        let mut batch = Mat::from_fn(2, 8, |_, _| 1.0);
        batch[(0, 3)] = 0.0;
        batch[(1, 5)] = f64::NAN;
        let problem = runtime(expression.eval(batch.as_ref()));
        assert_eq!(problem.sample, Some(3));
        assert_eq!(problem.problem.span, crate::diagnostics::Span::new(0, 6));
        assert_eq!(
            problem.problem.kind,
            crate::diagnostics::ProblemKind::NonFiniteValue {
                value: f64::NEG_INFINITY
            }
        );
    }

    /// `atan` would absorb the infinity; the walker faulted on the product and so
    /// must the tile, which is why every instruction is checked.
    #[test]
    fn an_absorbed_infinity_is_still_a_fault() {
        let ast = crate::parse("atan(x1 * x1)").unwrap();
        let expression = eval::bind(&ast, &Schema::new(["x1"]), Gradient::Never).unwrap();
        let mut batch = Mat::from_fn(1, TILE, |_, _| 1.0);
        batch[(0, 100)] = 1e200;
        let problem = runtime(expression.eval(batch.as_ref()));
        assert_eq!(problem.sample, Some(100));
        assert_eq!(problem.problem.span, crate::diagnostics::Span::new(5, 12));
    }

    #[test]
    fn a_faulted_lane_does_not_disturb_its_neighbours() {
        let tape = tape_for("sqrt(x1) * 2", &["x1"]);
        let batch = Mat::from_fn(1, 4, |_, c| [4.0, -1.0, 9.0, 16.0][c]);
        let mut file = RegisterFile::new(&tape, 4);
        let mut faults = vec![None; 4];
        let fault = run_tile(
            &tape,
            batch.as_ref(),
            RowLayout {
                inputs: 1,
                intermediates: 0,
            },
            0,
            4,
            &mut file,
            &mut faults,
        );
        assert_eq!(fault.map(|(lane, _)| lane), Some(1));
        let out = file.reg(tape.result, 4);
        assert_eq!([out[0], out[2], out[3]], [4.0, 6.0, 8.0]);
        assert!(out[1].is_nan());
    }

    #[test]
    fn a_wide_batch_is_tiled_and_reassembled_in_order() {
        let ast = crate::parse("x1 * 2").unwrap();
        let expression = eval::bind(&ast, &Schema::new(["x1"]), Gradient::Never).unwrap();
        let columns = 2 * TILE + 7;
        #[expect(clippy::cast_precision_loss, reason = "small counts")]
        let batch = Mat::from_fn(1, columns, |_, c| c as f64);
        let residuals = expression.eval(batch.as_ref()).unwrap();
        for c in 0..columns {
            assert_eq!(residuals[c], batch[(0, c)] * 2.0);
        }
    }

    #[test]
    fn the_sample_index_survives_tiling() {
        let ast = crate::parse("ln(x1)").unwrap();
        let expression = eval::bind(&ast, &Schema::new(["x1"]), Gradient::Never).unwrap();
        let mut batch = Mat::from_fn(1, 2 * TILE, |_, _| 1.0);
        batch[(0, TILE + 3)] = 0.0;
        assert_eq!(
            runtime(expression.eval(batch.as_ref())).sample,
            Some(TILE + 3)
        );
    }

    #[test]
    fn an_empty_batch_yields_an_empty_column() {
        let ast = crate::parse("x1 * 2").unwrap();
        let expression = eval::bind(&ast, &Schema::new(["x1"]), Gradient::Never).unwrap();
        let batch = Mat::<f64>::zeros(1, 0);
        assert_eq!(expression.eval(batch.as_ref()).unwrap().nrows(), 0);
    }

    /// The vectorised `maxnum` and the scalar one may not agree on which zero
    /// wins; the two executors must.
    #[test]
    fn max_and_min_agree_on_signed_zeros_between_executors() {
        for source in ["max(x1, x2)", "min(x1, x2)"] {
            let ast = crate::parse(source).unwrap();
            let expression = eval::bind(&ast, &Schema::new(["x1", "x2"]), Gradient::Never).unwrap();
            let rows = [[-0.0, 0.0], [0.0, -0.0], [0.0, 0.0], [-0.0, -0.0]];
            let batch = Mat::from_fn(2, rows.len(), |r, c| rows[c][r]);
            let tiled = expression.eval(batch.as_ref()).unwrap();
            for (c, row) in rows.iter().enumerate() {
                let lane = expression.eval_row(row).unwrap();
                assert_eq!(tiled[c].to_bits(), lane.to_bits(), "{source} on {row:?}");
            }
        }
    }

    /// The scalar backend and whatever `pulp::Arch::new()` picked run the same
    /// tape to the same bits, faults included. On CI's AVX2 runners this is the
    /// vector kernels against their scalar tails; on a machine without AVX2 the
    /// two coincide, which is fine.
    #[test]
    fn the_scalar_and_dispatched_backends_agree_bit_for_bit_on_random_inputs() {
        let mut rng = SplitMix64(0x5CA1_A2AB_1E00_0001);
        let names = ["x1", "x2", "x3", "x4"];
        for source in SOURCES {
            let tape = tape_for(source, &names);
            // Wide enough to cross a tile boundary, and with a few rows pushed
            // out of every operator's domain so that faults have to agree too.
            let columns = TILE + 37;
            let batch = Mat::from_fn(names.len(), columns, |_, c| {
                if c % 41 == 0 {
                    -rng.uniform(0.5, 10.0)
                } else {
                    rng.uniform(0.5, 10.0)
                }
            });
            let mut detected = RegisterFile::new(&tape, columns);
            let mut scalar = RegisterFile::new(&tape, columns);
            let mut detected_faults = vec![None; columns];
            let mut scalar_faults = vec![None; columns];
            let a = run_tile_on(
                pulp::Arch::new(),
                &tape,
                batch.as_ref(),
                RowLayout {
                    inputs: names.len(),
                    intermediates: 0,
                },
                0,
                columns,
                &mut detected,
                &mut detected_faults,
            );
            let b = run_tile_on(
                pulp::Arch::Scalar,
                &tape,
                batch.as_ref(),
                RowLayout {
                    inputs: names.len(),
                    intermediates: 0,
                },
                0,
                columns,
                &mut scalar,
                &mut scalar_faults,
            );
            // Rendered, because a fault can carry a NaN and NaN is not equal to
            // itself.
            assert_eq!(
                format!("{a:?}"),
                format!("{b:?}"),
                "{source:?}: first fault"
            );
            assert_eq!(
                format!("{detected_faults:?}"),
                format!("{scalar_faults:?}"),
                "{source:?}: fault table"
            );
            let (x, y) = (
                detected.reg(tape.result, columns),
                scalar.reg(tape.result, columns),
            );
            for c in 0..columns {
                if detected_faults[c].is_none() {
                    assert_eq!(x[c].to_bits(), y[c].to_bits(), "{source:?}, column {c}");
                }
            }
        }
    }

    // ----------------------------------------------------------------- holds

    /// A faulting column does not hold; its neighbours are judged on their own.
    #[test]
    fn a_faulting_column_does_not_hold_and_its_neighbours_are_judged() {
        let ast = crate::parse("ln(x1) + x2 <= 1").unwrap();
        let expression = eval::bind(&ast, &Schema::new(["x1", "x2"]), Gradient::Never).unwrap();
        // x1 = 1..8 so ln(x1) + 1 <= 1 holds only at x1 = 1; column 3 faults at
        // `ln(0)`, column 5 carries a NaN input.
        let mut batch = Mat::from_fn(2, 8, |r, c| {
            if r == 0 {
                [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0][c]
            } else {
                1.0
            }
        });
        batch[(0, 3)] = 0.0;
        batch[(1, 5)] = f64::NAN;
        let mut holds = vec![true; 8];
        expression.holds(batch.as_ref(), &mut holds).unwrap();
        assert_eq!(
            holds,
            [true, false, false, false, false, false, false, false]
        );
    }

    /// Judged from the fault table, not the value: the product faulted even
    /// though `atan` would have made the residual finite, and negative, again.
    #[test]
    fn an_absorbed_infinity_does_not_hold() {
        let ast = crate::parse("atan(x1 * x1) < 2").unwrap();
        let expression = eval::bind(&ast, &Schema::new(["x1"]), Gradient::Never).unwrap();
        let mut batch = Mat::from_fn(1, TILE + 5, |_, _| 1.0);
        batch[(0, TILE + 2)] = 1e200;
        let mut holds = vec![true; TILE + 5];
        expression.holds(batch.as_ref(), &mut holds).unwrap();
        assert!(!holds[TILE + 2]);
        assert!(holds[TILE + 1]);
    }

    /// `holds` only ever narrows: a column already ruled out stays ruled out.
    #[test]
    fn holds_only_narrows() {
        let ast = crate::parse("x1 <= 5").unwrap();
        let expression = eval::bind(&ast, &Schema::new(["x1"]), Gradient::Never).unwrap();
        let batch = Mat::from_fn(1, 4, |_, c| [1.0, 2.0, 3.0, 9.0][c]);
        let mut holds = vec![true, false, true, true];
        expression.holds(batch.as_ref(), &mut holds).unwrap();
        assert_eq!(holds, [true, false, true, false]);
    }

    #[test]
    fn holds_rejects_a_width_mismatch() {
        let ast = crate::parse("x1 + x2").unwrap();
        let expression = eval::bind(&ast, &Schema::new(["x1", "x2"]), Gradient::Never).unwrap();
        let wrong_rows = Mat::from_fn(3, 4, |_, _| 1.0);
        assert!(matches!(
            expression.holds(wrong_rows.as_ref(), &mut [true; 4]),
            Err(EvaluationFailure::RowWidthMismatch { .. })
        ));
        let right = Mat::from_fn(2, 4, |_, _| 1.0);
        assert!(matches!(
            expression.holds(right.as_ref(), &mut [true; 3]),
            Err(EvaluationFailure::RowWidthMismatch { .. })
        ));
    }
}

# Performance records

One CSV per benchmark case, in the format the other repos use: `sep=;` so Excel
opens them without a wizard, padded columns so a `git diff` is readable, and a
`*` on any figure that is not directly comparable to the rows around it.

A file is one measurement's history, read top to bottom.

| file | expression | vars | |
|---|---|---|---|
| [`add-two-vars.csv`](add-two-vars.csv) | `x1 + x2` | 2 | the dispatch floor — tree-walk overhead with no arithmetic worth speaking of |
| [`compare-with-square.csv`](compare-with-square.csv) | `x1 + x2 > 20 - x3^2` | 3 | lifted verbatim from the JVM `PerformanceFixture` |
| [`transcendental-mix.csv`](transcendental-mix.csv) | `sin(x1)*cos(x2)+sqrt(abs(x3))` | 3 | libm cost, which no tape work removes |
| [`deep-arithmetic.csv`](deep-arithmetic.csv) | a nested arithmetic chain | 4 | recursion depth, against a flat tape later |
| [`sum-200-squares.csv`](sum-200-squares.csv) | `sum(1, 200, i -> var[i]^2 - 3.0)` | 200 | lifted verbatim from the JVM fixture; unrolls to a 200-term fold |
| [`near-equality.csv`](near-equality.csv) | `x1 == x2 +/- 0.001` | 2 | the tolerance equality, desugared by `eval::irgen` into two comparisons and a fold; first row at `0.2.0` |

Written by `tests/throughput_benchmarks.rs` on `just bench`.

The `brute-*.csv` ledgers are the brute squad's and are described [below](#brute-squad-ledgers).

## Columns

`version;timestamp;host;vars;batch-1;batch-256;map`

`batch-1` and `batch-256` are `CompiledExpression::eval(MatRef)` at those batch
widths, in points per millisecond, **so larger is better** — the opposite sense
to the optimizer and metamodel ledgers, where the numbers are errors. `map` is
the JVM's `BabelExpression.evaluate(Map)`.

Two widths because the gap between them is what batching buys. A width of one is
the degenerate batch — everything the old per-call API did, still done per call —
and 256 is realistic. A tree walk amortises two buffer allocations across the
batch and nothing else; a tape should amortise the traversal itself, so this
ratio is the number to watch as that lands.

**The version column restarts at `0.1.0`** where the crate was renamed to sojourn.
Rows at `0.1.0` sit below the `2.1.0-native` rows and are later, not earlier: a file
is read top to bottom.

**Rows before `2.1.0-native` hold different measurements in those two
positions.** Until then the columns were `bound` and `naive`: a scalar
`evaluate(&[f64])` after binding once, and a convenience method that rebuilt a
`Schema` on every call. Both were deleted when evaluation went batch-only, so
those figures are history rather than a baseline — comparable to each other, not
to what follows.

## How writes work

**Upsert on version and host**: if the last row carries the current
`CARGO_PKG_VERSION` *and* this host, it is replaced, otherwise the row is
appended. Host is part of the key because a row from another machine at the
same version is a different experiment, not a stale reading. A tuning
session therefore leaves one row rather than twenty, and bumping the version is
what turns the current row into a historical one. A missing file is created, so
adding a case to `cases()` needs no setup here.

**Release only.** The same test runs under `cargo nextest run` in debug at a
twentieth of the workload, and under upsert that would not merely add a bad row,
it would overwrite the good one.

## Reading these honestly

These record wall-clock throughput, which the quality ledgers in the other repos
do not, and that brings hazards those files do not have.

**A number is only comparable within a host.** Hence the `host` column. [`hosts/README.md`](hosts/README.md) says which machine a host string is, and the benchmarks write a short description of each under `hosts/`; the writers take `SOJOURN_HOST` over `COMPUTERNAME`, so a machine with an unhelpful name can label its rows. A figure
from a different machine — or a build agent, or a thermally-throttled laptop —
is a different experiment.

**The noise floor is about 30%, and a single row is not evidence.** Measured on
`BATOU` over seven consecutive runs at one unchanged commit:

| case | min | max | spread |
|---|---|---|---|
| add-two-vars | 17069 | 20388 | 19% |
| compare-with-square | 9254 | 12088 | 31% |
| transcendental-mix | 9113 | 11856 | 30% |
| deep-arithmetic | 3035 | 3889 | 28% |
| sum-200-squares | 102.1 | 133.3 | 31% |

The harness already takes the best of three 400 ms windows, so that is *after*
the obvious mitigation. And the spread is not symmetric noise: **later runs in a
sequence come out faster across the board**, so two figures taken minutes apart
are biased against each other rather than merely noisy.

What follows:

- **Compare medians of several runs, not single rows.** Each row is one run and
  carries the whole spread.
- **Use an untouched case as a control.** A change to `^` handling cannot affect
  `transcendental-mix`, which contains no `^` — so whatever *that* moved by is
  drift, and the real effect is what survives subtracting it. That caught a
  "+32%" claim that turned out to be one lucky reading.
- **A regression under ~30% will not be visible here.** Getting below that needs
  pinned cores and a quiet machine, and has not been worth doing yet.

That method has since paid out as well as caught. Moving the boolean lowering
into `eval` removes one node per comparison, which is far too small to read off
a single row — but measured as five runs against five on the stashed parent
commit, `compare-with-square` went 12856 → 15774 while the control
`add-two-vars` moved 35318 → 36957. The two sets of five do not overlap on the
target (11661–13226 against 14538–16273) and overlap almost entirely on the
control, so the ~23% is real and about ~17% of it survives subtracting drift.
**Stash the parent, measure both in one sitting.** A figure from an earlier
session is a different experiment even on the same host.

**Some changes are invisible to this suite.** No case contains a constant
subexpression, so `rewrite::fold_constants` cannot show up at all. Before
measuring a change, check that a case exercises it.

## The reference rows

Not produced by a run, and carried forward from `docs/todo.md`:

- **`perf-fixture-jvm*`** — the original `~10k evals/sec`, and the reason for the
  star. `PerformanceFixture` built its input `Map` *inside* the timed loop,
  called `print(".")` inside it as well, and warmed up for fifty iterations where
  tiered HotSpot wants ten thousand. It measures map allocation and console I/O
  in a largely interpreted tier. Kept as the record of what that number meant,
  not as a baseline.
- **`jvm-11-map`** — the real JVM figures, from `ThroughputBenchmarks.kt` run in
  a throwaway `git worktree` at `db9add8^`, the last commit where the JVM tree
  built. It does **not** build on this branch: `db9add8` commented out four
  `locals [...]` declarations in `BabelParser.g4` that `rewriters.kt` needs. So
  these rows cannot be reproduced without restoring that grammar, and if the JVM
  tree is deleted they become the only surviving record of it.
- **`2.0.6-native`** — the Rust wave-1 baseline, recorded in `docs/todo.md`
  before these ledgers existed.

## Quality ledgers

`quality-<fixture>-<count>.csv`, written by `tests/sampling_quality.rs` on
`just quality`. These measure how good a design is, not how fast: each
fixture is a region whose uniform distribution is drawn exactly by a sampler
written without sojourn, and `sojourn::quality` scores `sample`'s designs
against twenty thousand of those draws. The plan and the fixtures are in
[`docs/sampling-quality.md`](../docs/sampling-quality.md).

Columns: `version;timestamp;host;dims;count;seeds;strata-mean;strata-worst;levels-worst;cl2-ratio-mean;cl2-ratio-worst;min-dist-mean;min-dist-worst;cover-mean;cover-worst;missing-worst;random-cl2-ratio;random-min-dist;random-cover;pool-size;pool-strata;pool-cl2-ratio;pool-cover;pool-missing`

| measure | better | random scores |
|---|---|---|
| `strata`: fraction of the reference marginal's `n` strata holding a point, mean over continuous axes | higher; `1.0` is Latin | `1 - (1 - 1/n)^n`, about 0.63 |
| `levels`: worst gap between a level's share in the design and in the reference | lower | — |
| `cl2-ratio`: centred-L2 discrepancy over what random scores | lower | `1.0` by construction |
| `min-dist`: smallest pairwise distance, box-normalised | higher | the `random-min-dist` column |
| `cover`: farthest a reference point lies from the design | lower | the `random-cover` column |
| `missing`: pieces of the region with no design point | lower | — |

**Ten seeds, the mean and the worst.** Every row is ten designs, one per
generator seed `0..10`, carried from `solve` into `sample`. A `-mean` column
is their mean, and a `-worst` column the worst seed in whichever direction is
worse for that measure. The `random-*` columns are the means of ten random
designs of the same count drawn from the exact sampler: the control. The
`pool-*` columns score the candidates `sample` chose from (`pool-size` their
mean count): no design covers better than its pool, and a piece the pool
misses every chooser misses.

**Deterministic, so no wall-clock caveats.** The same seed gives the same
number on every machine, and the noise is the spread across seeds, which the
worst column is. A change of generator or of how it is used should land
inside that band; one that moves the band is a finding. The test holds each case's worst seed to a floor pinned from the first rows
(`Floor` in `tests/sampling_quality.rs`), after writing the row. Host is still in the
key, for the upsert rule. Release only, because the reference's kernel sum is
quadratic in its points, not because a debug number would differ.

**A random design's CL2 ratio is heavy-tailed.** It is a weighted χ² of mean
one, dominated by a few low-order projections, so its median sits well below
the mean and a mean of ten strays. The gearbox's control read 0.47 over ten,
while two hundred hold within a tenth of one. The test checks the measures on
two hundred; the ledger's `random-cl2-ratio` is the first ten of them, for the
like-for-like comparison with the design.

## Brute-squad ledgers

Written by `tests/brute_squad.rs` on `just brute` or `just bench`. One file per
constraint family, all three over the unit cube at a feasible fraction of one
in a hundred — the cost of a check does not depend on how rare a hit is.

| file | family | sources |
|---|---|---|
| [`brute-corner.csv`](brute-corner.csv) | corner | `x_i > 1 - q` for `i = 1..3`, three constraints |
| [`brute-ball.csv`](brute-ball.csv) | ball | `x1^2 + x2^2 + x3^2 < r^2`, one constraint |
| [`brute-sine-corner.csv`](brute-sine-corner.csv) | sine corner | `sin(x_i) > sin(1 - q)`, three constraints, the one no solver can be asked about |
| [`brute-real-power.csv`](brute-real-power.csv) | real power | `x1^1.234 < p^1.234`, one constraint, a non-integer `pow` |

The `brute-gpu-*.csv` ledgers are the same three families through the GPU
sieve, written only when an adapter is present. Two rates, in **candidates per
second**: `given` uploads a million-column batch the CPU drew and sieves it in
`f32` (the evaluator plus the upload), `generated` has the device draw its own
four million candidates per dispatch, which is what brute force runs.
Survivors are not re-judged in either. They carry an `adapter` column beside
`host`, because the number belongs to the GPU and its driver, not the
machine: an iGPU and a discrete card in the same box are different rows, and
so is the same card after a driver update.

Columns: `version;timestamp;host;vars;batch;eval-only;pipeline`.

The unit is **constraint checks per second** — one check is one column of a
1024-wide batch judged against every constraint of its family — so, like the
evaluator ledgers and unlike the error ledgers elsewhere, **larger is better**.
It is not the same unit as the evaluator ledgers' points per millisecond: a
check here is up to three evaluations plus the column-wise `and`, and the
numbers are per second, not per millisecond.

`eval-only` evaluates batches prepared up front. `pipeline` refills each batch
with fresh uniform samples first, through the pool's own `fill_box`, so the gap
between the two is what the random-number generation costs. The brute-squad plan says the CPU target is a
pipeline problem rather than an evaluator problem; these two columns are how
that claim gets checked.

Reference points from `docs/brute-squad.md`: one million checks per second
is the stated CPU target, and about forty million is what the tree-walker
manages on a trivial expression at batch 256 on `BATOU`. Same upsert rule,
same release-only guard, same noise floor and same host caveat as above.

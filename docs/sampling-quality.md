# Sampling quality — measuring designs before changing how they are chosen

*Plan, written 2026-10-21. Step 1 has landed; steps 2 and 3 are not started. Named by the idea,
as `docs/brute-squad.md` and `docs/knothe-rosenblatt.md` are. Measurements go into
`docs/todo.md` part two as they land.*

## Context

`FeasibleRegion::sample` (src/solve.rs, engine in `cvg::design`) is farthest-first over a pool:
the opening's points, a round of uniform proposals, and hit-and-run from the region's chains.
The suite checks it with thresholds: `cvg_pools::the_pieces_of_a_region_each_receive_a_point`,
`cvg_discrete::a_gearbox_gives_every_ratio_a_point`, and `cvg_equalities`' `Case::coverage` and
`Case::occupancy`. Those say whether a design is broken. Nothing says how good one is, nothing is
recorded across commits, and the distributional oracles left with `cvg_benchmarks.rs` on
2026-09-16. A change that made every design worse while passing every threshold would go
unseen.

Other ways of choosing a design are coming: a caller choosing from the pool by another criterion
(a discrepancy rather than a distance), and the open question of whether `sample` should offer a
choice of criterion at all. Before any of that, the quality of what ships today is measured and
recorded, on regions whose true distribution is known in closed form.

**Outcome:** a public `sojourn::quality` of pure functions; a fixture suite of closed-form
regions with exact samplers written without sojourn; one ledger per fixture and count in
`performance-records/`, each value a mean over ten generator seeds with the worst seed beside
it; a random control in every row.

## Why the reference must not be sojourn's

On a constrained region the only reference sojourn has is its own pool, and the pool comes from
the machinery the design does. A walker that never reaches a corner grades its design well
against a pool that never reached it either. So the benchmark fixtures are regions whose uniform
distribution can be drawn exactly by a few lines of arithmetic: `r = √u` on a disc, normalised
exponentials on a simplex, a shear on a band. Those draws are the reference. They are checked
themselves first: every reference point passes `ConstraintSystem::is_feasible`, and the disc's
and simplex's marginals pass a KS test against their closed-form CDFs.

## The measures

All in box-normalised coordinates, `(x - lower) / width` per variable, the scale `repair` lands
by and a design is spread by.

| measure | what it catches | better |
|---|---|---|
| `strata`: per continuous axis, the fraction of the `n` equal-probability strata of the *reference's* marginal that hold a point | a frozen or bunched coordinate; `1.0` is Latin on that axis, random scores `1 - (1 - 1/n)^n` | higher |
| `level_shares`: per discrete axis, the largest gap between the design's share at a level and the reference's | a level starved or flooded | lower |
| `cl2`: Hickernell's centred L2 discrepancy, against the box in closed form or against a sample as an MMD; reported as a ratio to what `n` random draws from the reference score | unevenness overall, weighted toward one- and two-axis projections | lower; `1.0` is random |
| `min_distance`: the smallest pairwise distance | clumping, duplicates | higher |
| `covering_radius`: the farthest a reference point lies from its nearest design point | a part of the region the design missed: "did it hit every part of the region" as a number | lower |
| `allocation`: per labelled piece, missing pieces and the worst shortfall against the piece's share | a whole component skipped | lower |

**Why CL2 and not a grid count.** A grid of `k^d` cells is empty at a hundred points in twenty
dimensions, and `todo.md` already records occupancy saturating past three. CL2's kernel is a
product over axes, so its square decomposes into a weighted sum over every projection of the
design onto a subset of axes, the low-order ones weighted most. That is what a design at
`n < d` is for: an additive or low-order surrogate learns from the one- and two-axis projections.

**Why the ratio.** CL2² scales as `(13/12)^d`. Dividing by the expectation for `n` independent
draws from the reference, `(E k(Y,Y) - E k(Y,Y′)) / n`, puts every case on one scale: `1.0` is
random, lower is better, whatever the dimension or count.

**A correction this measurement licenses.** The crate's docs say a design of fewer points than
variables "has no useful stratification" (`FeasibleRegion::sample`, `cvg::design`, `AGENTS.md`
on designs). That overstates it. A Latin hypercube's guarantee is one-dimensional at every ratio
of points to dimensions: each axis still has `n` strata with one point each, whatever `d` is.
What is true is that the guarantee is *only* one-dimensional, and on a constrained region the
strata have to be the region's marginals rather than the box's. Rewrite those sentences in step 2,
with the numbers.

## Fixtures

| fixture | system | exact draw | counts | catches |
|---|---|---|---|---|
| box-2d, box-20d | no constraints | uniform | 10, 100 | the control; 100 in 20D is the regime that matters |
| disc | `x^2 + y^2 < 1` on `[-1,1]²` | `r = √u`, angle uniform | 10, 100 | a curved boundary |
| simplex-5d, simplex-20d | `x1 + … + xd <= 1` on `[0,1]^d` | Dirichlet(1, …, 1), slack dropped | 10, 100 / 100 | skewed marginals; 1/20! of its box, so walked |
| two-pieces | `abs(x1 - 0.2) > 0.6` on `[-1,1]×[0,1]` | pieces of width 0.6 and 0.2, mass 3:1 | 10, 100 | allocation across a gap |
| band | `x1 == x2 +/- 0.001` on `[0,1]²` | a shear of `[0,1]×[-t,t]` | 10, 100 | a thin region, a driven coordinate |
| gearbox | `cvg_discrete`'s gearbox at a band of 5 | a ratio by its speed range's width (5:10:20), speed uniform in it | 12, 100 | level shares, one piece per level |

Each reference is 20 000 draws. The empirical CDF is then within about 1/140 of the true one, a
little under a stratum's width at a hundred points. The kernel's sum over every pair, `O(m² · d)`,
is what bounds `m`: summed exactly once, so a design is compared against the sample and not
against an estimate of it.

## The ledger

`performance-records/quality-<fixture>-<count>.csv`, upserted on version and host as every
ledger is. Each run solves and samples on ten seeds (`seed_from_u64(0..10)`, one generator
carried from `solve` into `sample`, the way a caller would) and draws a random control of the
same count from the exact sampler on separate streams. The columns are the design's mean and
worst over the ten for each measure, then the control's means:

`dims;count;seeds;strata-mean;strata-worst;levels-worst;cl2-ratio-mean;cl2-ratio-worst;min-dist-mean;min-dist-worst;cover-mean;cover-worst;missing-worst;random-cl2-ratio;random-min-dist;random-cover`

The numbers are deterministic per seed and the same on every machine. The noise is the spread
across seeds, not the wall clock's thirty percent, and the worst column is that spread. A change
of generator, or of how the generator is used, should land inside the old band; one that moves it
is a finding. A pass/fail floor, when one is pinned, is judged on the worst seed, never the mean:
a mean hides one bad seed, and a bad seed is a design somebody receives.

The control is asserted on, over two hundred random designs per case: they must average a CL2 ratio within 0.15 of one and fill within 0.02 of
`1 - (1 - 1/n)^n` of their strata. That checks the measures, not sojourn. The design gets no
floors in step 1.

`just quality` runs it, in release. The fixtures' own checks run in debug with the rest of the
suite.

## Steps

1. **The measures and the fixtures.** `src/quality.rs` with hand-computed unit tests (a centred
   point's CL2² is 1/12; a midpoint grid agrees with the closed form to `2/m`; the diagonal is
   Latin and worse than a knight's-move square; a duplicate has distance zero; the corners cover
   the centre at `√½`). `tests/sampling_quality.rs` with the fixtures, their checks, and the
   ledger. *Landed 2026-10-21; first measurements below.*
2. **A baseline, and floors.** Read the first rows; pin floors at roughly 0.8× the worst seed
   (the knothe-rosenblatt plan's rule); rewrite the "no useful stratification" sentences; record
   in `todo.md` part two what farthest-first does in high dimension. The expectation, unconfirmed:
   maximin over a pool in twenty dimensions is pushed toward the box's faces and its
   one-dimensional projections bunch, which shows up as low strata and a CL2 ratio near or above
   one.
3. **`FeasibleRegion::draw` and the pool's own quality.** The candidate pool `cvg::design`
   already builds, returned without the selection, so a caller can choose from it by another
   criterion. Measured with the same fixtures: whatever is chosen from a pool can be no better
   than the pool, and a pool that misses a piece of the region loses it for every criterion.
   Before it lands, settle what has to come out with it. Today the selection decides things a
   caller would otherwise reinvent:
   - a small lattice is enumerated, and a short design is then `SampleError::Exhausted`, a proof;
   - a short design otherwise is `Degenerate`, decided at `farthest <= 0`;
   - `existing` points are spread from, which a caller choosing from a pool must also do;
   - the pool's size, `2n + 128`, is tuned for farthest-first.
   The alternative seam, if a pool alone does not compose, is a chooser handed to `sample` as a
   `&dyn` over (pool, existing, count), which keeps those semantics here. Decide with step 2's
   numbers in hand.

Later, not planned here: `sample` returning a `Quality` against its own pool, once the pool is
public and the closed-form fixtures have shown how far a pool-referenced number can be trusted.

## First measurements

*2026-10-21, `BATOU`, sojourn 0.3.0, `just quality` (62 s). Each cell is the design's mean
over ten seeds, the ten-seed random control in parentheses.*

| case | strata | CL2 ratio | min distance | covering radius |
|---|---|---|---|---|
| box-2d, 10 | 0.58 (0.67) | 0.80 (0.93) | 0.319 (0.081) | 0.308 (0.497) |
| box-2d, 100 | 0.63 (0.63) | 0.37 (1.23) | 0.080 (0.007) | 0.093 (0.184) |
| box-20d, 10 | 0.64 (0.67) | 1.97 (0.96) | 1.906 (1.234) | 2.057 (2.020) |
| box-20d, 100 | 0.62 (0.63) | 1.89 (1.00) | 1.472 (0.966) | 1.718 (1.659) |
| disc, 10 | 0.73 (0.63) | 0.52 (0.94) | 0.289 (0.083) | 0.285 (0.429) |
| disc, 100 | 0.66 (0.64) | 0.24 (1.14) | 0.069 (0.005) | 0.082 (0.158) |
| simplex-5d, 10 | 0.64 (0.65) | 1.14 (1.07) | 0.371 (0.155) | 0.477 (0.673) |
| simplex-5d, 100 | 0.63 (0.65) | 1.07 (1.01) | 0.151 (0.047) | 0.296 (0.430) |
| simplex-20d, 100 | 0.63 (0.63) | 1.45 (0.95) | 0.195 (0.119) | 0.337 (0.353) |
| two-pieces, 10 | 0.55 (0.67) | 0.72 (0.80) | 0.268 (0.058) | 0.249 (0.502) |
| two-pieces, 100 | 0.62 (0.63) | 0.51 (1.38) | 0.053 (0.004) | 0.061 (0.134) |
| band, 10 | 0.90 (0.69) | 0.19 (0.87) | 0.098 (0.013) | 0.099 (0.256) |
| band, 100 | 0.86 (0.64) | 0.13 (0.81) | 0.009 (0.000) | 0.017 (0.041) |
| gearbox, 12 | 0.78 (0.63) | 0.28 (0.97) | 0.025 (0.002) | 0.025 (0.163) |
| gearbox, 100 | 0.89 (0.64) | 0.36 (0.47) | 0.003 (0.000) | 0.003 (0.011) |

No design missed a piece: two-pieces and gearbox show `missing-worst` 0 at every count. One
random gearbox control of twelve missed a ratio.

What it says, for step 2 to confirm and act on:

- **In two dimensions, farthest-first does what it is for.** Minimum distance is ten times
  random's, the covering radius half, and the CL2 ratio well under one at a hundred points.
- **In twenty dimensions it loses to random on discrepancy.** CL2 ratio 1.9 on the box and 1.45
  on the simplex, against random's 1.0. It still spreads points apart (minimum distance half as
  large again), but its covering radius is no better than random's. The suspicion this was
  written to test is borne out: maximin's projections are worse than random's.
- **It is never Latin where it has room to be.** Strata sit at random's 0.63 on every box, disc
  and simplex. The band and the gearbox score higher only because their continuous room is one
  long thin stretch, where spreading apart is the same as stratifying.
- **The 20-D cases are the usual regime for an opening design,** a hundred points in twenty
  dimensions. That is where another criterion has the most to win.

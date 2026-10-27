# Sojourn — notes for agents

Sojourn is a constrained random vector generator: given a box and a set of constraints
it produces random points that satisfy them, by sampling, by walking, by a local solve,
and by interval branch-and-prune.
Constraints are written in babel, a small expression language: `x1 + x2 * cos(x3)^2`
is a transform, `x1 < x2 + x3` is a constraint. Babel began as Kotlin/ANTLR on the JVM
(EmpowerOps' optimizer used it) and was ported to Rust here; the generator began as the
university project sojourn-CVG and is `crate::cvg`.

Read these before changing anything, in this order:

1. [`src/README.md`](src/README.md) — the architecture:
   one `Ast`, a meaning-preserving front end, two backends (`eval`, `cvg`).
2. [`docs/todo.md`](docs/todo.md) — the roadmap *and* the reasoning: measurements, dead ends,
   and the decisions that are not recoverable from the code. Part two is long on
   purpose. Add to it when you learn something the code cannot say. Entries that
   events overtook move to [`docs/todo-overtaken.md`](docs/todo-overtaken.md),
   verbatim, with a line on why.
3. [`performance-records/README.md`](performance-records/README.md)
   — how to read and write a throughput number honestly.
4. [`docs/brute-squad.md`](docs/brute-squad.md) — the plan for wide-batch
   sampling (IR tape, CPU vectorisation, wgpu). Owns the "sample harder" tier;
   `docs/todo.md` owns the solver and equality-constraint side.
5. [`docs/sampling-quality.md`](docs/sampling-quality.md) — how good a design
   is, measured against regions whose distribution is known in closed form:
   `sojourn::quality`, the fixtures, and the ledgers. Read before changing how
   `sample` chooses points.

## Layout

| path | what | status |
|---|---|---|
| `Cargo.toml`, `src/`, `tests/`, `templates/` | the Rust crate, at the repository root. One package and no workspace; when a second crate appears (an FFI `cdylib`, say) it gets a sibling directory and the root `Cargo.toml` gains a `[workspace]` table. | live |
| `src/lib.rs`, `src/nodes.rs`, `src/system.rs`, `src/solve.rs`, `src/repair.rs` | the public API, as files: `compile` for one expression, `compile_system` for a document's worth of named expressions as nodes with edges (spec: `docs/compile-system.md`), the validated constraint system, how to solve it into a `FeasibleRegion`, and the repair the region offers. Source text goes in everywhere and no syntax tree comes out; `lib.rs` re-exports exactly this surface and nothing from the directories below. | live |
| `src/cvg/` | the search engine — private. Strategies, the ladder, the opening and the design, the local solve (`local.rs`, COBYLA from `basin`), branch-and-prune (`prune.rs`), the GPU sieve. Reachable from `tests/` only through the `#[doc(hidden)]` re-exports in `lib.rs`. | live |
| `grammar/*.g4` | the ANTLR grammar. `build.rs` regenerates the lexer and parser from it into `OUT_DIR`. | live |
| `performance-records/` | throughput ledgers, written by the benchmarks; see its README | live |
| `docs/sojourn/` | notes and statement of intent from the original CVG project, whose code became `crate::cvg` | reference |
| `Justfile`, `.github/workflows/rust.yml` | CI is exactly `just ci` | live |

`src/frontend/generated.rs` is ANTLR output; never hand-edit it.

The JVM implementation this crate was ported from was deleted in 1c26ed9. The
Kotlin fixtures are the spec the Rust tests were ported from — `corpus.rs` ←
`BabelExpressionFixture.kt`, `cvg_pools.rs` ← `Z3SolvingPoolFixture.kt`, etc. —
and live at `git show 6813e0d:src/test/kotlin/com/empowerops/babel/`.

## Build and test

Everything runs from the repository root (the Justfile uses `pwsh`).

```
just build          cargo fmt, then cargo build --all-targets   (also regenerates the parser)
just test           cargo nextest run --no-fail-fast, then cargo test --doc
just lint           fmt --check and clippy -D warnings, check-only; --all-targets compiles every test
just bench          release-mode throughput, writes performance-records/*.csv
just brute          time-to-first-hit rungs + checks/s, release, machine otherwise idle
just quality        design quality on closed-form regions, ten seeds a case, release
just tag NAME       tag the commit as v<version>[-suffix], checked against Cargo.toml
```

- Use **nextest**, not `cargo test`: the AST is recursive and a stack overflow in one
  test must not take the binary with it. `.config/nextest.toml` sets a 60 s
  slow-timeout; the walker's burn-in on a 60-variable beam legitimately nears it in
  a debug build.
- There is no C or C++ toolchain in the build: the crate is pure Rust, and the one
  C++ dependency it had (Z3, built from source) was removed in favour of interval
  branch-and-prune. Do not reintroduce one without a new fact.
- `antlr-rust-codegen` pulls in RustPython; the lockfile currently wants a recent
  stable rustc. If `cargo build` complains about `requires rustc 1.9x`, update the
  toolchain rather than downgrading dependencies.

## How to work here

**TDD.** The port was driven test-first and the tests are the spec. A new behaviour
starts as a failing test in `tests/` (integration, public API) or a
`#[cfg(test)]` module beside the code (unit). Red tests are acceptable on a
feature branch; tests that fail to *compile* are not — that is an incomplete API.

**A `panic!` beats a spin.** A hang is the worst failure mode this crate has —
the 20-segment beam ran 3.7 CPU-hours without returning before anyone knew why —
so every foreign call, and every API of ours with even a remote chance of
combinatorial explosion or exponential backoff, carries a budget that ends it:
a resource limit in the callee's own units where one exists (an evaluation
count for COBYLA, a contraction count for branch-and-prune, a proposal count
for brute force), and otherwise a wall-clock ceiling set so conservatively that
reaching it means a bug, not a slow case — the five-sigma use. Reaching it must
fail loudly (an error, a `tracing::error!` and abandonment, a `panic!`) and never
wait. A budget is a count and decides the answer deterministically; a ceiling
is a watchdog and only decides that something is broken. Keep them distinct,
and never let a ceiling become the thing that decides a result. (The last
foreign call with a ceiling was Z3's leash; it went with Z3, and every loop
that remains is a count.) Nothing runs after a call returns: `solve` runs on
the calling thread, brute force fans out over the cores and joins them before
it returns, and there is no worker, no channel and no future — the async
`solve` and the batch stream that justified them went on 2026-09-16, when the
last consumer of a stream turned out to want one design.
The full inventory of the engine's loops and the hedges considered is in
`docs/todo-overtaken.md` under "Hanging is the worst failure mode" — overtaken
with the worker, kept for the reasoning.

**Call functions by their module.** Import modules and types; call functions
qualified — `eval::bind(..)`, `cvg::open(..)`, `ast::to_index(..)` — rather
than importing the bare name. A four-letter verb says nothing about which part
of the system is speaking, and the qualifier is what makes a call site readable
to someone who has not memorised the tree. Re-exports in `lib.rs` are the one
exception, since they *define* the surface.

**Assertions are exact by default.** Only cases that route through libm carry a
tolerance. Do not add blanket tolerances to make something pass.

**A `match` over an enum is exhaustive by default.** A `_ =>` arm is written
only with cause — "this code really does care about these variants alone, and
ignoring every other, including ones not yet written, is safe here" — and the
cause goes in a comment on the arm. Adding a variant *should* break every
match that has to think about it; that is the compiler doing the audit, not a
lag on development. The tree's shape lives in one such match,
`Expr::children`; a query over the tree is `Expr::iter_preorder` and a
`filter`/`any`/`count`, and a pass that builds a tree recurses — the iterator
is for readability where a query collapses to one line, not a rule that
recursion is wrong.

**The tape is the only evaluator, and the tests are its spec.** `eval/` lowers
the AST to a three-address tape and runs it tiled or per lane. It was held to
the tree-walker it replaced on a few thousand random and adversarial rows, then
the walker was deleted. The spec is `tests/corpus.rs`, `tests/runtime_errors.rs`
and `tests/special_values.rs`: plain tests with hand-written expectations. Add
cases there; never a recorded-output file. The CPU tape checks for non-finite
values on every instruction; only a future GPU sieve is allowed to be coarse,
and it must re-run an offending column through the tape for the span rather
than be "fixed" to match (see src/README.md).

**Neither backend's lowering is visible to the other.** The front end produces the
canonical form of what the author wrote and nothing more. If a pass makes the tree
easier to *analyse*, it belongs in `frontend::rewrite`; if it makes it faster to
*run*, it belongs in `eval`; if it makes it *narrowable*, in `cvg::interval`.
The `<= 0 is true` residual convention is `eval`'s, not the language's.

`a == b +/- t` is the worked example of that seam. `eval::irgen` desugars it into
`b - t`, `b + t`, a comparison against each, and a `Worst` fold — `Compare::Gte`
is `right - left` and `Compare::Lte` is `left - right`, so the result is
`max((b - t) - a, a - (b + t))` node for node, and `corpus.rs` pins that by
value. That deleted the fused instruction from the tape, the scalar executor, the
SIMD kernel, the tiled path and WGSL. **It stops there deliberately**: doing it in
`frontend::rewrite` would leave `cvg::classify` looking at two comparisons, and
an equality does not merely bound a variable, it *determines* one — which is a
dependency claim no pair of inequalities makes, and the thing driving is built
on. Recovering it would mean re-pairing two `Compare` nodes by structure, which
`fold_constants` or `invert_monotone` can disturb on one side and not the other.

**SIMD is explicit.** The tile executor's kernels live in `eval/simd.rs`, built
on `pulp` with the instruction set picked at run time. Every operator is either
a named vector kernel or a named `*_scalar` one; do not rely on auto-vectorisation
anywhere. Never use pulp's `mul_add` (fused on every backend) or its `max`/`min`
(x86 semantics, not NaN-propagating). The crate has no `unsafe`; keep it that way.

**The pool's ladder is contract, probe, local solve, bisect, brute force.**
Branch-and-prune (`Strategy::Prune`, `cvg/prune.rs`) contracts the declared
box under every constraint before a proposal is spent — HC4 propagation over
`interval::narrow` — and a coordinate that empties is the proof, with the
constraints that emptied it as the blame (`Infeasibility::Proved`). `cvg`'s
uniform sampler (`Strategy::BruteSquad`) then probes with one brute-force
batch — tens of microseconds — and delivers where that lands often enough.
Where it lands nothing, a local solve goes next (`Strategy::LocalSolve`,
`cvg/local.rs`): COBYLA from the box centre and a few seeded starts, stopped
at the first point the oracle judges feasible — 24 evaluations and 150 ms on
the 100-segment stepped beam. When the walker will carry the search, or
nothing has been found, the contracted box is split and pruned for a budget
of contractions (`with_prune_budget`, a count so the answer is
machine-independent): every box dying is a proof the contraction alone could
not see, and the leaves that survive are the pieces the walker must be
started in, since a chain cannot cross between pieces. Splitting is a
low-dimensional tool and the budget is sized to say so. Only what none of
that decides gets brute force: the same sampler on every core for a proposal
budget (`with_proposal_budget`, default a billion), and an empty search is
`Infeasibility::NotFound`, which claims nothing — a contradiction too thin or
too algebraic for an enclosure ends there, and `docs/todo.md` records the
classes. What brute force finds is a function of the generator's state and the
budget, never of the thread count — keep it that way (the batch is the unit of
randomness).
`Strategy` is a test-only configuration, not a user-facing one. Pool tests
run with `common::PROPOSAL_BUDGET`, a million under debug, because the
default takes minutes on an unoptimised tape. The opening's state is a value:
`cvg::progress::Progress`, threaded through `open` and folded with
`absorb`/`extend`, never a field; what it ends with is the region's `points`.
`ConstraintSystem` is immutable and compiled once; `Ladder` holds only the
strategies' streams and knobs and lives for one opening. Keep it that way —
the only `&mut` in the search is an RNG or a walker's chain. **The generator
is the caller's**: `solve` and `sample` take `&mut impl rand::Rng` and draw
their own `Xoshiro256PlusPlus` from it once at the door, thirty-two bytes
whatever the search then spends, so nothing past the boundary is generic and
the caller's later draws do not shift with a budget. Everything the region
does afterwards is a function of the region: it keeps one stream from the
ladder (drawn last, after the strategies' and the burn-in's) for its
reference point's extra starts and for every repair's sampling box, which
draws from a clone — so `repair` is the same landing every call, and no
constant seed hides anywhere in the crate. A design (`cvg::pool` then `cvg::farthest_first`, `FeasibleRegion::sample`) is a pure
function of the region, the caller's existing points, the count and the
generator's state: a fresh sampler and a clone of the region's walker per
call, a pool of candidates, farthest-first selection in box-normalised
Euclidean distance; the same generator carried on gives the next design. Not
a Latin hypercube, and no more stratified than random. A Latin hypercube's
one-axis guarantee holds at any count, so "fewer points than variables" is
not the reason; "far from what I have", which it cannot state, is. In twenty
dimensions farthest-first crowds the box's faces and loses to random on
discrepancy: `just quality` measures it, and its floors guard it. Another
criterion chooses through `sample_with` and a `dyn Chooser`, which names its
pool size and returns indices into a pool the region drew, de-duplicated and
with the caller's points taken out; the region still decides `Exhausted`
against `Degenerate`, before the chooser runs. Keep those decisions on this
side of the trait.

**A discrete variable takes its levels and nothing between them.** An
`InputVariable` declares a `Domain`: `Continuous`, `Stepped` (a lattice), or
`Levels` (a list). `ConstraintSystem::new` validates each one and derives its
*support* (`cvg::support::VariableSupport`, the probabilist's word for the
set of values a variable can take). The engine reads bounds and levels only
from `ConstraintSystem::supports` and reads `variables` only for names, so a
relaxed or pinned copy of a system differs from the original only in its
supports. A lattice is decimal, with each level bit-equal to the literal
(the module header says how), and one that can't be held that way is
refused with a pointer to `Levels`. Every rung respects the levels without
making them a special case:
- **Sampling.** `fill` draws a level index, one stream value per row, so a
  continuous system draws exactly what it always did.
- **Driving.** `classify::plans` never drives a discrete position.
- **Intervals.** `hc4` and `prune::contract` round each narrowing inward onto
  levels, which is still a superset of the admissible values, and an
  interval between two levels is empty, which is a proof.
- **Bisection.** `bisect` splits between adjacent levels, and judges a box
  narrowed to a single point, dying with blame when it fails, so a small
  lattice is proved by enumeration.
- **The walker.** It moves a discrete coordinate by a Gibbs draw over its
  levels, with the plan's driven coordinates unsettled. Random directions
  span the continuous coordinates only.
- **The relaxation.** A proof over levels is re-opened as
  `ConstraintSystem::relaxed`, and a continuous witness makes it
  `Infeasibility::BetweenLevels`.
- **Enumeration.** `design` enumerates an all-discrete lattice of at most
  `ENUMERATION_LIMIT` points, and a shortfall there is
  `SampleError::Exhausted`, a proof where `Degenerate` is a shrug.
- **Repair.** `repair` searches assignments of levels nearest-first, each one
  repaired on `ConstraintSystem::pinned` by the unchanged continuous stages,
  so a level changes exactly when that lands nearer.
- **The GPU.** The sieve draws levels from a table, and survivors are snapped
  to the nearest level before the CPU judges them.
- **The oracle.** `is_feasible` is exact on levels and stays that way. A
  caller whose round trip drifts a level by an ulp calls
  `ConstraintSystem::snap`, the public face of `VariableSupport::snap`.

Level arithmetic lives in `cvg::support` and nowhere else: `snap`,
`count_within`, `outward_from`, `within`, `split`. A `match` on a support
elsewhere decides what a rung does with a discrete coordinate, never how to
compute with its levels.

Categories with no order are the caller's to alias onto an integer lattice;
"nearest" and "spread" then read the index order. The measurements and
decisions are in `docs/todo.md` under "Discrete variables".

**An equality is read before it is searched.** `cvg::classify` reads
`a == b +/- t` and answers what can be concluded: `Pinned`, `Driven`, `Implicit`
or `Opaque`. A `Driven` variable is one the walker *computes* rather than
searches, which is what lets it move along a measure-zero surface instead of
jittering beside it — `classify::plans` turns a system into the schema positions
the walker moves and the ones it computes, in evaluation order, and `classify::retract`
applies one. *Plans*, plural: each equality lists every variable it can be solved
for (`classify::drivable`), a maximum bipartite matching decides who drives what,
and every matching of that size is a plan (up to eight, deduplicated by driven
set). A disjunction such as `x1 * x2 == 0` is a plan per arm, and
`classify::tightest` picks per point the plan whose driven coordinates are
pinned hardest — the arm the point is on; at a crossing the walker spreads its
chains across the tie. Three rules hold the whole thing up:

- **Driving is a Gibbs draw, not an evaluation.** `y == f(x) +/- t` admits the
  whole band, so `retract` draws uniformly from `f(free) ± t`. Assigning
  `y = f(free)` collapses the band to its centre line and throws away a
  dimension — on two hundred pinned variables it returned the same point two
  hundred times. Drawing from a conditional slice is the move that leaves the
  uniform distribution invariant; evaluating to the centre is not.
- **A drive is a proposal, never a rewrite.** Feasibility is re-checked against
  every constraint afterwards, and `retract` skips a coordinate whose definition
  will not evaluate. So a wrong or over-narrow isolation costs rejected moves and
  never a wrong point — which is why the isolation table can be aggressive.
  Refusing to drive is always safe; no constraint is ever dropped.
- **A variable named on both sides makes the equality *implicit* in it**, and
  `ConstraintSystem::new` refuses it, naming the rearrangement (`a == b + a/2` is
  `a/2 - b == 0`). Not "cyclic" — a cycle is a mutual dependency *between*
  equations, which `plans` meets and handles by driving neither. Two narrower
  rules were tried and discarded; both are written up in todo.md.

`classify::reaches` answers whether the operators between an equality's root and
a variable that occurs **exactly once** (*linear* in it, in the term-rewriting
sense) can all be undone, so `x1 + x2 == 3` drives `x1`. It answers *whether*,
not *what*: it used to build the rearrangement `3 - x2` for `retract` to
evaluate, and `hc4::slice` derives that band by narrowing the constraint
itself, so the expression lost its consumer and the walk down the path is all
that survives. The arithmetic those rules encoded lives in
`interval::invert_binary`, tested there against the same cases — the two arms
where operands do not commute (`a - u == c`, `a / u == c`) are still where a swap
is silently wrong.

**What can be reached is exactly what `interval` can invert**, and the two are
held together by `interval::invertible_unary` / `invertible_binary` with a test
that the predicates match the tables. Claiming a coordinate is driven and then
handing the walker its whole box for it is the one combination that *stalls*,
where an honest refusal only wastes a proposal.

That set is wider than the old `isolate` allowed, and the reason is worth
keeping: `abs`, `sqr` and `cosh` are not injective, so a **symbolic** inverse
would have to choose a branch and be silently wrong half the time — which is why
`isolate`, which built an expression, refused them. Narrowing does not choose; it
intersects both branches with what the argument can already be. `^`, `%`, `max`,
`min` and the periodic functions still decline.

Two holes were red on purpose and closed on 2026-09-16: driving assumes the
feasible set is a **graph** over the free coordinates, so `x1 * x2 == 0` was a
cross with one arm unreachable — now a plan per arm, chosen per point; and two
equations wanting the *same* variable drove neither — now the matching. What
stays open is seeding: a chain keeps the arm it started on, and the other arm's
seed is the bisection's to find, which it can in low dimension only.

`var[i]` is resolved at the first moment a schema exists — `eval::bind`, and
before it `ConstraintSystem::new`, which keeps the resolved tree for the
search — since `parse` has none and `Kind::Global` indexes the expression's
own symbols while `var[i]` indexes the schema. A literal subscript is then
the variable it names on every path: a load, a reference, an incidence row,
or a problem with a caret if the schema has no such position — the same
`CompilationFailure` an unbound name gets. After that
`Ast::contains_dynamic_lookup` means "a subscript nothing could resolve" rather
than "a subscript", and nothing downstream special-cases one. **A subscript
indexes the inputs**, never the whole row: a `compile_system` node's row carries
the externals and outputs it names after the inputs, and a row's shape is a
`RowLayout { inputs, intermediates }` — the two regions, named that way
everywhere — handed to the executors beside the batch; a gather is bounded by
its `inputs`, the width is its `total()`, and `var[floor(x1)]` past the
inputs faults rather than reading an output. `compile` takes the two
regions as `Variables` — a plain list is all inputs — and its row is every
input, whether named or not, so `var[5]` never moves, then only the
intermediates the source names, since one may not exist yet; `rows()` says
which. For `ConstraintSystem` a row is all inputs. A subscript the
row decides must be a whole number by construction — `floor`/`ceil` outermost
or inside exact integer arithmetic, an aggregate's parameter; the table is
`ast::is_integral`, the crate's one type judgement, and extending it is a
language decision taken there — or it does not compile. Every form in the
table is exact in `f64`, which is why the runtime has no rounding check on a
gather and never needs one.

**A constraint says what interval a coordinate may take.** `cvg::hc4` is
HC4-revise over the constraint's *tape*: the evaluator's IR in single assignment
(`VirtualTape::single_assignment`), lowered from the canonical tree with its
whole powers intact (the evaluator unrolls them on its own copy first,
`rewrite::unroll_powers` — `x·x·x` over intervals is wider than `x³` and inverts
worse), compiled once per constraint at `ConstraintSystem::new`. One forward
sweep computes every instruction's interval into a slot; one backward sweep,
from the requirement that the residual be `<= 0`, pushes each instruction's
requirement onto its operands through the inverses in `cvg::interval`, skipping
every instruction whose dependency bitset says it cannot reach the wanted
symbol. `hc4::slice` intersects that across every constraint naming a
coordinate, and both the walker's axis moves and `retract` draw from it. The
tape replaced a walk over the AST that re-evaluated every subtree it descended
into — 8.5× on a 200-term sum, a 200-variable design 14 s → 3.4 s — with the
same containment and soundness tests passing unchanged.

**Every interval is a superset of what it models, and that asymmetry is the
whole design.** A value drawn from a superset and then judged by `is_feasible`
is, conditioned on acceptance, distributed exactly as one drawn from the true
slice — so an interval that is too wide costs a rejected proposal and one that is
too narrow removes reachable points and biases the answer silently. Everything
follows: anything without an inverse answers `ENTIRE` and degrades to the
behaviour that already shipped, which is what let this land in stages. The
type is hand-rolled rather than `inari` because padding a few ulps outward buys
soundness without rounding-mode control, and tightness is a dial rather than a
requirement.

The boolean at a constraint's root is the **only** place the kind of comparison
is read — it supplies a target interval, and everything below is one uniform
backward pass. `a == b +/- t` gives `[-t, t]`; the two bounds it desugars to
would give `(-inf, t]` intersected with `[-t, inf)`, which is the same interval.

**A coordinate about to be recomputed is not one to condition on.** `retract`
marks a driven coordinate settled only once it has been drawn, and skips any
constraint naming an unsettled one. Conditioning `y` on a `z` that is itself
about to be recomputed from `y` pins the pair within a tolerance of each other,
and they shuffle by `t` a sweep instead of travelling — three occupied cells of
eighty, where twenty-four are wanted. This is why `classify::Plan`'s topological
order cannot be replaced by a per-coordinate Gibbs sweep, and the attempt is
written up in todo.md.

**Every candidate is judged by the whole system.** There used to be a
restricted check for an axis move — only the constraints naming the moved
coordinate, via `Incidence::affected` — with the precondition that the point it
was derived from had been feasible. Re-measured on 2026-09-12 it bought nothing
outside noise (270 s against 255 s on `top_corner_200d`), so it went, and with
it a precondition a caller could get wrong. `Incidence::affected` still serves
the repair clamp's per-coordinate filter, where "which walls can this landing
see" is the actual question.

**`ConstraintSystem` is data, and every strategy borrows one.**
`ConstraintSystem::new` (in `src/system.rs`) compiles every constraint to prove
it binds and keeps the tape beside the AST as one `Constraint`, along with the
drive plan and the incidence graph, both derived while proving the set fits
together. Its fields are crate-visible and it answers the simple questions
only: `is_feasible` (with a clearance) and `worst_residual`. The moves are the
engine's, as free functions over `&ConstraintSystem` in the module that owns
the idea — `classify::retract`, `classify::settle` and `classify::centre` for
the plan, `hc4::slice` for the narrowing question, `prune::contract` and
`prune::bisect` for the box. There is no wrapper type around the system.

**`FeasibleRegion` is the solved system.** `ConstraintSolver::solve` takes the
system by reference and clones it once for the region, so the region can
answer for the system after the search: `system()`, `witness()` and
`points()` for what the opening found, `sample` for a design — walked from the
region's own chains, burnt in at `solve` and cloned per call — and `repair` —
which lives here rather than on the system because a region that could not
be solved has nothing to repair toward. `repair` is a function of the region,
the point and the clearance and of nothing else — it consults no census,
because a landing that depends
on other points steers the optimizer being repaired toward them (the
*anchors* it used to take did exactly that, measured as a 28° bias on a
disc). "Near" is Euclidean over box-normalised coordinates, not taxicab: it
clamps each coordinate into its slice, then projects from there — Newton on
the KKT system over the active set (`cvg/newton.rs`), with gradients from the
tape's reverse sweep (`eval/differentiate.rs`); COBYLA (`local::nearest`) only
where a biting constraint has no derivative or Newton did not converge —
unless the clamp's landing is separable (bounds), where the axis projection
already is the Euclidean one. A constraint flat where the point stands is
walked in from the region's reference — a local solve from the box centre,
run once by `solve` — then projected by the same Newton from the chord's
landing, where the constraint is well-scaled (a Newton step that would cross
a box wall stops short of it and linearises again: `newton::WALL_FRACTION`),
a candidate the box's landing may still beat; a constraint with a jump in it (`floor`, `%`, `sgn`) is also
sampled around, a box doubling and shrinking on a clone of the region's stream — the
backstop for "if it can be sampled it is landed near", last because it cannot
localise a thin or high-dimensional region. **Gradients are reverse-mode over the virtual tape**, one
rule per instruction beside the instruction set, every partial from one
backward sweep at a fixed multiple of an evaluation; a tape holding `floor`,
`ceil`, `sgn`, `%` or a computed subscript has no gradient and everything
that wants one declines, never approximates.
A coordinate lands at the caller's clearance inside its bound, *on* the bound
at zero clearance; the design and the alternatives it displaced are in
`docs/todo.md` under *Repair for Artemis*.

`cvg::incidence` is the bipartite graph of constraints and coordinates, kept in
both directions because the walker traverses it both ways. Its indices are
newtypes — `Row` for a schema position, `ConstraintId` for a position in the
constraint list — because both directions are lists of `usize` that mean
different things, and a transpose built the wrong way round reads identically as
bare integers. Three such swaps were tried against the newtypes and all three
are now compile errors.

Two things belong in `affected` that a naive reading of the constraint's symbols
misses, and both are soundness rather than efficiency: every constraint naming a
**driven** coordinate, because retraction moves those whatever was swept; and
every constraint carrying an **unresolved `var[i]`**, because it reads a column
chosen by the point and no symbol list names it. Skipping either accepts a point
the full check would reject.

**A statistical oracle runs on ten seeds and requires all ten.** One seed
cannot tell a real change from a lucky draw: the uniformity benchmarks the
stream used to have (`tests/cvg_benchmarks.rs`, retired with the stream on
2026-09-16 — a space-filling design is deliberately not uniform) were failing
on about half of all seeds and passing on the committed one, so a green tick
was reporting the seed rather than the sampler. Two lessons from them still
apply to any statistic compared over walker output: the walker emits
round-robin across its chains, so autocorrelation sits at lag `CHAIN_COUNT`,
not lag one, and a KS test needs the effective sample size pooled across
coordinates — never `values.len()`. `docs/todo.md` has the numbers.

**Another language is never built with a string builder.** WGSL goes through
askama templates under `templates/`, compiled at build time against views in
`eval/wgsl.rs` and `cvg/sieve.rs` (SMT-LIB went the same way while it existed).
The semantics — which helper, which guard, what is refused — stay in Rust; the
syntax lives in files that read as the language they produce, with one macro
arm per operator, and the operator types the templates match over are the
subsets the target language can spell, so a missing arm is a compile error. A
`format!` that writes a brace, a parenthesis, an operator or a keyword of
another language is the smell to refuse. Template output is validated (naga),
checked for the substrings that matter and for balance, and never recorded to
a file.

**The GPU is a sieve and never a judge.** Behind the opt-in `gpu` feature
(`just brute`, `just bench` and `just test --features gpu` turn it on), brute force runs
on whatever wgpu adapter is present: the tape is rendered as
WGSL through the templates, candidates are drawn and judged on the device in `f32`
with a slack, and *every survivor is re-judged exactly on the CPU*. A false
negative costs hit rate; a false positive costs a CPU check; neither changes an
answer. Shader compilers assume no NaNs, so the emitter guards every operator's
domain with a comparison rather than relying on NaN propagation — keep it that
way. Shader text is validated with naga and compared against the CPU, never
recorded to a file. The GPU path is deterministic per device, not across
machines; `with_gpu(false)` is the reproducible path, and `SOJOURN_GPU` picks the
adapter (`off`, an index, or a name) and logs the list when set. Every wait on the device
has a timeout, and the device is held only while a brute-force search is
using it — a `Weak` in the module, an `Arc` in each live sieve — never for the
life of the process. The default build has no wgpu in it and must stay that
way; `test-compile` builds with every feature so the GPU code cannot rot.

**`sum` and `prod` bounds are constants.** Both are unrolled at compile time; a
bound that depends on a variable is a compile error, not a loop. That feature was
dropped deliberately (todo.md, "Dropped features") — do not reintroduce a
run-time aggregate without reading why.

**Nothing non-finite travels.** NaN/inf is a compile error where provable
(`ProblemKind::NonFiniteConstant`) and a runtime error otherwise
(`ProblemKind::NonFiniteValue`), reported against the innermost span.

**Measure before claiming a speedup.** Run-to-run noise on throughput is ~30%.
Compare medians of several runs, in one sitting, with an untouched case as a control,
against the parent commit. Benchmarks are release-only; a debug number is meaningless
and under upsert would overwrite a good row.

**There is no SMT solver, and the limits of what replaced it are known.**
Interval branch-and-prune proves what an enclosure can see: a plain
contradiction, a disc that cannot meet its ring. It cannot prove a *thin*
contradiction (`x + y <= 1` against `x + y >= 1 + 1e-9`) or an *algebraic* one
(`x*x - 2*x*y + y*y < 0`), and reports those `NotFound`; `tests/cvg_pools.rs`
pins both. Z3 proved some of those and spun on others, could not be interrupted
reliably, and cost a C++ build; cvc5 and dReal were evaluated against it and
rejected. The record is in todo.md under "The solver question, settled" and
"Z3's fate". Do not re-shop for a solver without a new fact — a user story that
needs one of the two classes above proved would be one.

## Style

- Doc comments explain *why* and record what was measured; the code says what.
  Match that register — the module headers are the model.
- Prefer a type that makes the mistake unrepresentable (`Progress`, the `Slot`
  binding table, `ConstraintId`/`Row`) over a check that reports it.
- Public API is batch-only: `CompiledExpression::eval(MatRef) -> Col<f64>`, one
  column per sample, one row per schema variable. `eval_row` is crate-private for
  the walker and is the same tape through the per-lane executor, not a second
  implementation.
- Non-obvious decisions go in `todo.md` part two, with the measurement that
  justified them.

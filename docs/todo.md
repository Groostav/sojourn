# Babel — todo

The Kotlin → Rust port is done: every construct in the grammar translates and
evaluates, and babel's own suite is green. sojourn-CVG has moved in as
`crate::cvg`: contracts, ported fixtures, an adaptive sampler, a hit-and-run
walker, and distribution oracles with critical values behind them.

**Part one is the roadmap** — one line per item, done and outstanding.
**[Part two](#the-reasoning) is the reasoning** behind it: the measurements, the
things that went wrong, and the decisions that are not obvious from the code.

The through-line: **the measure that matters is `Document::untranslated`** — the
constraints the emitter cannot write down, which the pool must then attack blind.
Every wave is ordered so each item shrinks the input to the next.

---

## Dropped features

Things babel used to accept and now refuses. Listed here, loudly, because a
dropped feature is the one kind of change the code cannot explain: a user who
wrote it gets a compile error and nothing in the tree says it was ever legal.

- **Dynamic bounds on `sum` and `prod`** — dropped 2026-09-03. `sum(x1, 5, i -> …)`
  and `sum(1, ceil(sqrt(target)), i -> …)` with `target` a variable used to run
  as a loop at evaluation time. They now fail to compile with
  `AggregateBoundNotConstant`. `sum` and `prod` are big-sigma and big-pi over a
  fixed index set; both bounds must be constant expressions, and the whole
  aggregate is unrolled at compile time (up to 1024 terms, `AggregateTooWide`
  beyond). Why: a run-time loop cannot run a tile of samples at a time, so the
  feature was a performance landmine hidden in an expression nobody writes.
  Details under "Standing" below.

## Done

### Wave 1 — the encodings that were always available

- [x] **`floor`, `ceil` and `%`.** `(to_real (to_int x))` is floor with the right
      sign; `ceil` is its reflection; `%` is `a - b*trunc(a/b)`, a *remainder*
      and not a modulo, because babel follows Java in taking the dividend's sign.
      Cost one widened logic line, `QF_NRA` → `QF_NIRA`, measured free.
      Closed `cvg_pools::modulo`.
- [x] **`BinaryOp::Mod` → `BinaryOp::Rem`.** The names were lying: `-7 % 3` is
      `-1`, a modulo would answer `2`.
- [x] **A `Script` of lines, not a string.** A missing `\n` between `(…)` forms
      is harmless; a missing one after a `;` comment eats the next command, and
      quietly. Made unrepresentable, as `Sexp` did for parentheses.
- [x] **`SmtLogic`**, via `ConstraintSolver::with_logic`, defaulting from
      `SOJOURN_SMT_LOGIC`, defaulting to `QF_NIRA`.
- [x] **Model values are rationals.** Z3 answers `2.5` with `(/ 5.0 2.0)`, so
      `as_rational` is the ordinary path. Its fallback was truncating: `approx`
      takes decimal *places*, so 1.01e-23 read back as `0.0`.
- [x] **`rewrite::fold_constants`.** Every constant subtree becomes a literal,
      using the evaluator's own `apply` so values cannot change.
- [x] **`ast::const_eval` deleted, `static_range` simplified.** The payoff of
      folding was not the folding — it was that "is this constant?" stopped
      being a question anywhere else.
- [x] **`rewrite::invert_monotone`.** Ten monotone functions. `2 < ln(x1)`
      reaches the solver as `x1 > e^2`, `20 > 2^x5` as `x5 < log2(20)`.

### Wave 2 — nothing non-finite travels, and powers are multiplication

- [x] **Non-finite is an error, at both phases.** `NonFiniteConstant` at compile
      time for what is provable, `NonFiniteValue` at runtime for the rest —
      checked on every node, so the error names the innermost subexpression.
- [x] **The domain guards became the textbook ones.** Refusing `-inf` is what
      let `ln`/`log10` move from `u >= 0` to `u > 0`. `sqrt` keeps its inclusive
      floor, and the asymmetry is now principled rather than incidental.
- [x] **`rewrite::expand_powers`.** `x ^ n` for constant whole `n` becomes
      repeated multiplication — for consistency first, speed second: the emitter
      already expanded them, so the solver reasoned about `(* x x x)` while the
      pool filtered with `powf`. *Retired since:* each backend lowers a whole
      exponent itself; see the `^` item under "Repair for Artemis".
- [x] **`emit::power` deleted**, and a latent bug with it: it rendered a
      negative exponent as `(/ 1.0 …)` with no divisor guard.

      Measured against the wave-1 baseline, points/ms bound. **Seven runs, not
      one** — the first reading said +32% on `small (jvm)` and that does not
      replicate. Median and worst case both matter here, because the spread is
      about 30%:

      | expression | baseline | median of 7 | worst run | verdict |
      |---|---|---|---|---|
      | `x1 + x2` | 18584 | 19583 | -8% | drift |
      | `x1 + x2 > 20 - x3^2` | 9164 | 11686 (+28%) | +1% | real, magnitude uncertain |
      | `sin(x1)*cos(x2)+sqrt(abs(x3))` | 9360 | 10156 (+9%) | -3% | drift — no `^` in it |
      | deep arithmetic | 3546 | 3523 | -14% | drift |
      | `sum(1, 200, i -> var[i]^2 - 3.0)` | 77.6 | **125.5 (+62%)** | **+32%** | **real** |

      `transcendental` contains no `^` and still moved +9%, so roughly that much
      of every figure is machine drift rather than code. Subtracting it, only
      the 200-variable case is unambiguous — which fits: it does two hundred
      squarings per evaluation where `small` does one, and `powf(x, 2.0)` was
      never expensive enough for a single call to show.

### Restructure — one AST, two backends

- [x] **`frontend` / `eval` / `cvg`.** `gen` was considered and is a reserved
      keyword in edition 2024. The front end is everything meaning-preserving;
      the two backends lower it their own ways and neither sees the other's
      lowering.
- [x] **Renames.** `Expression` → `Ast`, `Bound<'a>` → an owned
      `CompiledExpression`, `ConstraintPool` → `FeasibleSamples`, `Solution` →
      `Satisfiability`, `Generator` → `Search`, `is_boolean_expression` →
      `is_constraint`, `compile` → `parse`.
- [x] **`compile` is a free function**, `eval::compile(&Ast, &Schema)`. A tree
      that knows how to compile itself is not a data type.
- [x] **Batch evaluation over faer.** `CompiledExpression::eval(MatRef) -> Col`,
      one column per sample, one row per schema variable — which is the shape
      `cvg` produces, so a batch goes back in with no transpose. The scalar
      `evaluate` is deleted.
- [x] **`ConstraintSystem`**, validated at construction, replacing the two
      parallel slices. That is what leaves `solve`'s `Result` about the search.
- [x] **`Satisfiability` is two arms**, not three. Z3's `unknown` no longer
      surfaces: one that yielded a point is `Satisfied`, one that yielded nothing
      is `Unsatisfiable { NotFound }`. `Infeasibility` keeps `Proved` and
      `NotFound` apart because they are different sentences to a user.
- [x] **`FeasibleSamples`** with `take` / `try_take` / `available` /
      `is_exhausted` / `close`, returning `Mat<f64>`.

---

## How an item on this list earns its place

**Every open item names the test that goes green when it is built.** Written as a
`Driven by:` line. An item with nothing under it is a description of a feeling,
and the way to find out which one you have is to try to write the line.

This came out of rows C and E. Both were red, both had a paragraph of taxonomy
behind them, and both went green when the *pool* learned to ask the solver about
parts of the box it had not reached — with `classify::shape` still answering
`Opaque` for both and not a line of the rearrangement or the matching written.
The tests had been measuring a symptom that had another cause. They are now
strict enough to tell the difference, and that is the standard: a test that green
without the feature was never driving the feature.

Where no such test exists yet, say so — `Driven by: nothing yet` is a useful
thing to read, and is the first work item rather than an admission.

## TODO

### Wave 3 — the hard part, scoped by what is left

- [x] **Measured the residue.** `emit::tests::residue_what_the_emitter_still_cannot_express`
      — a ratchet, so it fails when the set moves. **26 of 32 corpus shapes
      translate; 6 do not, and every one of them is `sin`.** The measurement
      derives the *why* rather than declaring it: collect the operators in every
      constraint, subtract those appearing in something translatable, and what is
      left cannot be expressible.
      It also turned up a row missing from the inversion table — `log(a, u)` for
      a constant base, which is `a^u`'s inverse and should have gone in beside
      it. Added; `3 > log(2, x)` now reaches the solver as `x < 8`.
- [x] **`log(base, u)` in the inversion table.** The two halves of one
      relationship: each is the other's inverse, both monotone in `u`, both
      reversing below a base of one.
- [ ] **Causalization — the matching third only.** The measurement says the
      full Modelica pipeline is over-specified for what is left. Of the six:

      | | constraints | needs |
      |---|---|---|
      | driven | `y == sin(x)`, `y > sin(theta)`, `y < sin(x*pi)`, `y > 1.1*sin(x*pi-0.5)` | bipartite matching |
      | periodic set | `sin(x1) <= 0` | interval decomposition on a bounded domain |
      | implicit | `x1 > sin(ln(cos(2.1^x1)))` | nothing; correctly reported |

      **There are no strongly-connected components anywhere in the residue** —
      so no BLT, no tearing, no Newton. Those exist to break algebraic loops and
      there are none. Build the matching and stop.
      **Caveat on all of this:** the corpus is sojourn-CVG's *test* fixtures, not
      customer expressions. If real formulations contain implicit trigonometry —
      `sin(x) == x/2` shapes — the SCC case returns and so does the tearing work.
      Worth asking Garry before committing to skip it.
      The wave-2 write-up of the trick itself is in `todo-overtaken.md` under "Duplicates".
- [x] **Interval propagation, in `cvg::interval`.** HC4-revise: with every other
      coordinate held, what interval does this constraint admit for this one?
      `ConstraintSystem::slice` intersects that across every constraint naming the
      coordinate; axis moves in `walking::advance` draw from it, and `retract`
      draws from it instead of from the single equality that defined the
      coordinate.

      **Soundness is the whole design and tightness is a dial.** A proposal
      drawn from a superset and then judged by `is_feasible` is, conditioned on
      acceptance, distributed exactly as one drawn from the true slice — so too
      wide costs a rejection and too narrow biases silently. Anything without an
      inverse answers `ENTIRE` and degrades to what already shipped. That is
      what let it land in stages, and it is why the interval type is hand-rolled
      rather than `inari`: padding outward buys soundness without rounding-mode
      control, and tightness we do not need to *certify*.

      **It bought less than expected.** Measured across ten seeds it is
      indistinguishable from the box chord on `top_corner_200d` and slightly
      worse on `p118` (mean KS 0.141 against 0.124, t ≈ 1.25, not significant).
      Both benchmarks are dominated by hit-and-run mixing in high dimensions,
      and making *axis* moves exact cannot help when axis moves are the wrong
      moves — `p118` is a sheet whose long axes are eigenvectors, not
      coordinates. What it did buy: inequalities inform the walk at all, fewer
      shrink iterations, and `sqrt`/`ln`/`asin` narrow where `isolate` refused.

- [x] **Rotational preconditioning, which is what `p118` actually implicates.**
      *Landed 2026-09-11; see "Preconditioning the walker" under "What the
      oracles found" for what was built.* Draw directions in the chain's own
      covariance eigenbasis rather than in coordinates — done as the Cholesky
      factor rather than the eigenbasis, which is the same ellipsoid without an
      eigendecomposition. The diagonal version — scaling components by slice
      width — would not have helped: a diagonal map cannot fix off-axis
      correlation. The ten-seed benchmark family is what made it measurable.
      The four consequences below are still open; none of them landed with it.

      Four things fall out of the same mechanism:

      - **Driving stops being a feature and becomes a consequence.**
        `y == sin(x) +/- t` admits `[sin(x) - t, sin(x) + t]`, which is exactly
        what `Drive` carries by hand.
      - **Inequalities start informing the walk**, which they cannot do at all
        now: `x1 + x2 <= 3` admits `(-inf, 3 - x2]` for `x1`.
      - **The chord gets real endpoints.** `walking::box_chord` clips against
        the declared box and nothing else, then finds feasibility by shrinking
        on rejection. Propagation starts the interval where the constraints say
        it starts, a mixing improvement with no equality in sight.
      - **`Shape` collapses** into "can this coordinate's slice be computed",
        and `Implicit` becomes "the slice depends on the coordinate itself" —
        the honest definition, rather than a rule about a variable appearing on
        both sides of a `==`.

      Isolation is not wasted: peeling a compound side is still how the slice is
      computed. What changes is that the answer is an interval instead of a
      centre and a tolerance.

- [x] **`eval` desugars `a == b +/- t`; the AST keeps it.** Half of the entry
      below, and the half that turned out to be safe.

      `eval::irgen` emits `b - t`, `b + t`, a comparison against each and a
      `Worst` fold. `Compare::Gte` is `right - left` and `Compare::Lte` is
      `left - right`, so the result is `max((b - t) - a, a - (b + t))` node for
      node — **bit-identical, and `corpus.rs` passes unchanged**, which is the
      only claim worth making about a change like this. Five instructions where
      there was one, and `throughput_benchmarks` does not move.

      Deleted: `Instruction::NearEq` and its four `tape.rs` arms,
      `lane::near_eq`, the `simd::near_eq` kernel with its dispatch harness and
      bit-exactness test, the tile arm, and the WGSL `Stmt` variant with its
      template line. 143 lines out.

      One behaviour change, small and worth recording: the WGSL arm carried its
      own `babel_slack`, so the GPU sieve's cushion goes from
      `S * (|a| + |b| + t)` to `S * (|a| + |b -/+ t|)` — a shift of order
      `S * t` against a cushion of `S * (|a| + |b|)`.

      **It costs about 1.8x per equality evaluation**, and the suite could not
      see that: `throughput_benchmarks` had no equality case at all, so a shape
      the evaluator kept a dedicated path for was invisible to the instrument
      built to watch the evaluator. A `near equality` row now exists — 195.6
      rows/us at width one, against 351.7 for `x1 + x2`, which is the same three
      instructions the fused form used to be.

      Downstream that is ~1.5x on a problem that is nothing but equalities:
      `top_corner_200d_as_equalities` went from about 230s to 338s across ten
      seeds. Judged worth it — five executors lost an arm, the GPU among them —
      but if it ever needs winning back, the place is a **peephole on the tape**
      that fuses the five instructions, not an AST node. An optimisation belongs
      where optimisations belong, and the front end stays simple either way.

- [ ] **A design of experiments over driven arguments.** Latin hypercube or
      Sobol over the argument expression's variables. A correctness issue, not a
      tuning one: pick one `x` and every point in the pool shares a `y`, which
      is a constant rather than a sample.
      The reasoning — why one `x` makes `y` a constant, not a sample — is in `todo-overtaken.md` under "Duplicates".
- [x] **Repair a near-miss point instead of discarding it.** Done in its minimal
      form: `ConstraintSystem::adjusted`, a bounded coordinate sweep run on a solver's seed.
      It landed because it had to. Emitting `a == b +/- t` as two bounds rather
      than `max(...) <= 0` changed which *edge* Z3 picks, and `cvg_pools::constants`
      went red — Z3 answers with the boundary exactly, because a boundary is the
      simplest solution there is, and `f64` then rounds it a hair outside. The
      previous green was luck: the old encoding happened to make Z3 pick the edge
      where the rounding went the other way.
      What is still owed is the gradient version. This steps coordinate-wise by
      ulps, which is right for a boundary witness and nothing else; it cannot
      rescue a point that is genuinely infeasible, and a test pins that it does
      not try.
- [ ] **The gradient version of repair.** When the solver
      nominates a point that babel's own `evaluate` then rejects, the pool bins
      it — throwing away the expensive half of the work over a difference the
      solver could not have seen, since it reasons in exact reals and we filter
      in `f64`. Perturb toward feasibility instead. The residual is signed and
      graded, so it says how badly and a finite-difference gradient says which
      way; isotropic jitter is the wrong tool in high dimensions, for the same
      concentration-of-measure reason the walker already has to work around.
      Watch the distribution: repairing a *seed* is free, repairing an *emitted*
      point skews the marginals, and `cvg_benchmarks` is what would catch it.

- [x] ~~**`take` should be `async`.**~~ Moot: `take`, the stream and the
      worker are gone (2026-09-16, below). `solve` is synchronous and
      `sample` is a design, not a stream.
- [x] ~~**`impl Stream for FeasibleSamples`**~~, likewise.
- [ ] **A pipeline to hang these on.** Causalization splits one problem in two
      and the DOE turns one into `n`; both are `SolverProposal -> Vec<_>`, so a
      `flat_map` chain is the shape. Deliberately not designed yet — the trigger
      is the *second* fan-out pass.

### Parallel — the evaluator, not the solver

- [x] **Lower to a tape.** Done 2026-09-02 as step 1 of `brute-squad.md`:
      a three-address tape with register allocation, a tiled executor and a
      per-lane one for the walker. Run-time-bounded aggregates were dropped on
      2026-09-03 (see below), so every tape is straight-line.
      The tree-walker did *not* stay as the oracle, despite the plan below: the
      tape was held to it on a few thousand random rows, then it was deleted.
      The spec is the corpus, `runtime_errors.rs` and `special_values.rs`.
      Measurements in the brute-squad plan.
- [ ] **A fast sine for the evaluator.** Remez/minimax coefficients,
      Cody–Waite reduction. Objective functions keep the exact path, and nothing
      here is ever emitted to a solver.
      The accuracy notes (SLEEF, Cody–Waite, the `FSIN` story) are in `todo-overtaken.md` under "Duplicates".

### Standing

- [ ] **Sampling quality: a baseline, then a pool to choose from.** *2026-10-21.* The
      plan is `docs/sampling-quality.md`; step 1 (`sojourn::quality`, the closed-form
      fixtures, `just quality` and its ledgers) and step 2 (floors on the worst seed,
      the stratification claim corrected) and step 3 (`sample_with` and `Chooser`, the
      pool measured in the ledgers) have landed. Part two, "What farthest-first does to a
      design", has the finding: beats random in two dimensions; in twenty it crowds the
      faces and loses to random on CL2; no more Latin than random anywhere it has room
      to be. Open: whether `FarthestFirst`'s `2n + 128` should grow where the pool is
      walked, since the 20-D simplex's pool is itself less even than random.
- [ ] **Discrete variables: three things left open.** *2026-10-20.* The design and the
      reasons are in part two under "Discrete variables".
      - **Measure the hot path.** `just bench` against `e8ed673`, on the continuous-only
        cases: the box check now reads a `VariableSupport` per coordinate, and `fill`
        matches a draw table per value. `fill` had a separate all-continuous loop, added
        for speed and never measured, so it went; if the bench shows the `match`, that
        loop comes back with the number beside it. Neither should show, and that has not
        been measured.
      - **A walker chain cannot follow an equality between discrete variables.**
        `n == 3*m +/- 0.1` with both stepped: a move changes one level at a time, and
        every single-level move leaves the line, so a chain stays where it started. A
        design still covers it, but only because the pool's uniform draws do, which is
        fine for a small lattice and not for a large one. A joint move along the
        equality, a level of `m` with `n` recomputed and snapped, would be a drive onto
        levels, which nothing does yet.
      - **Repair clones the system per assignment it tries.** That is cheap while the
        search stops at the second assignment, as it does on every fixture. If a caller
        with many discrete variables measures it badly, `Arc<[Constraint]>` makes the
        clone a pointer copy.

- [ ] **A computed subscript in a system is bounded, not understood.** *2026-09-21.*
      `compile_system` (`docs/compile-system.md`) lays a node's row out as the inputs it reads
      then the externals and outputs it names, so `var[floor(x1)]` could have reached an
      output; a row's shape is now `RowLayout { inputs, intermediates }`, handed to the executors
      beside the batch, a gather is bounded by its `inputs`, and past them is the same runtime
      fault as before. That is all that was done, on
      purpose. Two things were not: proving statically that a computed subscript can land in
      range at all (the input count and the box are both known, so `var[floor(x1)]` with `x1`
      on `[0, 100]` over three inputs is *mostly* a fault, and could be said so at compile
      time), and teaching `cvg::hc4` to follow the indirection instead of answering `ENTIRE` —
      which would mean a per-branch narrowing over every input the subscript could name, and
      is where the cost is. The feature is half a solution looking for its problem; the
      likely problem is *conditional variables* — "this constraint holding is what makes
      these coordinates matter" — and the shape of that is not yet known (a table? a
      constraint that gates a set of coordinates?). Decide the problem first; a subscript that
      turns out to be its encoding will then know what analysis it deserves.
- [ ] **`ast::is_integral` is the crate's one type judgement.** A table, not an analysis:
      the forms that are whole numbers by construction, each exact in `f64`. Anything that wants
      to read a value as an integer — a subscript today; an integer-typed exponent or a
      non-constant aggregate bound if either ever lands — asks it rather than growing a second
      copy, and a new row in the table is a language decision taken there, once.
- [ ] **Compatibility nobody wants.** `irgen` kept an out-of-range literal subscript faulting
      at run time "as the walker did" — preserved because it was there, not because anyone
      asked — while `ConstraintSystem::new` refused the same subscript at construction, so
      `var[2]` meant two things depending on the door (fixed 2026-09-17: `bind` resolves, one
      meaning). Audit for others: grep `as the .* did`, `used to`, `the JVM`, `Kotlin` in `src/`
      (33 hits on 2026-09-17, most of them honest history) and decide each — keep with a reason
      that names who wants it, or delete.
- [x] **Equality constraints — the taxonomy is closed.** One syntax,
      `a == b +/- t`, covering six structurally different things. A, B and D are
      driven or reached; C and F turned out to be one row and are **refused**, a
      variable on both sides making the equality implicit in it; E is green
      through isolation, and since 2026-09-16 through matching where isolation
      alone would have two equations fight over one variable.

      What made it work was not the labels. It was **driving**: the walker moves
      the free coordinates and computes the rest, so a point on a measure-zero
      surface stays on it. Everything else — the branch coverage, the compound
      sides, the subscripts — was about widening what could be driven.

      The tolerance still belongs to the *strategy* rather than the constraint,
      and that part is **not** done: the solver wants the surface, the walker
      wants the band. Both roads lead to an SMT beachhead with the walker
      building out from it, and `(2t)^200` is out of reach even at `t = 0.45`.
      Taxonomy, measurements and what each attempt cost are below, under
      [equality constraints](#equality-constraints).
- [x] **Run-time-bounded aggregates are gone.** `sum(x1, 5, …)` was a loop the
      batched evaluator could only run one sample at a time — a performance
      landmine hidden behind a feature nobody uses, since `sum` and `prod` exist
      to write big-sigma and big-pi over a fixed index set. Now every aggregate
      is unrolled or refused at compile time: `AggregateBoundNotConstant`,
      `IllegalAggregateBound`, `AggregateTooWide` (the 1024-term cap). The
      tape's loop instructions, the per-lane loop stack and the straight-line
      versus loop distinction went with it. The docs' one example with a
      variable in a bound, `sum(1, ceil(sqrt(target)), …)`, now needs `target`
      to be a literal.
- [x] **`Strategy::AdaptiveSampling` is gone** (2026-09-03). It narrowed the
      proposal box toward found points to seed the walker in the 1e-2..1e-6
      regime, biased by construction. Measured before removing: with it out of
      the default list, no `cvg_pools` or `cvg_benchmarks` verdict moved, because
      the solver already catches the narrow cases there. The brute squad is the
      planned replacement for `UniformSampling` as the thing that reaches a seed
      at all; `Strategy` itself stays as a test-only configuration, never a
      user-facing one.
- [x] **`Strategy::UniformSampling` became `Strategy::BruteSquad`** (2026-09-03, step 4 of
      `brute-squad.md`). Same rung, one more job: when the 10,000-candidate probe lands
      nothing and the solver could not settle it, the sampler keeps proposing on every core until
      a batch lands or a proposal budget is spent — `ConstraintSolver::with_proposal_budget`,
      default a billion. The batch is the unit of randomness (batch `k` is seeded from `(base, k)`,
      the lowest-numbered batch with a hit wins), so the point found is a function of the seed
      and the budget and never of the thread count; a test pins one thread against eight.
      Dropping the `solve` future cancels the search between batches. Nothing about the probe or
      the delivery path changed, and no seeded verdict moved.

      **Order, decided the same day:** probe, *solver*, brute force. The first cut ran brute force
      before Z3 and every problem the probe cannot land — an equality ribbon, a contradiction, a
      200-dimensional corner — paid the whole budget (three to seven seconds in release) before
      Z3 settled it in milliseconds. Now Z3 goes first when it is in the list, and brute force
      spends the budget only on what Z3 answered `unknown` to or handed back a witness that did
      not survive filtering — which is the transcendental regime the brute squad exists for.
      Without a solver in the list the probe hands straight to brute force, so the rung fixtures
      still measure sampling alone. The same change gave Z3 an `rlimit` (a work count, not a
      clock, so a problem answers the same everywhere) and shrank the probe from ten thousand
      candidates to one brute-force batch — under three thousand for three variables, tens of
      microseconds — with the easy-path threshold restated as the hit rate it always was, one
      in a thousand. Both re-roll the seeded pool verdicts; none moved. The budget is still minutes on an unoptimised tape, which is
      why every pool test runs with `common::PROPOSAL_BUDGET` (a million under debug, the default
      in release). Running the two concurrently remains a possible refinement; the sequential
      order removes most of the reason for it.
- [x] **The GPU sieve** (2026-09-04, step 3 of `brute-squad.md`, done after step 4).
      Brute force runs on a wgpu adapter when there is one: `eval/wgsl.rs` renders each
      constraint's tape as a WGSL function, `cvg/sieve.rs` wraps them in a harness that draws
      candidates on the device from a 32-bit hash of `(base, batch, lane, dimension)`, judges them
      in `f32` with a relative slack of `1e-4`, and appends survivors; the CPU re-judges every
      survivor in `f64`. Behind the **opt-in** `gpu` feature — a library opening a GPU and holding
      it flat out for seconds is something a consumer should ask for, where nobody blinks at SIMD —
      and `ConstraintSolver::with_gpu(false)` turns it off at run time; no adapter, or a shader that
      does not build, falls back to the CPU loop, and so does a device that stops answering (every
      wait has a 30 s timeout). The device is held only while a brute-force search is using it: a
      `Weak` in the module, an `Arc` in each live sieve, dropped with the last one, reconnected (a
      hundred-odd milliseconds) by the next solve that gets that far. `SOJOURN_GPU` picks the
      adapter — `off`, an index, or a substring of its name — and logs every adapter wgpu can see
      when it is set; unset, wgpu's high-performance preference chooses, which on a two-GPU
      machine is the discrete card. A value that matches nothing is a `warn` and the CPU path.

      Learned on the way: **shader compilers assume no NaNs.** This laptop's driver evaluates
      `NaN <= 0` as true, so a `sqrt` of a negative *passed* the sieve. The emitter now guards
      every operator's domain — `sqrt`, `ln`, `log`, `acos`, `asin`, division, `%`, `pow` with a
      negative base — with a comparison on the finite input, which is the CPU's fault semantics
      spelled out; overflow to infinity is left alone because infinity compares sanely. And the
      auto-derived bind group layout: each entry point uses a different subset of the bindings,
      so a layout derived from one rejects the other's bind group, silently, through the
      uncaptured-error handler; the layout is explicit now and every dispatch runs inside an
      error scope so a rejection reads as a failure rather than as "no survivors".

      **Determinism is per device class.** Same GPU and driver, same seed; a different vendor's
      special-function units round `sin` differently and may land a different seed. The CPU path
      is the machine-independent reference. The GPU has its own budget, thirty billion
      (`with_gpu_proposal_budget`), sized so a ten-billionth region is found three times over in
      expectation. Measured on the laptop's iGPU: two billion candidates drawn and sieved a
      second, the sine corner at the same rate as the plain corner; twenty-five times one CPU
      thread, four times all sixteen. The 1e-10 rung needs the budget in ten seconds and the iGPU
      takes fifteen, so it stays red here and waits for BATOU's card.
- [x] **SMT-LIB is emitted through askama templates too; `Sexp` and its macros are gone**
      (2026-09-05). `emit.rs` keeps the walk — which helper, which divisor guard, which auxiliary,
      what is refused and reported — and hands each node's already-rendered children to a `Term`
      whose template spells it; `Condition`, `Prelude` and the whole-document `Script` are the
      other views, under `templates/smt2/`. The operator types the templates match over are
      `SmtUnary`/`SmtBinary`, the subsets SMT-LIB can spell, so the transcendentals and `Pow`/`LogB`
      cannot reach a template by accident and a variant without an arm does not compile. What
      `Sexp` guaranteed structurally — an unbalanced document is unrepresentable — is now a
      property of five small template arms each closing what they open, pinned by
      `every_document_is_balanced`, by `smt::tests` putting every document through Z3's parser,
      and by `Z3Backend::solve` refusing a document that produced no assertions. The document
      template owns every newline, so the `;`-comment-eats-the-next-command hazard is pinned by
      `a_multi_line_source_is_one_comment_line`. Output is byte-identical: a dump of thirty-odd
      documents before and after diffs empty, including the one where askama's trailing-newline
      trim had to be compensated in the template. The `define_fun!` macro's own doc predicted
      its retirement and listed the reasons; they held.
- [x] **WGSL is emitted through askama templates** (2026-09-05). The first emitter wrote the
      shader into a `String` — a `writeln!` per instruction, a ninety-line `write!` for the
      harness, operator spellings as `format!` arms — and produced the flattened literals and
      broken indentation that prompted the rule: *another language is never built with a string
      builder.* Now `eval/wgsl.rs` turns a tape into a `Function` view (one `Stmt` per
      instruction, operands already named) and `templates/wgsl/function.wgsl.jinja` renders it;
      `operators.wgsl.jinja` is the operator table, one macro arm per babel operator with its domain
      guard, which is StringTemplate's tree-to-text model in askama; `harness.wgsl.jinja` is the sieve.
      Askama over minijinja or tera because the templates are checked at `cargo build` against
      the views, and the `match` arms are exhaustive in the Rust sense — a new operator without a
      spelling does not compile. The five buffer bindings became one table (`BINDINGS`, a `Slot`
      enum) that renders the shader's declarations, builds the bind-group layout, and names each
      slot's buffer through an exhaustive match, replacing three places that agreed by number.
      The rendered shader is identical in meaning: same seed, same 257 survivors, same checksum,
      before and after. `Sexp` was expected to stay for SMT-LIB; it went the same day, see the
      entry above.
- [x] **`a == b +/- 0` is a compile error** (2026-09-05): `ProblemKind::DegenerateTolerance`, for
      zero and negative tolerances, judged on the value in the parser so every spelling of zero
      is one rule; a non-finite tolerance is `NonFiniteConstant` like any other constant. Exact
      `f64` equality is a scatter of points with no volume, which sampling cannot reach and a
      solver's real witness misses by an ulp. A tiny positive tolerance below the ulp of the
      values compared is the same pathology and cannot be judged without the values.
- [x] **`Progress::points` is a window** (2026-09-05): the most recent `RECENT_POINTS` (1,024),
      oldest out first. Its two readers — the first batch delivered and the walker's chain
      starts — want a fair sample of where the region has been seen, not a history; unbounded,
      a caller drawing a million samples at two hundred variables held 1.6 GB nobody read.
- [x] **`cvg::Search` is gone** (2026-09-03). It held three things with different lifetimes —
      the problem, the strategies with their RNGs, and mutable search state (`found`, `route`) —
      and `produce` did different things per call because of the last. Now: `Problem` is
      immutable and compiles every constraint once (it used to recompile them all on every
      batch); `Ladder` holds the strategies and nothing but their streams and knobs; `Progress`
      is a *value* threaded through the worker — `progress = progress.absorb(trial)` — with
      the route pinned by the first trial and recorded in it. The worker is three straight-line
      functions, `serve` → `open` (probe, solver, brute force, no flags) → `keep_filling`, and
      `next_batch` is a pure function of the route. `PointSource` went with it: the sampler
      and the walker have their own methods and different shapes. `Trial` (one round of
      uniform proposals) and `Progress` (the fold) stayed two types because the walker's
      output must count as points and not as trials, which is a distinction the type carries.
      Two behaviour changes, both deliberate: the first batch is the points in hand rather than
      a walker batch from them, and a solver witness that survives filtering is `Satisfied` by
      itself (so "one point, no second" is `Satisfied` then exhausted, not `NotFound`). No
      `cvg_pools`, `cvg_benchmarks` or rung verdict moved. Left for later: `Progress::points`
      grows without bound in a long-lived pool, as `found` did; and the reminder to look for a
      test that shows the route should ever flip after the first batch.
- [ ] **Restricting `a ^ b` to an integer `b`.** Needs Garry. Less urgent than
      it was — every backend lowers a constant whole exponent itself and
      `invert_monotone` rescues `2^x5` before any restriction would see it.
      The three reasons — evaluator speed, narrowing, the solver — are in `todo-overtaken.md` under "Duplicates".
- [ ] **Relevance-filtered parameters in a runtime error.** Planned as wave 2's
      optional tail and **not done**. `RuntimeProblem::parameters` carries the
      whole row; narrowing it to the variables the failing subexpression
      actually reads means walking the program for the node matching
      `fault.span` and collecting its `Kind::Global` ids — error path only, free
      on the happy path.
- [ ] **Locals in a runtime error.** `RuntimeProblem::locals` is always empty;
      populating it needs a slot-to-name table the AST discards. Matters less
      now that the non-finite check is eager and the span is exact.
- [ ] **Capability metadata per backend.** The cheap half — `emit` taking a
      capability set rather than an implicit one — when a second backend exists.
- [ ] **`cvg_benchmarks::p118` is red on every seed.** Polytope mixing. Swept
      across ten seeds it failed all ten, at a mean KS of 0.124 against
      thresholds around 0.10 — so unlike `top_corner_200d` this is not a
      threshold it grazes, it is consistently short.

      Probed: fifteen dimensions, condition number 79.5, spectrum
      `230.45 155.12 49.06 21.84 …` — **two long axes and thirteen short ones, a
      sheet rather than a tube**, and two independent chains agree on that
      covariance to 16%, so the shape is the body's and not chain history.
      Rejection cannot reach it even at 400M proposals, so no unbiased reference
      exists and `RunsAgree` is the only oracle available.

      Interval propagation does **not** move it (mean KS 0.141 with, 0.124
      without, t ≈ 1.25). The long axes are eigenvectors rather than
      coordinates, so exact axis-aligned moves are still the wrong moves. What
      this implicates is rotational preconditioning, and measuring that needs
      the benchmark family below rather than this one sample.

      The sharpest single number, from the ten-seed run: one seed reports
      **a thousand points worth twenty-four independent ones — 2.4% efficiency**,
      through `assert_efficient`, which had been computing it all along with
      nowhere to report it. That is the autocorrelation the KS failures are
      downstream of, and it is a better target to optimise against than KS is:
      it needs one run rather than two, and it moves continuously.
- [ ] **The benchmark cannot resolve small changes.** Run-to-run spread is about
      15% on this machine; anything under that is not a reading. Also, no
      benchmark expression contains a constant subexpression, so folding is
      invisible to it.

      The same blind spot cost a real measurement once: there was no equality
      case either, so desugaring `a == b +/- t` in `eval::irgen` tripled the
      instructions for that shape with the whole table still inside budget. A
      row was added. **The rule the miss suggests: every shape the evaluator
      gives a dedicated instruction deserves a row**, or removing that
      instruction is free to the instrument and not to the user.

- [ ] **Find what the walker actually spends its time on.** Two changes that
      should each have been large were not: `ConstraintSystem::is_feasible_after` cut an
      axis move on `top_corner_200d` from two hundred constraint evaluations to
      one and bought 3% (134.7s to 130.6s), and 6% on `p118` (35.1s to 32.8s).
      Consistently positive, an order of magnitude short of predicted.
      *2026-09-12: re-measured with the full check on every axis move and the
      difference was gone — 270 s against 255 s on `top_corner_200d`, inside
      run-to-run noise — so the restricted check was removed along with the
      soundness precondition its callers had to hold. The walker judges every
      candidate with `is_feasible`.*

      So constraint evaluation is **not** the bottleneck it was assumed to be.
      The unexamined candidates are `ConstraintSystem::slice` walking an `Ast`
      per coordinate where `is_feasible` runs a compiled tape, and the
      per-proposal allocations in `advance` (the two-hundred-element `movable`
      `Vec` went with preconditioning, since the walker now holds it; the
      direction and candidate `Vec`s remain). Measure before optimising anything
      else here — this entry exists because that was skipped once already.

- [x] **`cvg_benchmarks` runs every problem on ten seeds and requires all ten.**
      One seed could not tell a real change from a lucky draw, and it was
      hiding a real defect rather than merely being imprecise.

      Measured on `top_corner_200d`, sweeping seeds with interval propagation
      and with the box chord it replaced:

      | | fails | mean KS |
      |---|---|---|
      | propagated slice | 5 of 9 | 0.1807 |
      | box chord (before) | 4 of 7 | 0.1805 |

      Indistinguishable — and **both fail about half the time**, where two
      hundred marginals at this alpha should fail together about one run in a
      hundred. The committed seed passed. So the green tick was reporting the
      seed rather than the sampler, and the same sweep on `p118` failed on
      every seed of twenty.

      `run` now drives [`REPLICATES`] independent trials through `catch_unwind`,
      reports every seed that failed, and each replicate reseeds the rival run
      and the projection directions too — a fixed rival would have held half of
      every `RunsAgree` comparison constant.

- [x] **`top_corner_200d` was the oracle, not the walker.** It failed six of
      ten seeds, and the conclusion recorded here was that the walker genuinely
      falls short of uniform. **That was wrong**, and the correction is worth
      more than the entry was.

      Emission is round-robin across `CHAIN_COUNT = 8` chains, so successive
      points come from *different* chains and the autocorrelation sits at lag
      eight. Measured:

      ```text
      lag 1..7:  0.06     (noise floor at 200 points: 1/sqrt(200) = 0.071)
      lag 8:     0.2303
      lag 9..16: 0.06
      ```

      Two failures compounded. Sokal's window closes at `WINDOW_FACTOR * tau`,
      and with `tau` near one that is **lag five — before lag eight**, so the
      rule written to avoid needing to know the chain count was defeated by
      exactly the structure it was written for. And per coordinate the signal
      sits *at* its own noise floor, so detection was a coin flip: one run
      reported an effective size of 200 for one coordinate and 32 for another.

      So `autocorrelation_time` now pools `rho_k` across coordinates — the
      interleave is a property of how the points were emitted, shared by every
      column, and pooling estimates it with two hundred times the data — and
      the window must reach lag sixteen before Sokal's rule may close it. The
      estimate is flat across windows of twelve, sixteen and twenty-four, which
      is the evidence it is measuring rather than choosing.

      `tau` goes from 1.33 to 1.85, effective n from 200 to ~108, and the KS
      critical value from 0.157 to 0.214 — above every failure observed
      (0.1684 to 0.1961). **All ten seeds pass.**

      The check that this is a fix and not a weakening: `p118` still fails, and
      fails *better*. Its effective n dropped from ~900 of 1000 to ~250-450,
      and one seed now trips the ten-per-cent efficiency floor outright rather
      than presenting as a distribution mismatch.

- [ ] **The walker still under-thins at high dimension**, which is separate from
      the oracle bug above and survives it. At 200 dimensions thinning is
      `2 * 200 = 400` advances per emitted point, and the sweep index is
      `steps % 200`, so each coordinate is swept exactly twice between points
      and takes an axis move with probability one half — about one fresh
      conditional draw per point, and **none at all a quarter of the time**.
      That is the `rho = 0.23` at lag eight.

      Measured against `top_corner_200d` before the oracle was fixed, so the
      counts are failures-out-of-ten under the old threshold rather than a
      verdict:

      | thinning | steps/point | failed |
      |---|---|---|
      | 2 (shipped) | 400 | 6 |
      | 4 | 800 | 2 |
      | 8 | 1600 | 1 |
      | 16 | 3200 | 3 |

      Raising it costs wall clock linearly and the restricted feasibility check
      (since removed; it measured out) did **not** make that affordable — see
      the entry on where the walker's time actually goes. Left alone deliberately: nothing red depends on it now, and
      changing it without knowing the cost driver would be guessing.


---

# The reasoning

Why the items above are what they are. Everything here is a record of
something measured or something that went wrong, kept because neither is
recoverable from the code.

## The waves

### The restructure: one AST, two backends

Not a wave — the thing the waves kept running into. `cvg` needs to *read*
structure and `eval` needs to *destroy* it, and both were being served by one
pipeline that made the second's choices for the first. So the crate now says out
loud what it always was: a front end producing an `Ast`, and two backends
consuming it.

**`gen` is a reserved keyword in edition 2024**, verified rather than assumed —
`mod gen` does not compile. Hence `frontend` / `eval` / `cvg`.

**Evaluation is batch-only.** `CompiledExpression::eval(MatRef) -> Col<f64>`,
one column per sample, one row per schema variable. That is the shape `cvg`
already produces, so a generated batch is directly an input matrix with no
transpose, and the scalar `evaluate` is gone — it had no consumer that a batch
of one does not serve.

Except one, and finding it was the surprise. **The walker is a genuine
one-at-a-time consumer**: its shrinkage loop cannot propose a candidate until it
has judged the last, so it is sequential by nature. Wrapping each point in a
one-column matrix cost *five times the evaluation itself* — `p118` ran 32s
against 6s, and `top_corner_200d` blew through the 120s nextest timeout. The fix
is a crate-private `eval_row`, which is the body of `eval`'s own loop rather
than a second implementation. The public API stays batch-only; the premise that
nothing needs a row was simply wrong, and it was wrong inside this crate.

**`Satisfiability` is two arms.** Z3's `sat`/`unsat`/`unknown` no longer
surfaces: an `unknown` that still yielded a point is `Satisfied` like any other,
and one that yielded nothing is `Unsatisfiable { NotFound }`. The invariant that
makes two arms sufficient is that **`Satisfied` means at least one sample is
already in hand** — which required a real fix in the worker, because a solver
seed can fail the pool's own filter and leave nothing behind.

`Infeasibility` keeps `Proved` and `NotFound` apart rather than carrying a
`proved: bool`, because they are different sentences to whoever reads the
result: *"your constraints conflict, here are the three involved"* sends someone
to rewrite a formulation, *"we found nothing"* sends them to widen a tolerance.
A flag invites code that ignores it and says the first when it means the second.

Batching measured, on `BATOU`, points/ms:

| expression | batch of 1 | batch of 256 | gain |
|---|---|---|---|
| `x1 + x2` | 8508 | **49463** | 5.8x |
| `x1 + x2 > 20 - x3^2` | 6657 | 17700 | 2.7x |
| `sin*cos+sqrt*abs` | 6225 | 15021 | 2.4x |
| deep arithmetic | 3029 | 4325 | 1.4x |
| `sum(1, 200, i -> var[i]^2 - 3.0)` | 125 | 137 | 1.1x |

Exactly the expected shape: the win is largest where per-call overhead dominates
and nearly absent where the arithmetic does. The trivial case at 49463 is more
than double the best the old scalar path ever managed, and batch-of-one is
*worse* than the old path was — two buffers and a `Col` per call against one
`Vec`. That is the cost of the degenerate batch, and it is the number a tape
should attack.



## Repairing a point rather than discarding it

Geoff's, and it is the other end of a problem wave 1 already touched from one
side. When a solver nominates a point and babel's own `evaluate` then rejects
it, the pool bins the point — and the pool is right to distrust it, because the
solver reasons in **exact real arithmetic** while every filter downstream runs in
`f64`. A point sitting on a constraint boundary can be feasible in the model and
infeasible in the evaluation, by a difference the solver had no way to see. Wave
1's ulp-widening on inverted bounds was the same divergence approached from the
emitter's side.

Discarding is the expensive answer. A solver call is seconds; an evaluation is
under a microsecond. Throwing away the former over the latter, when the miss is
microscopic, is the wrong trade — the point should be **repaired**.

Two things make repair tractable here, and one makes it delicate.

*The residual is graded, not boolean.* That is the whole reason for the `<= 0`
convention: a violated constraint reports how badly it was violated. So a
near-miss already carries its own error signal, and a finite-difference gradient
over `d + 1` evaluations says which way to move — still nothing against the
solver call that produced the point.

*Feasible regions near a nominated point are locally simple.* The solver has
already done the global work. What is left is projection onto a set the point is
almost in.

*But "small" in high dimensions is not small.* Geoff's own caveat, and it is the
concentration-of-measure effect the walker already has to work around: in `d`
dimensions a random direction is nearly orthogonal to the gradient, so isotropic
jitter mostly moves sideways and the step you need grows with `d`. Repair has to
be gradient-directed, not a random walk — the same lesson that made the walker
add axis moves at 200 dimensions.

**The risk is distributional.** A repaired point is drawn from a different
distribution than an unrepaired one: repair pushes points onto the feasible
boundary, and a boundary is exactly where a uniform sample should be *rare*.
Repairing a **seed** is free, because seeds are already not uniform and the
walker's job is to forget where it started. Repairing an **emitted** point would
bias the marginals, and `cvg_benchmarks` is the instrument that would say so —
which makes this one of the few items on the list that arrives with its own
test already written.

## Where CVG stands

`solve` → `Solution` → `ConstraintPool::generate` is settled. Two live strategies, divided by what
each can promise:

- **`AdaptiveSampling` seeds.** It narrows its proposal box toward what it has found, which is what
  makes a narrow region reachable — and which also means it can exclude a part of the region it has
  not seen. Biased, effective, and its points never leave the pool.
- **`HitAndRun` emits.** Converges to the uniform distribution over the region, so what a caller
  receives is governed by the strategy with a guarantee rather than the one with a heuristic.

Which of the three runs is decided per pool by one probing round — see `Route`. Plain sampling is
asked first because where it works it is not a fallback but the *best* option: unbiased by
construction, no burn-in, and none of a Markov chain's trouble with regions in several pieces.

A third strategy sits behind those two: when sampling finds nothing, **Z3** is asked for a first
point, and only then can `Solution::Unsatisfiable` be produced.

**156 of 158 tests pass.**

| red | why |
|---|---|
| `cvg_pools::modulo` | SMT-LIB's `mod` is integer-only and babel's `%` follows Java's sign-of-dividend rule, so the emitter cannot express it. Reported as `Solution::Unknown`, never silently dropped. |
| `p118` | two runs disagree by KS 0.114 against a 0.096 threshold — see below. Unrelated to the solver. |

## What the oracles found

The point of the fairness work was to make "the distribution is fine" a checkable claim rather than
an assumption. It is now checkable, and it caught things. Three open findings:

- [x] **The walker's disconnected-region limit, measured and then routed around.**
      `parabolic_roots_narrowing` under the walker returned 2000 points worth **87 independent ones
      — 4.4% efficiency**: a chain cannot cross a gap it cannot step across, so each of the eight
      chains lived in one band for its whole life and the split between bands was binomial(8, ½),
      50% ± 18%. The band *proportions* were noise.
      Fixed by the easy path, which hands that problem to plain sampling and never starts a chain.
      The limit itself is unchanged and still applies wherever sampling cannot reach — it is one of
      the reasons to want a solver — but it is now measured rather than assumed, and routed around
      where routing around it is possible.

- [x] **P118 does not mix, and it is not marginal.** *Resolved 2026-09-11 by the preconditioning
      entry below; the measurement that pointed there is recorded at the end of this entry.*
      This entry used to read "about 4.5% over,
      consistently" and wonder whether the significance level was merely too tight for the ~200 KS
      tests a suite runs. That was reading a single deterministic draw: `SEED` and `RIVAL_SEED` are
      hard-coded, so the committed number is one sample from a distribution, and it happens to be a
      mild one. Swept over six other seed pairs:

      | pair | worst coordinate | KS | threshold | |
      |---|---|---|---|---|
      | 0 | 11 | 0.1540 | 0.0895 | fail |
      | 1 | 7 | 0.0920 | 0.0935 | pass |
      | 2 | 13 | 0.2140 | 0.0895 | fail |
      | 3 | 13 | 0.0590 | 0.0895 | pass |
      | 4 | 13 | 0.1120 | 0.0895 | fail |
      | 5 | 1 | 0.1580 | 0.0895 | fail |

      Four of six fail and 0.214 is more than double the threshold, so **the "loosen the alpha"
      branch is dead** — no defensible significance level absorbs that. It is a real mixing failure.
      The worst coordinate wanders (11, 7, 13, 13, 13, 1), which is a chain that has not mixed
      rather than one bad variable.

      Any fix has to be re-measured the same way. A single seed pair cannot tell green from lucky,
      and that is exactly how this got mis-recorded the first time.

      **What settled it: the walker converges, it just emits too soon.** With
      `THINNING_PER_DIMENSION` raised from 2 to 20 and nothing else changed, P118 passes all ten
      seeds (209 s against 140 s). So burn-in (2,000 steps, about six independent samples' worth
      under the old kernel) was adequate and the failure was the emission cadence on a tube, which
      is exactly the shape hit-and-run on the sphere crawls along. Raising thinning globally would
      have multiplied the cost of the 200-dimensional cases, which are round and mix at two per
      dimension; the fix had to change what a random direction is, not how many are taken.

- [x] **Preconditioning the walker, which is the likely fix and is also load-bearing for
      equalities.** *Landed 2026-09-11, in `walking.rs`; the module documentation carries the
      argument. P118 passes ten of ten. What was built, against what this entry planned:*
      - The random half of the moves is drawn as `normalise(L z)`, `z` standard normal and `L` the
        lower Cholesky factor of the burn-in states' covariance over the movable coordinates.
        Axis moves are untouched: on an axis-aligned box they are exact Gibbs steps.
      - **Frozen before emission, fitted twice.** The first fit comes from the second half of a
        sphere burn-in, sampled at the emission cadence; a second burn-in walked under it refits,
        because chains that crawled on the sphere never traversed the tube and the first fit alone
        left one seed in ten marginally red (KS 0.154 against 0.149). Refitting before anything is
        emitted keeps the emitted chain Markov.
      - **Shrunk toward the diagonal by `m / n`**, dimensions over states, not by a constant. Burn-in
        states are autocorrelated — a coordinate is refreshed only by its own axis move, every `2d`
        steps — so in 200 dimensions the covariance rests on a few dozen effective samples and its
        spectrum is mostly noise; a fixed weight would have *introduced* anisotropy on a round box.
        `m / n` is the Marchenko–Pastur ratio, which bounds that noise's condition number near four
        below one and collapses to the diagonal at one or more. At 200 dimensions that is the
        diagonal (harmless, and the second burn-in is skipped there since a refit from the same
        counts would collapse the same way); on P118, fifteen dimensions and a few hundred states,
        it is nearly the full covariance. The eigenvalue floor this entry asked for is the wrong
        tool: noise inflates the top of the spectrum as much as it deflates the bottom.
      - Movable coordinates are computed once per walker rather than allocated per proposal, which
        the "what the walker spends its time on" entry had flagged as a suspect.
      *The original reasoning follows.* P118's polytope is a long thin tube — the ±7 couplings chain
      `x1 → x4 → x7 → x10 → x13` and the rest — which is the classic slow case for hit-and-run: a
      uniformly random direction almost always points at a nearby wall, so chords are tiny and the
      chain crawls.

      The fix is to walk in coordinates where the body is round. Take the covariance of the feasible
      points found so far, and apply the linear map that makes that covariance the identity;
      equivalently, draw the direction from the covariance ellipsoid rather than from the sphere.

      **This is a change of coordinates, not a bias.** The distinction matters and is easy to get
      backwards: "steer toward where the points are" is self-reinforcing and destroys uniformity,
      because the chain piles up where it already is. Preconditioning applies the *same* linear map
      to the body and to the walk, so the uniform distribution is preserved exactly — the same
      target, sampled in coordinates that are not adversarial.

      **The trap: adapting the transform as the run proceeds makes the chain non-Markovian**, which
      voids the stationarity argument the uniformity claim rests on. Two standard escapes — freeze
      the transform after a warm-up phase and walk the rest under a fixed map, or use a diminishing
      adaptation schedule so the changes vanish fast enough for the ergodic result to survive.
      Freezing is simpler, and the pool already has a phase structure to hang it on.

      **Why it matters beyond P118:** a thin equality band *is* a long thin tube. The
      surface-then-band split hands the walker a slab by construction, so whatever makes P118 mix is
      likely the same thing that makes the band case work — and conversely, equality benchmarks
      added before this lands will produce more red of exactly this kind. Judge them accordingly.
      One caveat specific to bands: the covariance of a slab is near-singular in the thin direction,
      so the map needs a floor on the smallest eigenvalue or it inverts into nonsense.

## Not ported from the Kotlin

A pass over `sojourn-CVG` for anything missed. Most of it is genuinely skippable; two items are not.

- [x] **The easy path, from `sojourn.kt`.** Done. Turned `tough_single_var` and
      `parabolic_roots_narrowing` green outright, and `Strategy::UniformSampling` is now first in
      `DEFAULT_STRATEGIES` rather than test-only. Original note follows.

      **The easy path, from `sojourn.kt`.** `makeSampleAgent` runs one round of the *fair*
      (non-adaptive) sampler first, and if it lands more than `EASY_PATH_THRESHOLD_FACTOR` (0.1) of
      the target it stays on plain sampling forever and never engages the walker or the solver.
      Worth porting for three reasons: plain sampling is unbiased by construction, it is far
      cheaper than a burn-in, and it makes the disconnected-region problem above disappear on every
      region it can reach — including `parabolic_roots_narrowing`, whose 4.4% would become 100%.
      Note also that `RandomSamplingPool.create` — non-adaptive — was a *production* pool over
      there, not just an oracle. `Strategy::UniformSampling` should probably be in the default
      strategy list rather than test-only.

- [x] **Eight `Z3SolvingPoolFixture` cases never ported.** Done — all sixteen are now present,
      four of them green (`logarithms`, `modulo_with_a_symbolic_divisor`,
      `equality_with_a_loose_tolerance`, `sine_below_zero`). Porting them also turned up two
      *mis-ports* from V0: `ceiling_and_floor` had been written as a two-variable
      equality-with-tolerance when the fixture's case is four variables and two inequalities, and
      `absolute_value` as one equation over two variables when the fixture pins three variables to
      three magnitudes. The first had been sitting in the red column looking like the solver's
      problem; it passes. `a_simple_inequality` is ours and appears nowhere in the fixture, now
      labelled as such. Original note follows.

      **Eight `Z3SolvingPoolFixture` cases never ported.** `cvg_pools` has nine of the sixteen.
      Missing: `logarithms` (`2 < ln(x1)`), `mod` (`x1 % 3.0 >= 2`), `mod with symbolic divisor`
      (`3 > 10 % x1`), `constants` (`x1 == pi +/- 0.001`), `sgn`, `vars`
      (`1.5 == var[1] + var[2] +/- 0.001`), `equality` (`x1 == x2 +/- 0.1`), `sin`
      (`sin(x1) <= 0`).
      These are not solver tests — they are coverage of *babel features* under the pool, and
      several are inequality-shaped and would go green today: `2 < ln(x1)` admits 26% of its range,
      `sin(x1) <= 0` admits 50%, `x1 == x2 +/- 0.1` is a diagonal band. `vars` is the only exercise
      of `var[i]` under CVG anywhere. Cheap to add and the largest single gap in the port.

- [x] **`Solution::Unsatisfiable` is reachable.** It has a producer, and two tests: a
      contradictory pair returns it naming both constraints, and a satisfiable pair does not return
      it at all — because a verdict that never stays quiet means nothing.

Deliberately skipped, with reasons:

- **`EuclideanNormSanityFixture`** tests `findDispersion`, which was dropped for
  Kolmogorov-Smirnov. Its two cases do confirm the definition (mean Euclidean distance from the
  centroid: 2/3 for {0, 1, -1}, sqrt(2) for the unit square), which is what identified the JVM's
  unasserted `dispersion` numbers as mean absolute deviation. Nothing left to port.
- **`TransposeFixture`** tests a list-of-lists transpose helper. `Point` is positional, so there is
  nothing to transpose — though this may come back with the structure-of-arrays tape.
- **`IntegrationFixture`** is a single `TODO()` against a CLI `main` we do not have.
- **`IntegrationTests`** asserts `assertThat(points).isEqualTo(20_000)` on a `List` — comparing a
  list to an int, so it can never pass — and follows it with `assertThat(points.all { ... })`,
  which has no terminal assertion and therefore asserts nothing. The *intent* was throughput on
  P118; that belongs with the benchmark work, not here.
- **`LanguageFixture`** is JVM/Kotlin sanity (`1E20.toInt()`, `TreePVector`, operator precedence)
  plus decimal-to-rational parsing. Only the last matters, and it belongs with the SMT emitter.

Worth knowing while reading that code: **`sojourn.kt` does not compile.** There is a bare `fail;`
at line 286, mid-way through the pool-balancing logic, next to a comment reading "ok, running with
'x1 < 0.0001^(x2+1)' scares me" and a note that the improver offers much higher variance but the
adaptive sampler keeps getting picked. So the round-robin balancing — allocating each pool's budget
by measured throughput, and avoiding pools whose dispersion is poor — was work in progress that was
never finished or run. Treat it as a design sketch, not as behaviour to reproduce.

## Speculation

Ideas with no test behind them yet. Each waits for a fixture that wants it.

- **A walk on the manifold, by Newton.** Driving parametrises an equality's surface one
  coordinate at a time, which is why a coupled pair (`x1 + x2 == 3` with `x1 - x2 == 1`)
  drives neither and a disjunction needs a plan per branch. The general move is the
  manifold one: at a point on `{g = 0}`, step in the tangent space — the null space of the
  Jacobian, which the tape's reverse sweep gives for a few evaluations whatever the
  dimension — and project back with Newton on the KKT system (`cvg/newton.rs`), about one
  repair per step (Zappa, Holmes-Cerfon & Goodman 2018, *Monte Carlo on manifolds*). It
  follows an arm of a disjunction by construction and switches only at the crossing, where
  the gradient vanishes; it subsumes driving for every smooth equality and handles the
  coupled ones; jumps (`floor`, `%`, `sgn`) keep retraction. The band `±t` becomes an
  offset off the centre line, fine for a design and a question for a uniform sample. Not
  built: no formulation has arrived with coupled or branching equalities at a dimension the
  bisection cannot seed.

- **Smooth proxies for the jumps.** `floor`, `ceil`, `sgn` and `%` have no useful derivative,
  so a constraint using one is compiled without a gradient and its projection falls to COBYLA.
  The lie worth considering: a continuous, differentiable proxy — a staircase with sigmoid
  risers standing in for `floor`, say — used *only to steer* a Newton step, with every landing
  still judged by the true oracle. It is a lie only about the step direction, never about
  feasibility, which is what makes it tempting. The sound alternative is already in the crate:
  branch-and-bound over intervals (`prune`) splits the domain at the jumps, inside each piece
  the function is smooth and the gradient real, and Newton runs within the leaf. Do the sound
  one first if either is ever wanted; a proxy is a fixture away from being worth trying.

## Watch these

- **The rewrites are accumulating ad-hoc, and there is no pipeline to hang them on.** Wave 1 #1 was
  small enough to bolt straight onto `emit`. The next few are not. Monotone inversion rewrites a
  constraint, causalization *splits* one problem into a driven part and a solved part, and a DOE
  over a driven argument turns one problem into `n`. That last one is the tell: these are not
  transformations of a document, they are transformations of a **set of problems**, and doing them
  in sequence inside `emit` would mean control flow nobody can follow.
  The shape Geoff sketched — a `Vec<SolverProposal>`, each pass being
  `SolverProposal -> Vec<SolverProposal>`, the whole pipeline a `flat_map` chain — is the right
  instinct and worth writing down before it is forgotten. Causalization becomes a fan-out, the DOE
  becomes a fan-out, and inversion is the degenerate one-to-one case. It is combinatoric by nature,
  which is a reason to keep each pass cheap and to cap the breadth, not a reason to avoid the shape.
  **Deliberately not designing this yet.** There is one pass today and a design for one pass is a
  guess about the second. The trigger to build it is the *second* fan-out pass — that is when the
  ad-hoc version starts costing more than the abstraction, and step 7 ("measure the residue") is
  where we will know what the passes actually have to be.

- **Read-ahead is not free on expensive problems.** `BATCH_SIZE * CHANNEL_CAPACITY` points get
  produced on spec. Raising either costs real time on `top_corner_200d` and nothing anywhere else,
  so measure that test specifically before touching them — 64 points of read-ahead is about three
  seconds there.
- **A broken exhaustion signal is a hang, not a failure.** Which is why
  `a_pool_that_can_never_deliver_reports_exhausted_rather_than_blocking` was written before anything
  else in that increment, and why the nextest timeout sits behind it.
- **`Drop` must drain before it joins.** `Drop` runs before the struct's fields do, so the receiver
  is still alive while the pool waits on its worker — and a worker parked on a full channel stays
  parked until somebody reads. Draining first is what makes the join terminate rather than
  deadlock; `dropping_a_pool_mid_fill_does_not_deadlock` is the guard.

Green, but not robustly so — worth knowing before treating them as settled.

- **`parabolic_roots_ribbon` passes on about six seed points — and 2026-09-02 it stopped.**
  Swapping the pool's generator from ChaCha12 to Xoshiro256++ (brute squad step 2) re-rolled
  every seeded verdict, and this one landed with no point in the far band: `DisjointBands`
  reports "found nothing in the band". The assertion was not loosened; the honest fixes are the
  ones below. Original note follows. The band at x = 1 is 6.6e-6 wide in
  a box of 10, so a two-million-proposal probe lands roughly two or three points across both bands,
  and the walker's chains then start from whichever of those it drew. It went red before the easy
  path added a second probing round and green after — but nothing guarantees the next RNG seed puts
  a point in the far band. A solver is what makes this deterministic.
- **`signum` sits on the easy-path threshold.** Its feasible fraction is about a thousandth, and a
  hundred-point probe at 100x oversampling lands on the order of ten hits against a threshold of
  ten. It currently routes to sampling and passes. It could route either way.
- **A test restating a library constant went stale, silently.** `cvg_benchmarks` had its own
  `PRODUCTION` strategy list, so when `UniformSampling` joined the defaults the benchmarks spent a
  full run measuring a configuration the product had already left behind — and reported the old
  failures convincingly. `DEFAULT_STRATEGIES` is now `pub` (doc-hidden) and the tests import it.
  Worth remembering the shape of that mistake: the test was not wrong about what it measured, only
  about what it claimed to measure.

## The solver question, settled

Shopped for a replacement to Z3 and came back with Z3. Measured, on this machine:

| babel needs | Z3 | cvc5 |
|---|---|---|
| `sin` `cos` `tan` | parses, instant `unknown` | parses, **20s timeout** |
| `asin` `atan` `sinh` | parse, instant `unknown` | — |
| `ln` `log10` `log2` `exp` | **rejected — no such symbol** | **rejected — no such symbol** |
| `sqrt` `cbrt` | rejected — we encode as `y*y = x, y >= 0` | native, but the encoding already works |
| `^`, constant exponent | **works** — `(^ x 0.5)` on a pinned `x` gives 3.1622776601683795 | positive integers only |
| `^`, variable exponent | **`unknown`** — `2^x5 < 20`, and every log encoded through it | **rejected** |
| `pi` | a symbol, but never a model value — see below | — |
| `e` | **rejected — no such symbol** | — |

**Net capability gain of cvc5 over Z3 for the functions babel has: zero.** It adds `exp` (babel has
no `exp`) and native `sqrt` (already encoded), and loses real-exponent `^`. dReal has the best theory
by a distance but is Unix-only, x86_64-only on Linux, no arm64 anywhere, last release June 2021.

**Logs specifically, since the question came up.** Z3 has no logarithm under any spelling — `ln`,
`log`, `log2`, `log10` and `exp` are all parse errors, and so is the constant `e`. The obvious
workaround is inversion through `^`, writing `ln x = y` as `x = e^y`, and it does not work either:
open, it answers with the trivial origin `y = 0, x = 1`; narrowed to `9 < x < 11` it answers
`unknown`; *pinned* to `x = 10` it still answers `unknown`. **Z3 cannot compute `ln 10`.** The rule
underneath is that Z3's `^` is polynomial in practice — a variable in the base is fine, a variable
in the exponent is not — which is one fact covering `ln`, `log10`, `log(b, x)` and `2^x` at once.

Two traps found while measuring, worth not rediscovering:

- **`(^ -8.0 0.3333)` answers `sat` with `y = 0`.** Z3's `^` is underspecified for a negative base
  with a fractional exponent, so it is free to return anything and does. Sound by its own semantics,
  wrong by babel's `cbrt`. The auxiliary-variable encoding avoids this and should stay.
- **`sat` does not mean every variable has a value.** Ask about `pi`, or about `sin` on a pinned
  argument, and the model comes back missing the irrational entries, because `as_rational` fails and
  `approx(17).parse()` fails after it. Already handled — `escalate_for_seed` falls back to the lower
  bound and lets the pool's own filter judge the point — but the behaviour is deliberate, not
  incidental, and the fallback is why `Sat([])` never becomes a bogus seed.

**Nobody is coming to fix this.** SMT-COMP 2025 ran fifteen quantifier-free divisions and a dozen
quantified ones, and **not one of them involves a transcendental function**. The nonlinear real
division is `QF_NonLinearRealArith` — polynomials. No competition category means no competitive
pressure, which is the whole explanation for why a field this active has left `sin` where it is.
For what it is worth, the division babel does live in was won by **Z3-alpha** (2856 solved), a Z3
fork with learned strategy selection, ahead of **z3siri** (2803, also Z3-derived), **cvc5** (2766)
and **Yices2** (2746). The top two are Z3 wearing a hat. Yices2 is worth knowing about — there is
2026 work extending its MCSAT core to `sin` and `exp` that reportedly beats the field — but it is
a research prototype, not a release, and its licence is GPL-family rather than Z3's MIT.

Worth keeping, though: **cvc5 *can* be linked in-process on Windows/MSVC**, which the crate's
`#[cfg(not(unix))] panic!` appears to deny. That panic guards only the *static build-from-source*
path; the dynamic path has no platform gate. Proven end to end — MinGW-built DLL loaded by an MSVC
Rust binary through cvc5's C API, built, linked and ran. One upstream bug in the way:
`cvc5-sys` canonicalises the include dir, which on Windows yields a `\\?\`-prefixed
extended-length path, and libclang silently ignores such a path as an `-I` search directory — it
opens the main header and then cannot resolve that header's own includes. Five-line fix, worth
sending upstream. If a future cvc5 grows real trigonometry the door is open.

## Equality constraints

The biggest open question in the pool, and the one the driven-variable work runs
straight into. What follows is the shape of the problem, not a design.

### There is one syntax and at least six meanings

Babel admits exactly one equality form. `BabelParser.g4`:

```
booleanExpr : scalarExpr eq scalarExpr plusMinus literal
```

So `a == b +/- t`, and `t` **must be a literal** — there is no bare `a == b`,
and no computed tolerance. `rewrite_booleans` lowers all of it to
`max((b - t) - a, a - (b + t))`, one residual, and by the time anything
downstream looks, an equality is indistinguishable from any other constraint.

That is the problem. One syntax covers at least six structurally different
things, each of which wants different handling:

| | shape | corpus |
|---|---|---|
| **A** | **Pinned to a constant.** A variable equals a literal. No search at all: the dimension is gone. | `x1 == pi +/- 0.001`, `x2 == e +/- 0.001` |
| **B** | **Driven.** One variable alone on one side, appearing nowhere else in the constraint. Evaluate it, never solve for it. | `y == sin(x) +/- 1e-6`, `x1 == sqrt(x2) +/- 1e-4`, `x3 == cbrt(x4) +/- 1e-4`, `x1 == x2^3 +/- 1e-4` |
| **C** | ~~Driven after rearrangement.~~ **Refused.** A variable on both sides makes the equality *implicit* in it — same answer as F, which is where the two collapsed. | `x2 == x1 + 1/2*x2 - x3/x4` — write it `1/2*x2 - x1 + x3/x4 == 0` |
| **D** | **Multi-valued.** The equation names a set with several branches. Nothing is driven; a solver may pick a branch and the walker must then stay on it. | `abs(x1) == 1 +/- 0.001` (two roots), `(x + 2) * (x - 1) == 0 +/- w` (two bands) |
| **E** | **Under-determined system.** More variables than equations, so *which* variables are driven is a choice — this is where matching earns its keep. | `1.5 == var[1] + var[2] +/- 0.001` together with `1.5 == var[2] - var[3] +/- 0.001`: two equations, three variables, one degree of freedom left |
| **F** | **Implicit. Refused.** The variable is on both sides, so nothing can solve the equality for it. | `sin(x) == x/2`, `x == x*x + 2` — **out of scope.** See below |

A and B are dimension reductions. D is a branch choice. E is a matching problem.
**C and F are one row and it is refused** — see below.

**C and F are one row, and it is refused.** They were written as separate
problems — C rearrangeable, F needing Newton — and both are the same thing seen
from different sides: a variable named on both sides of an `==` makes the
equality *implicit* in it, and nothing downstream can drive what it cannot
isolate. `ConstraintSystem::new` rejects it with `SystemError::Implicit`:

> constraint c0 is implicit in x2: it names x2 on both sides, so nothing can
> solve it for x2 without rearranging it first. Write x2 on one side only —
> `a == b + a/2` is `a/2 - b == 0`

**"Implicit", not "cyclic".** A cycle is a mutual dependency *between* equations
— `x1 == f(x2)` with `x2 == g(x1)` — which `classify::plan` meets and handles by
driving neither. Those stay legal. One equation that cannot be solved for a
variable it names is the textbook implicit form, and that is what this is.

**What is refused is a phrasing, not a problem.** `x2 == x1 + x2/2 - x3/x4` and
`1/2*x2 - x1 + x3/x4 == 0` describe the same set and only the first is implicit.
`cvg_pools::simple_arithmetic` is that fixture written the other way round and
still passes, which is the evidence that nothing was lost but the implicitness.
It is also not a claim of emptiness: `sin(x) == x/2` has three solutions and
`x == x*x + 2` is an ordinary quadratic, so `Unsatisfiable` would be false.
`SystemError` is the shape for "we will not try".

**Two rules were tried and discarded before this one.**

The first was "on both sides *and* inside something `cvg::emit` refuses", which
let row C through on the grounds that Z3 can answer it. True, and beside the
point: nothing could *drive* it, so it fell to whatever the sampler managed and
read as a capability that did not exist. It also needed a second copy of the
emitter's refusal list, which would eventually have disagreed with the first.

The second required one side to be a bare variable, which made the rule depend
on *spelling*: `x == x*x + 2` refused and `x*x == x + 2` allowed, the same
equation. Measured, not supposed. The rule is now a single sentence with no
exception for how either side is written.

`Driven by:` `cvg_equalities::an_implicit_equality_is_refused_by_name` for the
refusal, and `the_rearranged_form_is_accepted_and_explored` for the line being in
the right place — the second matters more, because the failure mode here is
refusing too much.

### The tolerance belongs to the strategy, not to the constraint

`+/-` exists because rejection sampling cannot land on a measure-zero set. But
each consumer wants something different from the same user constraint:

| consumer | wants | why |
|---|---|---|
| the SMT solver | **`a == b`, exactly** | it reasons in exact reals and can hit a measure-zero set. Handing it a band asks for a thicker answer than it needs, and the band is the part it is worst at |
| the walker | **the band** | it needs volume to move in. A surface has none |
| rejection sampling | **the band**, and a generous one | it will never hit anything thinner |
| the pool's final filter | **the band** | it re-checks in `f64`, where exact `==` is satisfied by essentially nothing |

So the solver should be asked for a point *on the surface*, and the walker
should explore the band *around* it. That decomposition is better than either
alone: the solver is good at finding the surface and bad at the band, and the
walker is the reverse.

**The expression we ask a solver to solve is therefore not the one the user
wrote.** That is the sentence this whole section turns on, and it is what makes
the fan-out pipeline the right shape rather than a nicety —
`SolverProposal -> Vec<SolverProposal>`, one user constraint becoming several
proposals, each aimed at a different consumer. Dropping the tolerance is one
such rewriting; causalizing a driven variable is another; splitting D into its
branches is a third.

### Two things that will bite

**A point exactly on the surface in real arithmetic is not on it in `f64`.**
The solver returns a witness for `a == b` and the pool re-checks in `f64`, where
it may miss by an ulp — the same divergence that made wave 1 widen inverted
bounds by one ulp, seen from the other side. It is also exactly the case
Geoff's *repair* item exists for: perturb the near-miss onto the surface rather
than discard it. **Dropping the tolerance for the solver and repairing its
output are one piece of work, not two.**

**A thin band is the walker's hardest case.** Hit-and-run in a slab has almost
every chord tiny, which is what `parabolic_roots_narrowing` and
`parabolic_roots_narrow` already measure. Making the solver's job easier by
handing it a surface makes the walker's job harder by handing it a slab, so the
two have to be assessed together.

### `TopCorner200D` as an equality, which is the case worth thinking about

`top_corner_200d` is 200 *inequalities* — `xi > 10.5` over `10..11`, a corner
that is `0.5^200` of the box, or `6e-61`. Write the same problem as 200
*equalities*, `xi == 10.5 +/- t`, and two things follow.

**Drop the tolerance and the feasible set is a single point.** 200 exact
equalities over 200 variables is fully determined. The solver finds the one
solution immediately — it is trivial for it — and then there is nothing to
sample, because "give me 500 distinct points" has no answer.

**Keep the tolerance and sampling still cannot find it.** The feasible set is a
product of 200 bands, so its volume is `(2t)^200`, and that decays exponentially
in the dimension count no matter how generous `t` is:

| `t` | band per dimension | fraction of the box | samples for one hit |
|---|---|---|---|
| 0.45 | 0.9 | 7.06e-10 | 1.4 billion |
| 0.1 | 0.2 | 1.6e-140 | — |
| 0.01 | 0.02 | underflows `f64` | — |

A band covering **ninety per cent of every dimension** is one hit in 1.4 billion,
and a realistic tolerance does not have a representable fraction. So the
equality twin is not the easy case: **it is reachable only through an SMT
beachhead with the walker building outward from it**, which is the same answer as
the inequality version and for the same reason.

That is the strongest argument for the surface-then-band split rather than a
complication of it. And the geometry is encouraging — the band is an
axis-aligned convex box, exactly what `top_corner_200d` already is, so a walker
that mixes in one should mix in the other. One thing to check rather than
assume: whether anything in `walking.rs` carries an *absolute* scale assumption,
since a `t` of `1e-4` makes the region four orders of magnitude thinner than
anything the walker has been run against.

It is also the missing benchmark. Nothing in the corpus is a high-dimensional
equality band, which is precisely the shape the surface-then-band split would
produce, and precisely where the walker's mixing is least understood.
`top_corner_200d` forced axis moves into the walker; its equality twin is the
natural place to find out what the next such surprise is.

### What the taxonomy actually measures — `tests/cvg_equalities.rs`

Written before the classifier so the classifier gets designed against measured
failures rather than against the table above. Each case is an existing green
`cvg_pools` fixture with the slack taken out — same shape, tolerance dropped to
`1e-9` — plus a **coverage** assertion the pool fixtures do not make: name the
coordinates that are genuinely free, and require the sample to span a stated
fraction of their range. That is the claim a solver alone cannot satisfy, since
one witness repeated two hundred times is feasible, countable, and explores
nothing.

Seven of nine are red, and the failures are more uniform than expected:

| case | row | what happened |
|---|---|---|
| `a_pinned_variable_does_not_freeze_the_free_one` | A | **passes** — `x1` pinned at `1e-9`, `x2` still spans >80% |
| `a_pinned_system_is_still_satisfiable_because_the_tolerance_is_not_optional` | A | **passes**, and see below |
| `a_driven_variable_is_evaluated_not_searched` | B | `Unsatisfiable { NotFound }` — the whole system comes up empty |
| `a_driven_variable_a_solver_can_reach_is_still_explored` | B | `x2` and `x4` each span **0.0000%** |
| `a_variable_on_both_sides_linearly_is_still_driven` | C | `x1` and `x3` each span **0.0001%** |
| `both_branches_of_an_absolute_value_receive_points` | D | `x1` spans **0.0000%** — one branch only |
| `both_bands_of_a_parabola_receive_points` | D | `x` spans **0.0000%** — one band only |
| `an_under_determined_system_explores_its_remaining_freedom` | E | `x1` spans **0.0000%** |
| `the_tolerance_floor_is_where_it_was_left` | — | floor measured at `1e-4`, ratchet set to `1e-9` |

**Every failure is the same failure.** Two hundred *distinct* points, all
feasible, spanning essentially none of the range — the solver's witness plus
jitter. The pipeline is not failing to find the set; it is failing to move along
it. That is one defect, not five, and it says the classifier's first job is not
telling A from B but **telling the walker which directions are free**.

**The floor is `1e-4`.** Measured, not guessed, on `x1 == x2` — one linear
equation, two variables, one degree of freedom, nothing to invert or branch. If
anything survives a tight tolerance it is that, so `1e-4` is the ceiling on the
whole family. Every corpus fixture sits at `1e-3` to `1e-5`, which is to say
they were all written just above the floor, and that is why they are green.

**Two things this got wrong, worth keeping.** The "fully determined" case cannot
be written today: `+/- 1e-9` still leaves a `2e-9` square holding an enormous
number of representable doubles, so two hundred distinct points genuinely exist
and the pool finds them. A tolerance is not optional in this grammar, so the
one-point case only appears once the tolerance is *dropped for the solver* — it
is a consequence of the surface-then-band work, not a precondition for it. And
the B case does not merely under-explore, it reports `NotFound`: with the box
moved to `2..3`, the accidental `sin(0) = 0` seed that carries the loose fixture
is gone, and nothing else can reach the curve.

### The classifier — `src/cvg/classify.rs`, rows A and B shipped

Built against the table above rather than the other way round. It reads one
equality and answers `Pinned`, `Driven`, `Implicit` or `Opaque`, then
resolves a whole system into a **`Plan`**: which schema positions the walker
moves, and which it computes from them, in evaluation order.

**Row B needs no inverse**, which is why it was worth doing first. `y == sin(x)`
does not require solving `sin` for anything — `y` is alone on one side, so it is
a formula for `y`. The test is pure syntax: one side is a bare `Kind::Global`
that does not appear on the other. `monotone()` in `rewrite.rs` stays private and
unused; it becomes relevant for row C.

Rows D and E answer `Opaque`, which is honest rather than a stub. Rows C and F
answer `Implicit` and are refused at construction — see above.

Refusing to drive is always safe and this leans on it: an ambiguous definition
(one variable defined by two equalities), a cycle, a variable the schema does not
carry, a definition that will not compile — each leaves its constraint exactly as
it was. **No constraint is ever dropped**, and feasibility is still checked
against all of them, so a wrong drive costs rejected moves and can never cost a
wrong point.

**Results.** Rows A and B green, the tolerance floor on `x1 == x2` gone from
`1e-4` to past `1e-17` — driving makes the tolerance nearly irrelevant for that
shape. `a_driven_variable_is_evaluated_not_searched` was reporting
`Unsatisfiable { NotFound }`, i.e. nothing ever proposed a point; it is green.

### Driving is a Gibbs step, not an evaluation — the correction that mattered

The first version assigned `y = f(free)` and it was **wrong**, in a way worth
recording because the reasoning is seductive and the failure is loud.

Babel has no bare equality. `y == f(x) +/- t` admits the whole band, so
collapsing it to its centre line throws away a dimension of the feasible region.
On `TopCorner200DAsEqualities` — two hundred variables, each `== 10.75 +/- 0.2` —
every coordinate pinned to exactly `10.75` and the pool returned *the same point
two hundred times*. The benchmark caught it on the duplicate check.

The fix is to draw uniformly from `f(free) ± t`, and it is not a fudge: with the
other coordinates held, the feasible slice for a driven variable **is** that
interval, so drawing uniformly from it is a Gibbs step and leaves the uniform
distribution invariant. Evaluating to the centre does not.

Where several constraints mention the same driven variable, the band used is one
of them and the others are not consulted, so a draw can land outside them and be
rejected like any other candidate. Correct, and less efficient than a full
conditional — worth revisiting if row E's matching lands.

**The density worry did not materialise.** The concern going in was that driving
would change the sampled distribution away from what rejection sampling produces,
and that `parabolic_roots_wide` would go red saying so. It did not — but only
because `(x + 2) * (x - 1) == 0` is row D and classifies `Opaque`, so nothing is
driven there. The question is still open and will arrive with row D.

One deliberate restraint: the retraction is applied in the **adaptive** sampler
and not in `Strategy::UniformSampling`. That strategy is the fairness oracle the
benchmarks measure against, and helping it would make the oracle a copy of the
thing it judges.

### `TopCorner200DAsEqualities` — which now passes

*Written when it was red; the finding stands and the verdict has moved.*

The missing benchmark, now in `cvg_benchmarks.rs` beside its inequality twin:
200 bands `xi == 10.75 +/- 0.2` over `10..11`, volume `0.4^200`, against
`top_corner_200d`'s corner of `0.5^200`. The tolerance is deliberately absurd —
a band covering forty per cent of every dimension is far past anything a user
would write, and is still one hit in `10^79` by rejection.

It delivered all 200 points and failed on *uniformity*, marginally: KS 0.1698
against 0.1628 on `x70`. **That was a much better result than predicted.** The
expectation was that it would not reach the region at all; instead the walker
reached it and moved in it, and was merely slightly non-uniform. The axis moves
that `top_corner_200d` forced into the walker were evidently doing the work here
too — a band is axis-aligned, exactly like a corner.

**It is green as of the classifier.** Every one of the 200 constraints is row A,
so the whole system is driven and each coordinate is drawn uniformly from its own
band — which is precisely what `UniformMarginals` asks for. The marginal KS
failure is gone rather than narrowed.

So the high-dimensional band is **not** the hard case. The hard case is the
low-dimensional one, where the region is a curve or a pair of points rather than
a slab, and there is no axis direction to move along. That inverts the priority
the notes had.

### Row D — and it was not branch selection

The two multi-valued cases were red because of **seed poverty**, not because
anything was blind to branches. Sampling finds nothing at `1e-9`, so `run()`
escalates; `smt::escalate_for_seed` returns exactly one witness; all eight chains
start from it; and hit-and-run crosses a gap only by luck. `walking.rs` said so
in its own doc comment — *a region in several pieces is only covered if the
chains start in several pieces* — and nothing had ever handed it a second piece.

So **row D still classifies `Opaque` and `classify.rs` was not touched.** What
changed is where seeds come from. Same lesson as row B, where the payload turned
out to be the walker split rather than the label.

**Gap-covering.** After the first batch, take the box the points actually reached
and re-ask the solver with the `InputVariable` bounds narrowed to a slab outside
it — `smt::seed_within`, which is `escalate_for_seed` with the box as a
parameter. Found `x ≈ 1` over `-5..5`? Ask about `-5 ..= 1` and Z3 answers with
the other root. Slabs are ranked by the fraction of their coordinate's range they
cover and the budget is spent down that list, so at 200 dimensions the calls go
where there is most to find rather than over a 400-long list of empty ones.

Sound because narrowing the box makes each query a *sub-problem*: the constraints
are untouched, so a witness is feasible for the real problem. Only
`Verdict::Seed` is read — an `Impossible` from a narrowed box means the slab is
empty and says nothing whatever about the problem.

### Two things that were wrong on the first attempt

Both are the same mistake in different clothes: assuming a mechanism reaches the
rare thing when the common thing is right next to it.

**A slab shares an endpoint with the covered region, and the solver sits on it.**
Asked about `-5 ..= 1` having already found `x = 1`, Z3 returns `x = 1` — a
perfectly good answer inside the slab — so every query re-found the same
component and the budget went nowhere. Fixed by bisecting: when the answer is
inside the covered box, halve the slab away from that endpoint and requeue it.
`-5 ..= 1` becomes `-5 ..= -2`, which holds the other root and nothing of the
first. Bisection because it converges with no separation constant to argue about,
and a component skipped by an over-eager halving is picked up from the other side
in a later round.

**`start_chains` picked at random, and the rare seed drowned.** By the time
chains start, the seed set is thirty-odd points walked out from the first witness
plus *one* seed in the piece nothing had reached. Eight uniform draws from
thirty-three points miss the interesting one about four times in five —
`both_bands_of_a_parabola_receive_points` measured exactly that, staying red
after the gap seed was already being found. Replaced with **farthest-point
selection**: take one at random, then repeatedly take whichever candidate is
farthest from everything already taken. The rare seed is not one point among
many, it is the farthest point there is. Capped at a 256-candidate window so the
quadratic cost does not grow with a long-running search.

`PointSource` also gained `reseed`, because `start_chains` only fills up to
`CHAIN_COUNT` and a seed arriving *after* eight chains exist was being ignored
outright. The walker drops its chains and pays the burn-in again, which is the
right trade against a chain in the wrong component.

### The nine original cases pass — and two of them were not testing what they said

Rows C and E went green with the row D work, which was flagged as possible and
not promised. Verified across five seeds, since `SEED` is a constant and one draw
is not evidence.

**But the mechanisms for C and E do not exist.** `classify::shape` answers
`Opaque` for both; the rearrangement and the matching are unwritten. They passed
because gap-covering reaches the region anyway — "the walker is stuck in a
corner" does not care whether the cause is a gap or a thin connected set.

So the tests were measuring a symptom that had another cause, and a green bar was
telling us the feature existed. **Extent was the flaw.** `Case::coverage`
measures `max - min`, and gap-covering places its seeds at the edges of the
uncovered box *by construction* — which is exactly where a span is measured.
Sixteen well-placed points satisfy a span assertion on any number of coordinates
while occupying nothing in between.

`Case::occupancy` is the replacement: a grid over the free coordinates, and how
many cells must receive a point. `n` seeds fill at most `n` cells however
cleverly they are placed, so a threshold above the seed budget can only be met by
a search that moves along the feasible set. The two strict cases sit at **3 of 64
cells** and **3 of 80 bins** — not marginal, and not a threshold anyone tuned.

Their numbers are bounded by the geometry rather than picked for difficulty, and
that mattered: the first attempt asked for 40% span on `x2` when the constraints
allow only 37.5%, and 30 of 40 bins when only 15 are reachable — *below* the seed
budget, so it would have proved nothing even if it had failed. Both are worked
through in the tests' own doc comments.

- [x] **Rows C and F — refused, not built.** A variable on both sides makes the
      equality implicit in it, and `ConstraintSystem::new` rejects it naming the
      rearrangement. The three strict row C cases collapsed to one refusal test
      plus one that the rearranged form is still accepted and explored. Written
      up above, under *Equality constraints*.

- [x] **Row E — green, and not by the route it was written for.** Two equations
      over three variables leave one degree of freedom, and *which* two get
      driven was supposed to need bipartite matching. It did not: once `var[i]`
      resolved and isolation could reach a compound side, each equation isolated
      a different variable on its own and `plan`'s existing topological order
      handled the dependency between them.

      **Matching, built 2026-09-16.** The case that needed it: `x1 + x2 == 3`
      with `x1 + x3 == 2`, where both equations would drive `x1`, `plan`
      refused to choose between them and drove neither, and three variables
      under two equations were left entirely free — twelve occupied cells of
      forty. Now `classify::drivable` lists every variable an equality can be
      solved for (bare side first, then whatever `reaches` can peel, in schema
      order) and `plan` takes a maximum bipartite matching of equations to
      variables by Kuhn's augmenting paths — equations in order, candidates in
      order, so it is deterministic — and hands the result to the same
      topological ordering as before, which still refuses a cycle. Polynomial,
      equations × edges; a chain of ninety-nine equations over a hundred
      variables matches and orders in the noise of a 70 ms solve
      (`torture_tests::a_chain_of_ninety_nine_equalities_is_matched_and_walked`;
      the 13 s that test costs in debug is the walker retracting ninety-nine
      coordinates a step through burn-in, not the matching). A variable wanted
      by two equations that can drive nothing else is driven once and the other
      equation stays a constraint, as before.
      `Driven by:`
      `cvg_equalities::two_equations_wanting_the_same_variable_strand_each_other`

- [x] **Isolation — a variable that occurs once is peeled out of a compound
      side.** `x1 + x2 == 3` drives `x1` as `3 - x2`, and it was failing where
      `y == sin(x)` worked purely because the classifier tested for a bare
      variable on one side. Seven arithmetic rules, one test each; five occupied
      bins of sixty became a full traversal in 0.2s, driven rather than
      solver-bound.

      Not done, and separate steps for good reasons: **unary inverses**
      (`sqrt(x1) + x2 == 3` for `x1`) need a *symbolic* inverse table, where
      `rewrite::monotone` holds numeric ones for comparisons against a literal,
      and they bring the branch problem — `asin` is a principal value, so
      isolating through `sin` silently picks one solution of infinitely many.
      **Gathering** (`x2 == x1 + x2/2`) is normalisation and the start of a CAS;
      it is refused as implicit instead.

- [x] **`var[i]` resolves at bind time.** `ConstraintSystem::new` is the first
      moment a schema exists — `parse` has none, and `Kind::Global` indexes the
      expression's own symbols while `var[i]` indexes the schema — so nothing
      earlier could bridge them. Resolving there means `classify` needed **no
      change at all**, which was the test of whether the seam was right.

      It also fixed a bug of its own: `substitute` replaced a loop parameter with
      a literal and did not fold, and `fold_constants` runs *before* unrolling,
      so `var[i-1]` stayed `var[2 - 1]` and `emit` refused it as a computed
      subscript. Every arithmetic subscript in the corpus, Rosenbrock included,
      was being withheld from the solver for want of one reduction.
### Measuring "did we sample this region" — the instruments and their reach

`Case::occupancy` is right for what it currently measures and **does not
generalise past about three dimensions**, which is worth writing down before
somebody reaches for it at two hundred.

The grid holds `divisions^d` cells against `n` points. At `d = 1` or `2` that
bites: 500 points into 64 cells means a good sample has to revisit cells, and a
budget's worth of seeds cannot fake it. Past `d ≈ 3` the cell count runs away
from `n` and **every point lands in its own cell almost surely**, so occupancy
saturates at `n`. It fails by becoming too easy, not too hard. It still catches
total stalling — stuck chains jitter inside one bin whatever the dimension — and
stops catching *partial collapse*, where a sample confined to a curve through a
200-dimensional set scores a perfect `n`.

**This is the Latin hypercube's own weakness, arrived at from the other side.**
An LHS dodges the curse of dimensionality by stratifying marginals rather than
the joint — `d` histograms instead of `n^d` cells — and the identity permutation
on every axis gives points strung along the diagonal: perfect stratification,
useless coverage. The metric that scales is exactly the one a degenerate sample
passes.

What exists and works at 200 dimensions is in `cvg_benchmarks::assert_same_distribution`,
and it is three instruments rather than one: every coordinate marginal (catches a
frozen coordinate), KS on **random projections** from `unit_vector` (catches the
diagonal, which no marginal sees), and a radial profile against the reference
centroid (catches a shell or a clump). All three compare against a reference
sample, which is the catch — the regions rows C and E are about are exactly the
ones no reference can reach.

- [ ] **`Progress::remember` evicts FIFO, and the window has two jobs that want
      opposite policies.** It keeps the most recent `RECENT_POINTS = 1024`,
      dropping from the front. Its own doc says the job is "to be a fair sample
      of where the region has been seen", and FIFO gives a *recency* sample.

      **FIFO is not obviously wrong**, which is the interesting part. For a
      mixing chain, later points are better: burn-in means the tail is closer to
      stationary than the head, so preferring recent output is preferring
      post-burn-in samples. That is a real argument and it is why the window was
      built this way.

      It fails on the other job. The set also holds the *rare* points — a solver
      witness, a gap seed, a caller's hint — which are expensive, structurally
      different, and the only evidence that a second component exists. A
      thousand consecutive points from one chain evict every one of them. So the
      window is being asked for both "a fair sample of the region" and "one
      representative per component", and FIFO serves the first while destroying
      the second.

      Space-filling eviction serves both: when full, drop the point whose
      nearest neighbour is closest — the most redundant one — which keeps
      component representatives and still thins a well-mixed cluster. The
      primitive already exists as `walking::nearest_distance`.

      Exact maximin eviction is `O(n^2)` per insert, which at 1024 points and
      200 dimensions is too slow. Geoff's read is that it may be affordable in
      the case where it matters, since that case is low-dimensional; the cheap
      general version is a reservoir with a small random tournament — sample `k`
      candidates, evict the most redundant of those, `O(k)` per insert.

      `Driven by:` nothing yet, and it should stay that way until a case
      exercises eviction. **This causes no current failure**: chains start once
      and `cover_gaps` runs once, both inside `open()`, long before 1024 points
      accumulate. Building the reservoir now would be building an instrument
      with nothing to measure.

- [x] **Driving assumes the feasible set is a graph over the free coordinates.**
      `x_driven = f(x_free)` is a *function*, so where the set is not a graph
      one branch gets parametrised and the rest are unreachable. `x1 * x2 == 0`
      is the case: a cross of two arms of equal measure, driven as `x1 = 0/x2`,
      which describes the `x1 = 0` arm and cannot describe the other — at
      `x2 = 0` the variable `x1` is unconstrained, so no function of `x2` gives
      it. A chain on the second arm can only move `x2`, and moving it leaves the
      set. Measured 394 of 400 points on one arm, 7 on the other.

      **Fixed 2026-09-16, as branch selection by plan.** The matching admits
      several maximum matchings and each is a plan; `classify::plans` keeps up to
      eight (deduplicated by driven set — `drive` narrows against every constraint
      naming a coordinate, so two matchings with the same driven set are one
      plan). `classify::tightest` picks, per point, the plan whose driven
      coordinates the others pin hardest — a dry drive summing slice widths as
      box fractions. On the `x2 = 0` arm at `(0.7, 0)` the slice of `x2` given
      `x1` is `±t/0.7` and the slice of `x1` given `x2 = 0` is the whole box, so
      "drive `x2`" is that arm's own parametrisation; a chain under it moves `x1`
      and stays. At the crossing every plan ties and the walker spreads the
      chains seeded there across the tie, which is what this fixture needed: the
      opening's only point is the origin (the box centre is feasible), so all
      eight chains start on both arms at once. Now 19 and 22 of 40. `settle` and
      `centre` take the first tied plan; brute force draws a plan per candidate.
      With one plan every path is what it was, which the seeded fixtures check.

      The honest limit: this keeps a chain on the arm it was seeded on, and the
      seeds are the opening's — the bisection's coverage in low dimension, and
      nothing past a handful. The general mechanism is in Speculation below.

      Every point is feasible; coverage is what breaks, which makes it row D's
      branch problem in different clothes.

      **The reason it is worth fixing rather than noting** is that it is not a
      regression, and that is the trap. Before isolation this was `Opaque`, the
      walker missed a measure-zero set every chord, and sat on its seed —
      visibly broken at about five bins. Now one arm samples beautifully.
      *Isolation turns a visibly stuck sample into a confidently wrong one*, and
      an occupancy claim over `x2` alone passes it, because `x2` really does
      span its range. Only the arms together show it.

      The fix is a choice not to make from one example: refuse to drive where a
      divisor can vanish — conservative, and it gives up cases that are fine —
      or treat it as branch selection, the way row D is handled.

      Not to be confused with the zero divisor itself, which is benign. `x = c/a`
      at `a = 0` is a pole when `c != 0`, and `0*x == c` has no solution there,
      so the asymptote sits over infeasible ground; it is a hole when `c = 0`,
      and `0*x == 0` admits every `x`, so leaving the coordinate alone is right.
      `retract` skips a non-finite definition, which lands correctly both ways.
      `Driven by:` `cvg_equalities::both_arms_of_a_product_receive_points`

- [ ] **Components are weighted by chain count, not by measure.** Eight chains
      spread over two components give 50/50 whatever their relative volume. Both
      fixtures are symmetric — `abs` has `|f'| = 1` either side, the parabola
      `|2x + 1| = 3` at both roots — so 50/50 is right for them and **nothing in
      the corpus would catch it being wrong**.
      `Driven by:` nothing yet, and that is the first work item: a benchmark whose
      two components have deliberately unequal measure, with an oracle on the
      ratio. Building the weighting before that oracle exists is building blind.

### Which tests are expected to move

Not regressions — this is a semantic change and the churn is the point:

- **`cvg_pools::constants`** (A) — passes today only because `pi` and `e` are
  folded to literals and the band is samplable. Under A it should not be
  searched at all.
- **`cvg_pools::roots`, `power`, `signum`** (B) — driven; they should stop being
  solver questions entirely.
- **`cvg_pools::simple_arithmetic`** (C) — the rearrangement case, and
  the one most likely to expose a wrong matching.
- **`cvg_pools::absolute_value`** (D) — two branches; whatever picks a branch has
  to not bias which one.
- **`cvg_pools::dynamic_variable_lookup`** (E) — two equations over three variables, the
  smallest real matching problem in the corpus.
- **`cvg_benchmarks::parabolic_roots_*`** (D again, with distribution oracles
  attached) — these are the ones that will say whether the surface-then-band
  split preserves uniformity, and they are the reason not to guess.

## The plan: getting constraints in front of the solver

The measure that matters is `Document::untranslated` — the constraints the emitter cannot write
down, which the pool must then attack blind. Sorting the CVG corpus by *why* a constraint is
untranslatable is what ordered this list, and the answer was a surprise: **most of it is not a
theory problem.** Four of babel's functions are refused today for reasons that were true when the
comment was written and are not true now.

Ordered by cost-to-value, cheapest first. Each step shrinks the input to the step after it.

- [x] **1 — Write the encodings we already knew how to write. Done.**
      `floor`, `ceil` and `%` were refused with the note that they "want `to_int`, which leaves
      QF_NRA for QF_NIRA". True, and not a reason. All three are now in `prelude()` as
      `babel_floor`, `babel_ceil` and `babel_mod`, with a `babel_trunc` that exists only because
      `%` needs it.
      `(to_real (to_int x))` is floor with the right sign behaviour — `to_int` of -2.7 is -3, floor
      and not truncation — which is what lets `ceil` be `(- (to_real (to_int (- x))))`. Babel's `%`
      follows Java in taking the sign of the *dividend*, so it is `a - b*trunc(a/b)`; floored modulo
      would answer 2 to `-7 % 3` where babel answers -1. Both halves of that are pinned in
      `the_prelude_helpers_mean_what_they_say`, in the form of claims a wrong encoding would fail.
      `%` also pushes the divisor guard that `/` pushes, for the reason `/` does: `a % 0` is NaN and
      the pool bins NaN, but SMT-LIB leaves `/0` underspecified, so a solver may otherwise satisfy
      a constraint *through* the zero and hand back a point that is thrown away.

      **The one cost is the logic line.** `to_int` and `to_real` live in SMT-LIB's `Reals_Ints`
      theory, which `QF_NRA` does not include, and Z3 enforces this — measured, a `QF_NRA` document
      containing `to_int` is rejected outright. So `emit` now writes `QF_NIRA`. That is free: the
      same prelude and the same polynomial constraint solve identically under either name, and a
      clean re-run of the benchmark suite is unchanged (`top_corner_200d` 31.4s against 30.8s
      before, `parabolic_roots_narrow` 7.5s against 8.1s — a 20% spread seen on the first run was
      machine contention, and a test that never calls a solver moved with the rest, which is what
      gave it away).

      `cvg_pools::modulo` is green. The suite is 163/164, with only `cvg_benchmarks::p118` left.

      Four things came out of review, all of them worth more than the feature was:
      *`%` is a remainder, not a modulo*, and `BinaryOp::Mod` is now `BinaryOp::Rem` all the way
      through. The names had been lying: a remainder takes the dividend's sign (`-7 % 3` is -1), a
      modulo takes the divisor's (2). The grammar's `MOD` token and the `%` spelling stay, since
      both are the JVM's.
      *The document is a list of lines, not a string.* `Script` owns the separator and comments are
      their own variant. A missing `
` between two `(…)` forms is harmless — SMT-LIB does not care
      — but a missing one after a `;` **eats the next command**, and quietly: the parse guard only
      fires when *every* assertion is lost, so a document that loses one still looks fine and comes
      back `sat` for a question nobody asked. Same argument as `Sexp` and parentheses: make it
      unrepresentable rather than tested for.
      *The logic is configurable*, via `ConstraintSolver::with_logic`, defaulting from
      `SOJOURN_SMT_LOGIC`, defaulting to `QF_NIRA`. Explicit beats environment beats built-in, and
      `from_variable` is split out so the precedence is testable without mutating a process-global.
      *Model values are rationals, always.* Z3 answers `2.5` with `(/ 5.0 2.0)`, so `as_rational` is
      the ordinary path rather than a fast case. Where it fails — either half overrunning `i64`, or
      an algebraic irrational like `(root-obj (+ (^ x 2) (- 2)) 2)` — the fallback was wrong:
      `approx`'s argument is decimal *places*, not significant figures, so at `approx_f64`'s 17 a
      value of 1.01e-23 read back as a confident `0.0`. Now 330, which covers every magnitude an
      `f64` can hold.

      **Not** constant folding, which was listed here on a wrong premise. `pi` and `e` never reach
      the emitter as symbols: the lexer has `PI` and `EULERS_E` tokens, `literal` admits them, and
      `front_end::translate_literal` returns `std::f64::consts::PI` at parse time. That is why
      `cvg_pools::constants` is green today. What is left for a folding pass is constant-argument
      *calls* — `sin(2.3)` — which is its own item.

- [x] **2 — Constant folding, and two deletions it paid for. Done.**
      `rewrite::fold_constants` collapses every subtree made only of literals, bottom-up, using the
      evaluator's own `UnaryOp::apply` / `BinaryOp::apply` so a value cannot change — `corpus.rs`
      pins several constant expressions by value and is the guard on that.
      The payoff was not the folding. It was that **"is this constant?" stopped being a question
      anywhere else.** `ast::const_eval` — a recursive evaluator that existed to serve exactly one
      caller — is gone, and `static_range` is now "are both bounds `Kind::Literal`?", because after
      folding a statically known bound *is* one. Bottom-up folding needs no recursion of its own:
      by the time a node is reached its children are literals already.
      Placement is load-bearing in both directions. Before `rewrite_booleans`, because folding a
      wholly-constant comparison afterwards would collapse `3 < 3` onto the strictness epsilon and
      put a three-hundred-digit denormal in the document. Before `invert_monotone`, because
      inversion wants a literal to invert against. That leaves unrolled aggregate terms unfolded —
      an evaluator-throughput opportunity for the tape work, not a coverage one.
      Non-finite constants are **refused**, not folded: `ProblemKind::NonFiniteConstant` at the
      offending span. Leaves as well as folded results, since babel's `FLOAT` admits `1.0e400` and
      refusing `1.0e400 * 1.0` while accepting `1.0e400` would be a rule with a hole in it. One
      consequence worth knowing: `sum(0/0, …)` now reports the division rather than "an illegal
      lower bound", so `IllegalAggregateBound` at compile time is reached only by a *finite* bound
      that is not an index — `sum(1, 20/3, …)`, which now has its own test.
      Note `pi` and `e` were never part of this. They are lexer tokens that
      `front_end::translate_literal` turns into `std::f64::consts::PI` at parse time.
      **`just bench` cannot see this change and never could** — not one of the five benchmark
      expressions contains a constant subexpression, since every literal in them sits next to a
      variable. Numbers are unchanged within the run-to-run spread, which is currently about 15%
      on this machine (`trivial` came back 14270 and 16792 on consecutive runs), so treat any
      single reading below that threshold as noise. If folding is ever to be measured, the suite
      needs a case written for it.

- [x] **3 — Monotone inversion. Done, and it is the cheap half of causalization.**
      `rewrite::invert_monotone` turns `f(u) op c` into `u op' c'` for ten strictly monotone
      functions, computing the bound here in `f64`. `2 < ln(x1)` reaches Z3 as `x1 > e^2` and
      `20 > 2^x5` as `x5 < log2(20)` — both linear, and the solver is never asked about a logarithm
      it does not have. `u` is a whole subtree, so `ln(x1 + x2) > 2` inverts too.
      Three things had to be right, and two of them only showed up under test:

      *Range is a correctness requirement, not an optimisation.* `atan(x) > 2` is unsatisfiable —
      2 is outside `atan`'s range — and inverting through `tan` gives `x > -2.18`, which is almost
      always true. A constant outside the range is left alone. The `bound.is_finite()` check
      afterwards catches the open ends (`atanh(1.0)`) and plain overflow (`exp(710)`).

      *`ln(0)` is negative infinity, not NaN.* So zero satisfies **any** upper bound, and the
      domain guard on `ln`/`log10` has to be `u >= 0` rather than the textbook `u > 0`. Written the
      textbook way first; the round-trip test caught it at exactly one sampled point. A guard
      narrower than the constraint it replaced is the direction that turns a sound `unsat` into a
      wrong one, so this mattered. **Match the evaluator, not the mathematics.**

      *`and` is `max`.* An upper bound does not carry the domain with it — a lower one does, since
      `f`-inverse lands in the domain by definition — so `ln(x) < 2` emits
      `max(x - e^2 + eps, -x + eps) <= 0`. No `Kind::And` was needed; two residuals hold together
      exactly when the larger is `<= 0`, which is how `NearEq` has always lowered. `residual` is
      now factored out of `rewrite_expr` and shared between the two.

      The bound is nudged one ulp outward — `next_down` for a lower, `next_up` for an upper — so
      the region asserted is never *narrower* than the one described. `fl(e^2)` is not `e^2`, and
      on the narrow side `unsat` stops implying anything. The test asserts that one-sidedness
      directly rather than asserting agreement, which is what the design actually claims.

      Unplanned benefit: this makes the wave-2 restriction affordable. Restricting `a ^ b` to an
      integer `b` would make `2^x5` a compile error, and inversion rewrites it away first.

## Wave 2 — nothing non-finite travels, and powers are multiplication

- [x] **Non-finite is an error now, at both phases.** One rule:
      `ProblemKind::NonFiniteConstant` from `fold_constants` for what is provable
      at compile time, `ProblemKind::NonFiniteValue` from `eval_expr` for the rest.
      The runtime check is on **every node** rather than only the operations that can
      produce a non-finite value. The producing set is nearly everything once infinities
      are in play — `0/0`, `inf - inf`, `0 * inf`, `sqrt` of a negative — so checking
      selectively would save little, and checking uniformly catches two things it would
      miss: a non-finite **input**, which now fails at its own `Kind::Global` instead of
      travelling to wherever it first matters, and an **unwritten frame slot**, since
      `evaluate` fills the frame with NaN as a sentinel and reading one is a
      slot-allocation bug.
      **Infinities are in scope, and that was the whole point.** A NaN-only rule would
      have left the trap that prompted this: `ln(0)` is `-inf`, not NaN. See below.
      *Eager per lane, per instruction.* The tape fuses the finite test into every
      instruction's loop and records each lane's first fault, so it is as exact as
      the walker was and costs nothing on the happy path. Only a GPU sieve gets to
      be coarse — check the output buffer, re-run an offending column through the
      CPU tape for the span — and that split is in `src/README.md` so nobody
      "fixes" a kernel to match. The policy itself is the `is_finite` test on every
      checked instruction in `eval/tile.rs` and `eval/lane.rs`.
      Blast radius was smaller than expected. **The pool needed no change at all** —
      `.ok().is_some_and(|residual| residual <= 0.0)` always treated an `Err` and a NaN
      alike. Both `runtime_errors` bound tests moved from `IllegalAggregateBound` to
      `NonFiniteValue`, which points at `20/x1` rather than at "the upper bound";
      `IllegalAggregateBound` now fires only for a *finite* non-index, and
      `a_finite_bound_that_is_not_an_index_still_reports_as_one` exists so the variant
      does not quietly die.

- [x] **The domain guards became the textbook ones.** This is what including infinities
      bought. `rewrite::monotone` gave `ln` and `log10` a floor of `u >= 0` because
      `ln(0)` was `-inf` and so zero satisfied any upper bound — a workaround for the
      evaluator, not a fact about logarithms. Refusing the infinity made it `u > 0`.
      `sqrt` keeps the inclusive floor, and the asymmetry is now principled: `sqrt(0)`
      is `0.0`, a finite answer, where `ln(0)` is not an answer at all.
      The round-trip test needed tightening rather than relaxing. It skipped a sample
      when either side failed to evaluate, which was right when failure meant NaN; the
      un-inverted form now errors at exactly the points the guard exists to exclude, so
      silently skipping them would have gutted it. It asserts instead that **when the
      un-inverted form errors, the inverted form rejects.**

- [x] **`rewrite::expand_powers`.** `x ^ n` for a constant whole `n`, `|n| <= 64`,
      becomes a `Kind::Fold` of multiplications — `n == 0` a literal `1.0`, a negative
      `n` a reciprocal. Runs *after* unrolling, which is the only reason a loop index can
      be an exponent at all: `sum(1, 3, i -> x^i)` arrives as `x^1, x^2, x^3` only once
      `substitute` has put the literals there.
      **The first argument is consistency, not speed.** `emit::power` already expanded
      constant integer exponents, so the solver reasoned about `(* x x x)` while the pool
      filtered with `powf` — two functions that disagree in the last place. Doing it once,
      in the AST, leaves both reading the same expression.
      The trade is that results move in the last ulp for `n >= 3`: `2.3^3` is
      `12.166999999999998` through `powf` and `12.166999999999996` as `x*x*x`, about one
      case in five. `n == 2` agrees everywhere, libm having special-cased squaring, and
      **`corpus.rs` did not move** — every `^` in it is `^2`, the exact `3 ^ 4`, or
      `sin(21)^3`, which agrees.
      `emit::power` is deleted, `BinaryOp::Pow` is unconditionally untranslated, and a
      latent bug went with it: `power` rendered a negative exponent as `(/ 1.0 (* x x))`
      **without a divisor guard**, so a solver could satisfy `x^-2 < 1` through `x = 0`.
      As an ordinary `Kind::Binary { Div }` it picks the guard up automatically, and
      `a_negative_power_pins_its_base_away_from_zero` pins it.
      **Retired.** The pass cost a compound base `n` evaluations and left a product fold
      that nothing could narrow, which is what stranded repair on `x^2 + y^2 < 1`. Each
      backend lowers a whole exponent itself now, keyed on `Expr::whole_exponent`; the
      guard test stays and the emitter still writes `(* x x x)`.

- [x] **The benchmark measured one thing, and taught a lesson about itself.**
      The first `just bench` after wave 2 read +32% on `small (jvm)` and +35% on
      the 200-variable sum, and that was written up as fact. It was one run.
      Seven runs say otherwise:

      | expression | baseline | median of 7 | worst run | verdict |
      |---|---|---|---|---|
      | `x1 + x2` | 18584 | 19583 | -8% | drift |
      | `x1 + x2 > 20 - x3^2` | 9164 | 11686 (+28%) | +1% | real, magnitude uncertain |
      | `sin(x1)*cos(x2)+sqrt(abs(x3))` | 9360 | 10156 (+9%) | -3% | drift — no `^` in it |
      | deep arithmetic | 3546 | 3523 | -14% | drift |
      | `sum(1, 200, i -> var[i]^2 - 3.0)` | 77.6 | **125.5 (+62%)** | **+32%** | **real** |

      **`transcendental` contains no `^` and still moved +9%**, which is the
      tell: that much of every figure is machine drift, and runs later in a
      sequence come out faster across the board. Subtract it and only the
      200-variable case is unambiguous — every single run beat the baseline by
      at least 32%, and the median by 62%.

      Why that case and not `small (jvm)`, when both use `^2`: two hundred
      squarings per evaluation against one. `powf(x, 2.0)` is cheap enough that
      a single call disappears into the noise, and two hundred do not.

      The real spread is **about 30%, not the 15% previously recorded**, with a
      systematic warm-up drift on top. A single reading of this suite is not
      evidence. That is now in `performance-records/README.md`, where someone
      reading a row will see it.


## Next

- [x] **SMT-LIB2 emitter.** Done, in `cvg/emit.rs`: a pure
      `(&[InputVariable], &[Expression]) -> Document`, twelve tests, no solver involved. `Fold` maps
      straight onto `(+ a b c …)` as hoped. Two things worth knowing came out of writing it:
      *Strictness does not survive the trip.* The boolean rewrite encodes `<` as `+ f64::MIN_POSITIVE`,
      which works because f64 rounding swallows it at any real magnitude. Real arithmetic does not
      round, so emitting it literally puts a 310-digit denormal in the document *and* means the
      wrong thing. The emitter recognises the marker and asserts `(< r 0.0)` instead.
      *`Global` and `var[i]` index different lists.* `Global` indexes the expression's own symbols;
      `var[i]` indexes the schema. Using one for the other mis-resolves silently, and did, until
      the unrolled-aggregate test caught it.
      What it cannot express — `sqrt`, `cbrt`, `mod`, `floor`, `ceil`, the transcendentals, a
      non-integer exponent — is **reported** through `Document::untranslated`, never dropped. Five
      of the seven `cvg_pools` reds are already expressible; `roots` needs `sqrt`/`cbrt` and
      `modulo` needs `mod`, and both are dialect questions rather than theory ones.

- [x] **A backend: Z3, `bundled`, unconditional.** Done. Builds on Windows in about four minutes
      cold and links statically, so there is nothing to ship to a customer machine. The three
      hazards the probe warned about all landed as predicted, and all three are handled:
      *`Solver::from_string` cannot report a syntax error* — it returns `()`, and a malformed
      document leaves an empty solver that answers `sat` instantly. `Z3Backend::solve` refuses to
      believe any verdict unless `get_assertions()` came back non-empty. This is the load-bearing
      line in the backend; without it an emitter regression reads as success.
      *Algebraic irrationals have no rational form*, and `sqrt` is exactly where Z3 produces one.
      `Real::approx_f64()` is the fallback, used only when `as_rational()` declines.
      *`define-fun` helpers appear in the model* with arity > 0, and `apply(&[])` on one panics
      inside the binding rather than erroring. Skipped by arity.
      Unsat cores work through `from_string`, which was not a given — a contradictory pair comes
      back naming *both* constraints. `Solution::Unsatisfiable::blamed` changed from
      `Option<Expression>` to `Vec<Expression>` to say so honestly: a contradiction is a
      relationship, and `x > 8` is perfectly satisfiable until `x < 2` turns up.

- [x] **The emitter cannot produce an unbalanced document.** *(Retired 2026-09-05: `Sexp` and its
      macros were replaced by askama templates, see that entry; this stays as the record of why
      balance mattered and what the hazards were.)* Terms were built as `Sexp` in
      `cvg/sexp.rs` and rendered by its `Display`, so parentheses come from structure rather than
      from format strings. That class of bug is not caught, it is unrepresentable — which matters
      because `Solver::from_string` reports a syntax error by silently accepting nothing and then
      answering `sat`. A stray paren used to read as "solved it".
      `sexp!` and `define_fun!` sit on top so the static fragments are written as lisp rather than
      as ninety lines of nested constructors; the prelude is six. They expand to `Sexp` constructor
      calls rather than to strings, so the shorthand does not bypass the guarantee — and balance
      ends up enforced twice, since Rust's tokenizer rejects an unbalanced macro argument before
      any of this code runs.
      Two Rust-isms shaped the syntax, recorded in the module docs: `define-fun` tokenizes as
      `define`, `-`, `fun`, so the keyword lives in the macro *name* rather than its argument; and
      `|x|` is a quoted symbol in SMT-LIB but two bitwise-ors in Rust, so quoted symbols come only
      from `Sexp::symbol` — fine, since they only ever hold run-time names. Raw `stringify!` was
      the alternative and is worse: it eats the space in `(ite(< x 0.0) ...)` and yields a string
      rather than a term.

- [x] **The prelude's *meaning* is pinned, not just its syntax.** It is a hand-written table and
      nothing else covered it: swapping `babel_min`'s `<=` for `>=` still parses, still solves, and
      every other test still passes, because no corpus case drives `min` through a solver.
      `the_prelude_helpers_mean_what_they_say` puts seventeen closed claims to Z3 —
      `(= (babel_min 2.0 5.0) 2.0)` must be sat, `(= (babel_max 2.0 5.0) 2.0)` must be unsat, and
      `babel_sgn(0)` must be `0` because babel follows Java there while Rust's `f64::signum` does
      not. Confirmed it fails by actually making that swap.

- [x] **Z3 validates every document the emitter can build.** A test walks 27 expression shapes —
      every translatable unary and binary, folds, blocks, `var[i]`, powers including negative and
      zero exponents, nested roots, a Unicode name — emits each and requires Z3 to accept it. Worth
      more than a parenthesis counter, since Z3 checks sorts, arities and scoping too.
      It also compares assertions *written* against assertions Z3 *took*, and pins the fact that
      makes one guard sufficient: **a parse error loses the entire document, not just the tail.**
      If Z3 ever became more forgiving, that test fails and the guard would need to start counting.

- [x] **The two emitter fixes.** Divisor guards — every `(/ a b)` now also asserts `b != 0`, without
      which Z3 satisfied `simple_arithmetic` by setting everything to zero and reading `0/0` as
      whatever suited. And auxiliary variables, so `sqrt` becomes `y >= 0 and y*y = x` and `cbrt`
      becomes `y*y*y = x` — which also gets the domain right for free, since a negative `sqrt` is
      then unsatisfiable, matching babel's NaN. That unlocked `roots`.
      Assertions are now named `(! ... :named c0)` so a core points back at a constraint, and
      auxiliary names carry their constraint index because two constraints both declaring `aux0` is
      a redeclaration that kills the whole document.

      **Z3 works, and a probe against a real emitted document turned up three things.**
      `z3 = { features = ["bundled"] }` builds on Windows and links statically — 4m38s cold, no DLL
      to ship. (An earlier attempt failed with CMake `FileTracker FTK1011`, which was MAX_PATH from
      a deep scratch directory, not Z3.) Feeding it `emit`'s actual output for `simple_arithmetic`:

      - [ ] **The emitter must guard divisors.** Z3 returned `x1=x2=x3=x4=0` as a model — satisfying
            the constraint via `x3/x4` = `0/0`, which SMT-LIB leaves *underspecified* so the solver
            may pick any value for it. Babel evaluates that point to NaN and rejects it, so the
            solver's answer would be silently discarded and the search would spin. Adding
            `(assert (not (= |x4| 0.0)))` produced a genuine point: `x4 = 1/2`, everything else zero.
            Every `(/ a b)` needs a side condition, which means the translator has to contribute
            assertions as well as a term.
      - [ ] **`Solver::from_string` swallows parse errors.** A deliberately malformed document
            returned **Sat** with an empty model rather than an error — the signature returns `()`.
            So an emitter bug would present as "solved it instantly". The backend has to verify the
            declared constants actually came back, or check Z3's error code, before believing a
            verdict.
      - **The auxiliary-variable encoding works.** `sqrt` as a fresh `aux0` with `aux0 >= 0` and
            `aux0*aux0 = x` solved fine, so `roots` is reachable once the translator can emit
            declarations alongside terms — the same machinery the divisor guards need.

      **UNSAT-as-diagnostic already works.** A contradictory pair (`x > 8` and `x < 2`) came back
      `Unsat` from the same path, with no pool and no model parsing involved.

      **cvc5 is ruled out on Windows.** `cvc5-sys` 0.4.0's build script is explicit:
      `#[cfg(not(unix))] #[cfg(feature = "static")] fn ensure_cvc5_built_and_install() { panic!(
      "This rust binding for cvc5 is only supported on Unix systems!") }`. The non-`static` path
      does not save it either — it probes for headers by invoking the C compiler with `-E -M -xc -`,
      which are GCC/Clang flags MSVC does not accept, and it wants a cvc5 already installed with
      `CVC5_LIB_DIR` pointing at it. So the in-process story becomes a build-time native-library
      requirement for anyone compiling babel, which is worse than the deployment problem it was
      chosen to avoid. It remains the best fit on Linux and macOS.

      Worth picking the theory before the solver: babel has `sin`/`cos`/`log`/`sqrt`, so that is
      QF_NRA with transcendentals — dReal's domain — rather than QF_FP, which is where Z3
      bit-blasts and falls over. `smt::DRealBackend` is stubbed with that reasoning recorded; the
      catch is that dReal has no Rust crate, so it is subprocess-only, which is the thing you did
      not want to ship to a customer machine. Z3 via `z3` + `bundled` links in-process and parses
      SMT-LIB2 through `Solver::from_string`, so text emission and in-process execution are
      orthogonal choices.
      Decided on the literal question: shortest round-trip decimals, not exact rationals. Exact
      literals would close one gap and leave the larger one open, since SMT reasons in real
      arithmetic while babel evaluates in `f64` and *every operation* rounds — `(* x x x)` need not
      equal `x.powf(3.0)` in the last place. The model is a real-arithmetic idealisation on purpose,
      and solver output is filtered through babel's own `evaluate` regardless. Swapping in exact
      dyadic rationals is a one-function change if that ever proves wrong.
      Considered and rejected: reading the literal's *source text* back through its span. Tempting,
      since SMT-LIB reals are exact decimals and a user's `0.1` maps in losslessly — but babel
      evaluates the `f64`, and a solver's point is filtered through babel's own `evaluate` before
      anyone sees it, so source text would have SMT modelling a program babel does not run. It is
      also a no-op in practice: for every literal in the corpus the shortest round-trip repr is
      character-identical to what was typed. And it covers neither input bounds (which arrive from
      the caller as `f64` with no source at all) nor `pi`/`e` (whose source text is not a numeral).
      The one place the argument reverses is the UNSAT diagnostic, where "what the user meant" is
      arguably the right semantics.
      Considered and rejected: reading the literal's *source text* back through its span. Tempting,
      since SMT-LIB reals are exact decimals and a user's `0.1` maps in losslessly — but babel
      evaluates the `f64`, and a solver's point is filtered through babel's own `evaluate` before
      anyone sees it, so source text would have SMT modelling a program babel does not run. It is
      also a no-op in practice: for every literal in the corpus the shortest round-trip repr is
      character-identical to what was typed. And it covers neither input bounds (which arrive from
      the caller as `f64` with no source at all) nor `pi`/`e` (whose source text is not a numeral).
      The one place the argument reverses is the UNSAT diagnostic, where "what the user meant" is
      arguably the right semantics.

- [x] **One entry point.** `solve`, `solve_with_rng` and `solve_with` are collapsed into
      `ConstraintSolver`, which holds the three injected dependencies — rng, known-feasible points,
      strategy list — as fields with defaults. Construction is infallible, because nothing it holds
      can be invalid alone; the one thing that can be (a constraint naming a variable the box does
      not declare) needs the problem and is still checked in `solve`. `Strategy`,
      `with_strategies` and `with_rng` stay `#[doc(hidden)]`: which strategy runs is the module's
      decision, and `Route` makes most of it at runtime from a probe rather than from
      configuration. (Later, 2026-09-16: the rng left the builder again — it was never a
      setting with a default but the caller's stream, so it is a parameter of `solve`; see
      "The rng at the API". `with_rng` is gone.)
      Tests drive it with `#[pollster::test]` — a dev-dependency, so nothing propagates to
      consumers. Chosen over `#[tokio::test]` because the body is synchronous and a reactor buys
      nothing, and because the crate's own tests then stand as proof that no runtime is required.
      One line to swap if that turns out to be the wrong call.

- [x] **`solve` is `async` and now behaves like it.** The search runs on a worker thread; `solve`
      awaits a `futures_channel::oneshot` carrying the opening verdict, so the future genuinely
      completes off-thread and a caller-side `timeout` has something to race. Cancellation is an
      `AtomicBool` the worker reads between batches — the cooperative token that was always the
      actual requirement, since no signature can cancel CPU-bound work that does not agree to stop.
      Since step 4 of the brute squad (2026-09-03) dropping the `solve` future is itself a
      cancellation: the worker polls `oneshot::Sender::is_canceled` between brute-force batches
      and stops. Since 2026-09-11 a solver call in progress is interrupted too: Z3 runs on its
      own thread (`smt::Z3Backend::solve`), the producer waits on it polling the cancellation,
      and sends `Z3_interrupt` when the future is dropped or a wall-clock ceiling passes. Z3
      usually agrees to stop; when it does not (below) the thread is abandoned with an
      error-level `tracing` line and the search carries on without it.

- [x] **Gap coverage is budgeted in Z3's own units; the stepped beam census returns.**
      *2026-09-12.* Artemis's `e06-stepped-beam-20` (`tests/regression_fixture.rs`): 40
      variables, 41 constraints, one a `sum` over every segment, and `solve` ran 3.7 CPU-hours
      without returning. Not a Z3 hole. Sampling had the region in under a second; what did not
      return was `cover_gaps`, which on the satisfied path asked up to sixteen gap queries
      before `solve` returned, each with the whole solver limit, an `unknown` merely halving
      the reach and asking again. Measured: one plain query on the n = 10 document is `sat` in
      15 s having spent 295k rlimit units; n = 20 `sat` in 47 s; an n = 20 gap query spends
      the full 3,000,000 and answers `unknown` at 115 s. Sixteen of those, leashed at 480 s
      each, is the hours. Now the backend reports what a call spent (`smt::Reply::spent`, from
      `rlimit count` in the solver's statistics, reported after an `unknown` too), the stage
      treats the limit as its *whole* budget, hands each query the remainder, and stops at
      zero or at the first `unknown` — a weaker exclusion is not a cheaper question — with a
      `warn` that coverage was cut short. n = 20 now opens in 109 s and delivers 256 points at
      137 s; n = 10, where the fixture is pinned because it fits the standard test bound, in
      about 35 s; n = 5 is unchanged. Two things learned on the way. **A unit is not a number of
      microseconds:** eight on the problems the leash ceiling was calibrated on, about forty on
      the beam, which is why the ceiling is only ever the last resort against a hung process
      and budgets are kept in units — the same work on any machine, and deterministic. **A
      smaller solver limit is not a shortcut here:** below the ~1.2M units the seed query
      needs, the opening falls through to the brute squad's billion proposals, which on a
      system with a 20-term `sum` per candidate is longer than the query it replaced. The
      brute-force budget is a count of proposals, not of work, and that mismatch is its own
      entry waiting to be written.

- [x] **A local solve finds the seed; Z3 is reached only to prove there is none.**
      *2026-09-13.* `Strategy::LocalSolve`, `src/cvg/local.rs`: COBYLA (`basin`, pure Rust,
      MIT/Apache-2.0, caret-pinned at 1.11) over the unit cube, every constraint in babel's own
      `<= 0` form plus the box as `2d` more rows, from the box centre and three seeded starts,
      each capped at `8 (d + 1)` evaluations. Every evaluation is judged by the oracle and the
      run stops at the first that passes — on the 100-segment stepped beam that is evaluation
      24, inside COBYLA's initial simplex, 150 ms; on two hundred strict bounds, 189 ms. Two
      things learned. **The objective is the sum of the residuals, not the worst:** on the
      bounds started at the centre every residual is `+2e-308` and the maximum is flat across
      the whole simplex, so COBYLA converges three hundred iterations later on a point the
      oracle rejects; the sum has a slope on every coordinate and the same start lands in the
      simplex. **Do not let COBYLA converge:** its model algebra grows with `d² m`, 0.3 s an
      iteration at 200 variables, and driving inward from the first feasible point cost 1,600
      evaluations at 20 segments and would cost hours at 100; the walker's burn-in moves off
      the walls far more cheaply. Found on the way and fixed: dropping a `FeasibleRegion` waited
      out the walker's burn-in, 95 s at 200 dimensions, because burn-in never read the stop
      flag; it does now, per step. Acceptance is `the_100_segment_beam_opens_by_local_solve`
      and the 30-segment census in `tests/regression_fixture.rs`, both with the solver off the
      ladder, because with it on the opening still spends one budgeted gap query on the beam's
      document after the seed — which is the next entry's question. Left as measured, not
      fixed: the walker's burn-in at 200 dimensions on the beam is 95 s and 256 points 335 s,
      the deflection `sum` evaluated every step.

- [x] **Coverage after a seed the probe did not find.** *Settled with Z3's fate, 2026-09-14:*
      coverage is a bisection of the contracted box (`prune::bisect`, below), spent only when
      the walker will carry the search or nothing is in hand, and skipped when the probe found
      points by uniform sampling, since `k` uniform hits already miss a component of measure
      `f` with probability only `(1 - f)^k`. Its budget is a count of contractions
      (`DEFAULT_PRUNE_BUDGET`, 256), so on a region in one piece it is bought for nothing and
      bounded: 13 s for the ten-segment beam's census under debug where the Z3 stage was 85 s.
      Moving it off the opening's critical path is still the concurrent-coverage entry.

- [x] **Z3's fate: deleted; interval branch-and-prune proves, blames and covers.**
      *2026-09-14.* `src/cvg/prune.rs` replaces `smt.rs`, `smtlib.rs`, `templates/smt2/`, the
      leash, `SmtLogic`, `with_solver_limit` and the `z3`/`z3-sys` dependencies with their C++
      build. **Contraction** is HC4 propagation over `interval::narrow` — every constraint,
      every symbol it names, intersected into the declared box and requeued on any coordinate
      that shrank by more than one per cent of its declared width, capped at 32 visits per
      constraint — and a coordinate that empties is the proof. **Blame** is the trace: each
      box carries, per coordinate, the constraints that narrowed it, and the blame for an
      emptied coordinate is the culprit plus that trace closed backwards over the coordinates
      the culprits read. No deletion filter, no re-run: on the typo fixture it names exactly
      `x1 < 3`, `x2 > 5`, `x1 > x2` and not `x1 + x2 < 20`, which narrowed nothing — the bar
      the previous entry set. **Bisection** splits a surviving box on its widest relative
      coordinate, contracts both halves, and keeps going breadth-first for a budget of
      contractions; every box dying is a proof the contraction alone could not see (the disc
      that cannot meet its ring, two contractions), and the leaves that survive are the pieces
      the walker must be started in. A leaf whose centre — driven coordinates put on their
      surfaces by `classify::centre` — is judged feasible is settled and is a seed for free; an
      unsettled leaf gets a one-start local solve inside its own box only when nothing at all
      is in hand. `interval::difference` gained the forward check, so a constraint whose
      enclosure misses its target on a box empties it with no inverse needed:
      `x^1.234 > 1000000` on `[0, 10]` is now proved rather than brute-forced.

      **What is not proved, and stays `NotFound` forever**, pinned in `tests/cvg_pools.rs`:
      a *thin* contradiction, `x + y <= 1` against `x + y >= 1 + 1e-9`, which bisection
      reaches only at a width of `1e-9`; and an *algebraic* one, `x*x - 2*x*y + y*y < 0`,
      whose enclosure holds negatives on every box because the square is never seen as a
      square. Also `x % 3 >= 2` against `x % 3 <= 1`: `%` across its discontinuities encloses
      everything, so the never-deliver fixture now reports `NotFound` where Z3 proved it. Z3
      proved some of these and spun on others; a decision procedure over polynomials (nlsat,
      CAD) is what the algebraic class needs, and a user story that needs it is the new fact
      that would justify one. `NotFound { unexpressed }` now names the constraints that
      narrowed nothing over the whole opening — a computed subscript is the permanent case —
      and is empty when every constraint said something short of a proof.

      **Splitting is a low-dimensional tool.** At 4096 contractions the ten-segment beam
      (twenty variables, one piece, walker-carried) spent forty seconds of an unoptimised
      build bisecting for nothing, since twelve splits isolate nothing at twenty dimensions;
      at 256 it is a couple. The old budget's first draft also spent sixteen COBYLA solves in
      leaves that were the whole box on 188 of 200 coordinates — minutes each — which is why
      leaf solves are for the nothing-in-hand case only. Timings against HEAD on the same
      loaded machine: beam-10 54 s → 13 s, beam-30 92 s → 44 s, the benchmarks unchanged.

      **Follow-ups not done here:** the contracted root could be handed to the sampler as a
      tighter proposal box (a plain contradiction already ends before a proposal; a half-empty
      box would double a probe's hit rate); an inner-box test (every constraint's enclosure
      inside its target) would settle fat boxes without a centre judgement; and the classic
      objection to HC4 — it cannot see a shared subexpression, which is why `%` above is not
      proved — is what a proper CSP (or a symbolic square) would address, if ever needed.

- [x] **Z3: prove it earns its build, or delete it.** *Deleted; the entry above is the
      answer.* Since the local solve, Z3 finds nothing
      the search could not find without it; its remaining role is `Infeasibility::Proved`. The
      test to write: a system interval analysis cannot prove empty — propagation over the box
      never contracts a domain to nothing, as on `x*y == 1 ∧ x + y == 1` — where a proof and an
      unsat core are what a user needs. If that test cannot be written from a real problem, the
      honest outcome is deleting `smt.rs`, `smtlib.rs`, the leash, the `z3`/`z3-sys`
      dependencies and their bundled build, and answering `NotFound` where `Proved` was; a
      propagation-based emptiness check with blame covers the contradictions users actually
      write (`x > 8 ∧ x < 2`, a bound an equality cannot meet).
      *2026-09-13: the tests that reach `Proved` today are three, all in `tests/cvg_pools.rs`:
      `contradictory_constraints_are_reported_as_unsatisfiable` (`x > 8 ∧ x < 2`),
      `a_pool_that_can_never_deliver_reports_exhausted_rather_than_blocking` (`x % 3 >= 2 ∧
      x % 3 <= 1`, which accepts `Proved` or `Exhausted`), and the new
      `a_backwards_comparison_is_blamed_together_with_what_it_contradicts`: the user's typo,
      `x1 < 3`, `x2 > 5`, `x1 + x2 < 20`, `x1 > x2`, where Z3's core blames exactly the three
      that conflict and leaves the innocent fourth out. That last one is the bar: whatever
      replaces Z3 has to return that set, not just "empty". All three are linear, so
      propagation with blame would clear the bar; the open question is only whether a real
      problem exists where it would not.*

- [x] **`SmtBackend` no longer demands `Send + Sync`.** It carried those while it was a stub, on
      the assumption the pool would share a `Box<dyn SmtBackend>` across threads. It does not — the
      backend is built where it is used, on the worker thread, and no call site is dynamic. The
      bounds would also have ruled out a reasonable implementation, since Z3's context is
      thread-local and anything caching one could never be `Sync`.

## Diagnostics

- [x] **Fold a bind failure into `CompilationFailure` as span-located problems.** Done
      2026-09-16. `CompileError` and `BindError` are gone; `compile` answers
      `CompilationFailure` for both halves, and an unbound name is `ProblemKind::Unbound` at
      its first reference — `Ast::reference_spans` reads the spans off the tree's `Global`
      nodes, which had them all along, rather than keeping a second list beside `symbols`.
      `SystemError::Unbound` carries the same failure. `Driven by:` `compile_errors::
      an_unbound_name_gets_a_caret_at_its_first_reference`, `every_unbound_name_is_reported`,
      and `system::tests::an_unbound_name_in_a_constraint_points_at_it`.

## Coverage

- [ ] **Get code coverage running.** `cargo-llvm-cov` is the mainstream choice: LLVM source-based
      instrumentation, integrates with nextest (`cargo llvm-cov nextest`), and emits lcov, HTML and
      cobertura. lcov is what Coveralls, Codecov and IDE importers all read, so one command feeds
      both CI and the editor.

      Three things to know before chasing a number:

      **Exclude the generated parser first.** `babel_parser.rs` is ~2,760 lines in `OUT_DIR`.
      Unless it is filtered out (`--ignore-filename-regex`) it dominates the report and the
      percentage means nothing.

      **100% statement coverage is impossible**, and not for want of tests. The `unreachable!()`
      arms exist precisely because translation rules those states out — they cannot be reached by
      any input, by construction. Either accept the shortfall, restructure so the impossible states
      are not representable, or mark them (`#[coverage(off)]`, nightly).

      **Path coverage is not really on offer.** Rust gives good line and region coverage; branch
      coverage has been the weak spot for years. Region coverage is the closest useful thing — it
      counts distinct executed regions rather than whole lines, so it does catch a `?` that never
      fired.

- [ ] Wire it into `.github/workflows/rust.yml` and publish to Coveralls, once the number means
      something.

## Performance

- [x] **There is a benchmark now**, on both sides, and the numbers are in
      `crates/babel/tests/throughput_benchmarks.rs` (`just bench`, release) and
      `src/test/kotlin/.../ThroughputBenchmarks.kt`. Points per millisecond:

      | expression | vars | Rust bound | Rust naive | JVM map | bound vs JVM |
      |---|---|---|---|---|---|
      | `x1 + x2` | 2 | 18584 | 4680 | 9700 | 1.9x |
      | `x1 + x2 > 20 - x3^2` | 3 | 9164 | 2843 | 4198 | 2.2x |
      | `sin(x1)*cos(x2)+sqrt(abs(x3))` | 3 | 9360 | 2935 | 4388 | 2.1x |
      | deep arithmetic | 4 | 3546 | 1792 | 1244 | 2.9x |
      | `sum(1, 200, i -> var[i]^2 - 3.0)` | 200 | 77.6 | 46.7 | **4.5** | **17x** |

      Rust release, JVM 11 (the toolchain the Gradle build pins, despite `JAVA_HOME` being 21).
      *bound* is `Bound::evaluate(&[f64])`, bound once; *naive* is
      `Expression::evaluate(&[(&str, f64)])`; *map* is `BabelExpression.evaluate(Map)`. Every
      harness gets pre-built inputs, so none of them is timing allocation.

      The old `~10k evals/sec` figure was worse than recorded: `PerformanceFixture` builds its `Map`
      inside the timed region, calls `print(".")` inside it as well, and warms up for fifty
      iterations where tiered HotSpot wants ten thousand before C2 engages. Left in place as the
      record of what that number meant.

- [x] **Flatten the AST to a tape**, batch loop innermost, and *measure before reaching for
      SIMD*. Done; see step 1 of `brute-squad.md`. The tree-walk evaluator was not kept
      as the differential oracle after all: the tests are the spec, and the walker is gone.

## Semantic analysis

Babel is small but not so small that it has no semantics to check. These are *translation-time*
errors — knowable without running the expression — and they are the reason `SemanticTranslator`
keeps a fallible signature.

- [x] **Statically illegal subscripts.** `var[0]` and `var[-1]` are wrong for *every* schema, since
      indices are one-based — no need to wait for a row. Same for a non-integral literal subscript.
      Done 2026-09-17: `fold_constants` refuses a literal subscript of zero
      (`ProblemKind::ZeroIndex`, "var[0] is not the first parameter (did you mean var[1]?)"),
      negative (`NegativeDynamicIndex`) or not a whole number (`DynamicIndexNotAnInteger`)
      at the subscript's span, after folding, so `var[2 - 2]` is caught too. Which parameters
      *exist* still needs a schema and stays with `resolve_subscripts`.
      Later the same day the rule went onto the *shape*: `rewrite::check_subscripts` refuses a
      subscript the row decides unless `ast::is_integral` says it is a whole number by
      construction — `floor`/`ceil`/`sgn` of anything, an aggregate's parameter, a `var` bound to
      one of these, and `+ - * % max min abs` negation and whole powers over them, every one
      exact in `f64` below 2^53 — with "wrap it in floor() or ceil() to say which" in the
      message. The runtime `NotAnInteger` fault is unreachable and gone; a gather's one fault is
      "past the end". `substitute` propagates the fold's verdict now, so `sum(0, 2, i -> var[i])`
      is `ZeroIndex` at parse rather than the row's. `Driven by:`
      `compile_errors::a_subscript_the_row_decides_must_say_how_it_rounds`,
      `a_subscript_that_is_whole_by_construction_compiles`, `every_non_integral_subscript_is_reported`,
      `an_aggregate_that_unrolls_onto_zero_is_refused`, and `ast::tests` for the table's rows.
- [ ] Revisit whether anything else deserves rejecting rather than evaluating: division by a
      literal zero, a lambda whose parameter is unused, a bound range that is statically empty.
      Kotlin allowed all three, and the first is load-bearing — `0/0` producing NaN is how the
      illegal-bound check triggers.

## Cleanups

- [ ] **Canonicalise `+`/`*` into `Kind::Fold`.** Today `Fold` only arises from unrolling, so `a+b`
      has two possible shapes. Canonicalising would mean `BinaryOp` loses `Add` and `Mul` — a swap
      rather than an addition, so no consumer grows a case. Worth doing when SMT emission lands and
      the n-ary form is being consumed anyway.
- [x] **Traversal helper (`preorder`/`postorder`).** Done 2026-09-17 as `Expr::iter_preorder`
      over one exhaustive `Expr::children`. Nine read-only walks became one-line queries —
      `classify::globals`/`mentions`/`occurrences`, `Ast::reference_spans`,
      `rewrite::holds_subscript`, four test helpers — and `contains_dynamic_lookup` is read off
      the finished tree instead of tracked by the translator. Three of the nine were partial
      (`mentions_unary` never looked under a `Block`; `collect_literals` only under `Unary` and
      `Binary`) and passed on shallow fixtures; the iterator makes every query total. The
      transforms and the scoped judgements stay recursive on purpose — readability was the
      reason, not stack depth, which hand-typed expressions never approach. Postorder is one
      flag on the stack entry and unbuilt for want of a consumer.
- [ ] **`max`/`min` as n-ary `Fold` kinds.** `max(max(max(x1, x2), x3), …)` over a hundred
      variables is a hundred nodes deep for no reason the tree already has a convention against:
      `Fold` is n-ary so a thousand-term sum is one node. The same for `max`/`min` would make the
      chain one node, give HC4's `Combine{Worst}` its n-ary source directly, and take the one
      plausible deep tree out of the language.
- [ ] **Upstream bug report to `antlr-rust-runtime`.** The parser builds
      `AntlrError::MismatchedInput { expected, found }` and formats it into a string before any
      listener sees it (`parser.rs:6819`), with the recovery path passing `error: None`
      (`parser.rs:5533`) while the fatal path passes `Some(error)`. So the expected-token set is
      only reachable as prose.

## Tooling

- [x] **`just tag <name>`** — the tree must be clean and the tag must be `v<Cargo.toml version>`,
      optionally suffixed (`v0.1.3-artemis`), because Artemis pins sojourn by git tag and a
      mismatched one resolves fine and wastes an afternoon. Tags locally; pushing stays a
      deliberate step. Done 2026-09-17, in a justfile trimmed to what is used: `test-compile`
      (clippy `--all-targets` compiles every test), `test-list`, `fmt-check` (folded into `lint`),
      `clean` and `test-gpu` (`just test --features gpu`) are gone.
- [x] **`cargo test --doc` in the justfile.** nextest does not run doctests; `just test` runs them
      after nextest now (2026-09-17). The `lib.rs` example had already become `no_run`.
- [x] **Justfile arg passthrough breaks on nextest filtersets.** `{{ARGS}}` interpolates into a
      pwsh command line, so a filterset's parentheses need quoting twice:
      `just test "-E 'test(/repair/)'"`. Documented on the recipe (2026-09-17) rather than worked
      around with a shebang recipe, which would move the whole justfile off `pwsh -Command`.
- [x] **Delete the Kotlin tree** and promote `crates/babel` to the root. Done in 1c26ed9 and the commit after it; no workspace table until a second crate
      appears. `set working-directory` in the Justfile went away at the same time.

## Deliberately not doing

- **Serialization is a caller concern.** "Dump the source, recompile on load" is the whole strategy,
  and `Expression` already owns its source. Neither kotlinx.serialization nor XStream can tie into a
  Rust type anyway, and attaching serde to the AST would mean writing a `to_lisp`/`from_lisp` for no
  benefit.
- **No `Send + Sync` assertion.** It is almost certainly satisfied — everything in `Expression` is
  owned — but asserting it signals "somebody is doing something with threads here" when nobody is.
  Add it when rayon actually shows up.
- **Pluggable walkers** (`compile(literal, vararg walkers)`) are gone and staying gone; merging
  sojourn-CVG replaces the need.
- **`ProblemKind::BooleanInScalarPosition`** is deleted. It existed because the JVM rewriter would
  turn `x1 > 5` into `5 - x1` in place, letting `(x1 > 5) * 3` compile. The grammar admits
  `booleanExpr` only at `returnStatement`, so that is unreachable here — pinned by
  `a_boolean_cannot_be_used_as_a_scalar`.

## Discrete variables

*2026-10-19 to 2026-10-20.* Built test-first: `tests/cvg_discrete.rs` was written red
in one commit and driven green rung by rung. The decisions that the code cannot say:

**A declaration, not a language feature.** Discreteness is a property of the variable,
so it belongs next to its bounds as a `Domain`, not in babel. As a constraint it cannot
be said. `x == floor(x)` is implicit and refused. `x % 1 == 0` is a set of measure zero,
which brute force never lands on and nothing can drive. Rounding a continuous answer
afterwards fails the oracle, and `repair` then moves the point back off the lattice.

**Ordinal only.** A discrete domain is numbers in order. Categories (`table`, `chair`)
are the caller's to alias onto an integer lattice. Sampling and proof don't care about
order: a Gibbs draw over levels and a split between levels are order-free. Three
places do read the order: a design's spread, repair's "nearest", and the local solve's
relaxation. Artemis normalises every variable to `-1..1` before calling, so it already
lives with that, and a `Categorical` domain waits for a caller who doesn't.

**A lattice is decimal.** `lower + k·step` puts level 3 of `{0, 0.1, 1}` at
`0.30000000000000004`, so `t <= 0.3` fails on a declared value. `cvg::support` reads
each bound and the step as their shortest round-trip decimals, brings them to a common
power of ten, and holds integers. Level `k` is then one correctly rounded operation on
two exact operands, which is bit-equal to parsing the literal (Clinger's fast path).
What can't be held that way, such as a step of `1/3`, a bound of `0.1 + 0.2`, or a
lattice past `2^53`, is refused with a pointer to `Domain::Levels`.

**Support, not domain.** The `Domain` is what the caller wrote, and is kept as written.
The `VariableSupport` is that declaration validated and precomputed, and it is the one
thing the relaxation and repair's pinned systems change. So the engine reads bounds only
from `ConstraintSystem::supports`. Deriving a lattice's integers per judgement means
formatting floats, which is why the support exists at all.

**The measure.** The uniform distribution over a mixed space is counting measure on the
levels times Lebesgue measure on the rest. A level is weighted by how much continuous
room it has. In the gearbox fixture, ratio 4 has four times ratio 1's speed band. That
is what the walker's Gibbs move over levels converges to when nothing is driven. With a
driven coordinate, the move re-draws the band after changing a level and does not
weight by band width: the bias the walker already had, not a new one. A design spreads
across the levels whatever the weighting, which is what the tests pin.

**Rounding inward is sound, and is the proof.** An interval is a superset of the values
it models, and rounding its ends onto levels removes only values the coordinate can't
take. `n` in `[2.5, 2.9]` on the integers is empty, and that emptiness is a proof
nothing else could give. Contraction counts one level removed as progress, because the
1% width rule would ignore a single level on a long lattice. The visit cap bounds the
crawl: `2x == 2y + 1` on `0..=20` empties in about eleven visits, and on `0..=1e6` it
crawls to the cap and stays `NotFound`, the honest answer.

**A box of one point is decided by judging it.** Bisection splits a discrete coordinate
between adjacent levels. A box narrowed to a single value in every coordinate is judged
as it stands, without `classify::centre`, which could move a driven coordinate out of
the box. If it fails, the box is empty, blamed on what it violates and on what narrowed
the box to it. That makes enumeration a proof: `n % 2 == 1` over even `n` is about
twenty contractions. The rule holds for a continuous coordinate of zero width too, and
no existing fixture moved.

**`BetweenLevels` needs a witness.** A proof over levels might mean the formulation
conflicts, or that the levels miss a region that exists. The two send the caller to
different places: fix the formula, or change the step. So `solve` re-opens
`ConstraintSystem::relaxed` with every discrete variable continuous, rebuilt rather
than re-supported, because a continuous variable can be driven and the plans differ.
- **The run.** It uses the stream the region would have kept, which goes unused on this
  path, and has no brute force: a relaxation thin enough to need a billion proposals is
  no basis for telling a caller to move a step.
- **The verdict.** A witness gives `BetweenLevels`. A continuous proof gives `Proved`
  with its own blame. Nothing gives `Proved` with the lattice's blame.

`relaxed` is a `Point` and not an `Option`. Without a witness, nothing says the levels
are the trouble.

**`Exhausted` is a proof and `Degenerate` a shrug.** When every variable is discrete
and the lattice is at most `ENUMERATION_LIMIT = 2^16` points, `design` judges all of
them. A shortfall then means there are no more points. A zero-width continuous variable
is not declared finite, so `a_region_that_is_one_point_designs_one_point_and_says_so`
stays `Degenerate`.

**Repair is pure distance, by search over levels.** "Keep the optimizer's levels unless
there's no other way" was considered and dropped. Artemis normalises discrete variables
along with the rest, and on an all-discrete system the rule decays into distance anyway.
Every continuous stage of repair moves coordinates continuously, and a level isn't
reached that way, so the stages were left untouched and run on
`ConstraintSystem::pinned`, each discrete support narrowed to one level, a zero-width
coordinate that every stage already handles.
- **Order.** Assignments are tried nearest-first by their discrete distance alone. The
  search stops at the first one no nearer than the best landing.
- **Budget.** It is bounded by `LEVEL_SEARCH_BUDGET = 64`.
- **Pruning.** A pinned box that contracts to nothing is skipped without a repair.
- **Fallback.** The reference's own levels are tried last if nothing lands.
- **Measured.** Before the search, the gearbox repair `(ratio 1, speed 90)` landed at
  `(2, 48.4)`, a sampling-box hit at 0.533. With it, it lands at `(2, 50)` at 0.521.
- **Defect found.** The search exposed one in the continuous path. The reference stage
  offered its chord's end as feasible without judging it, which was safe only while the
  reference was always feasible. It now judges it.

**The oracle stays exact; the caller snaps.** *2026-10-20.* Artemis normalises every
variable into `-1..1` and back, and the round trip moves a level by an ulp: 15 of the 101
hundredths of `0..=1` come back off their level, and `is_feasible` rejects a point
sojourn handed out. Two fixes were weighed. A tolerant oracle, snapping discrete
coordinates within some distance before judging, would change what "feasible" means on
every judging path, the walker's hot loop included, and would need a tolerance nobody
can choose well. `ConstraintSystem::snap`, a pure function the caller applies after its
own round trip, keeps the oracle exact and puts the correction where the drift came
from. Snapping is to the nearest level always, not within a tolerance: it is the caller's
statement that the coordinate is meant to be on a level, and a far-off value is then a
proposal to judge or repair, not something to hide. Writing its test found
`Levels::nearest(inf)` returning the second-to-last level, both candidates being
infinitely far; it now clamps into the span first.

The centre of a two-level variable, where HDS and Artemis anchor at normalised zero, is
never a level, so it always goes to repair. Both levels are equally far, so the
continuous coordinates decide, and an exact tie goes to the lower level. That bias stays
in the anchor: a design with the anchor among its existing points spreads to the other
level next. On uneven levels the centre is by value, not by index: `{1, 2, 4}` centres
at 2.5 and repairs to 2.

**The GPU draws levels from a table.** Each row is `(lower, upper, count, offset)`, the
last two carried as `u32` bits in `f32`. A discrete row draws `levels[offset +
floor(u·count)]`, from at most `2^20` levels in all, and a system past that stays on the
CPU. A survivor's `f32` level is not the level, so `brute_force_on_gpu` snaps each
discrete coordinate to the nearest `f64` level before the exact judgement, which decides
regardless.

## What farthest-first does to a design

*2026-10-21.* Measured by `just quality` (`tests/sampling_quality.rs`, plan and table in
`docs/sampling-quality.md`) against exact samplers written without sojourn, ten seeds a
case, beside random designs of the same count.

**It is not Latin, and the reason the docs gave for that was wrong.** They said a design
of fewer points than variables "has no useful stratification". A Latin hypercube's
guarantee is one-dimensional and holds at any count: `n` strata per axis, one point in
each, whatever `d` is. What farthest-first actually scores is random's fill, about
`1 - 1/e`, on every box, disc and simplex. Only the band and the gearbox score higher
(0.86, 0.89), because their continuous room is one thin stretch, where spreading points
apart is the same thing as stratifying them. The sentences in `FeasibleRegion::sample`,
`cvg::design` and `AGENTS.md` now say so.

**In two dimensions it beats random on everything.** At a hundred points on the box:
CL2 ratio 0.37 against 1.23, minimum distance 0.080 against 0.007, covering radius 0.093
against 0.184.

**In twenty it is less even than random.** At a hundred points the CL2 ratio is 1.89 on
the box and 1.45 on the simplex, against random's 1.0. The step-1 guess was that maximin
pushes points to the faces *and* bunches the one-axis projections. A throwaway probe over
the same ten seeds on the 20-D box split the two:

| n | within 0.05 of a face | mean `\|z - ½\|` | mean `\|corr\|` between axes |
|---|---|---|---|
| 100 | 0.154 (random 0.100) | 0.287 (random 0.250) | 0.060 (random 0.082) |
| 10 | 0.174 (random 0.100) | 0.297 (random 0.250) | 0.240 (random 0.266) |

The faces, yes; the bunching, no. The axes are less correlated than random's, and the
strata fill at random's rate. Maximin's farthest point in a high-dimensional box is
toward a corner, so each pick pushes outward on most axes at once. CL2's kernel charges
a point roughly `|z - ½|²` per axis for standing off-centre, and the product over
twenty axes compounds that. The 2-D design is outward too (0.165 near a face) and does
not pay, because in two dimensions spreading also fills the joint, which is what CL2
mostly measures there. What follows is that a better choice in high dimension has to fix
the marginals; spreading harder will not.

**The floors guard this, not a target.** Each `Case` in `tests/sampling_quality.rs` holds
its worst seed to 0.8× the first run's worst where higher is better and 1.25× where lower
is, with no piece of a region left empty. They catch a regression in today's sampler. The
twenty-dimensional CL2 ceilings sit well above random's 1.0 on purpose; a chooser that
beats random there is step 3's and the consumer's work, measured in the same columns.

# Repair for Artemis — a public `repair`, specified before it is built

**Status:** built — `FeasibleRegion::repair` in `src/repair.rs`, driven by `tests/cvg_repair.rs`.
What follows is the brief it was built from, with what changed on the way marked as such.

**2026-09-14: the anchors are gone.** `repair(point, clearance)` is a function of the system,
the point and the clearance and of nothing else. Artemis's optimisation quality was bad — it
was not finding global optima — and the anchors were the mechanism: where clamping could not
land, the chord from the nearest anchor returned a *convex combination* of anchor and
proposal, every coordinate dragged toward wherever the census had put points, innocent
coordinates included, and `Release` could revert a coordinate only where reverting kept the
clearance, which on a curved boundary it rarely does. Measured on a disc with anchors clustered
on the rim at angle zero: the corner `(1.2, 1.2)` landed at **17°** instead of 45°, the
corner `(−1.2, 1.2)` at 125° instead of 135°. Over thousands of repairs that herds the
optimizer toward the anchor cluster — the region the *walker* found — rather than along the
boundary the objective prefers, and an optimum on an active constraint far from the census is
exactly what it fails to reach.

What replaces the shotgun is the **projection**: the feasible point nearest the proposal,
`min ‖u − p‖²` over the unit cube subject to every constraint, by a local solve
(`local::nearest`, COBYLA from where clamping left the point). What is optimised over: all
`d` coordinates, in the cube the caller normalises into; the objective is L2² because COBYLA
fits linear models and L1's kinks fight them — the L1 argument was about ranking anchors,
which no longer exists. The rows carry the clearance built in — each is the worst of a
constraint's residual at the point and at the point stepped twice the clearance along every
coordinate it names — because without that a *linear* constraint defeats the solve outright:
COBYLA's steps land exactly on a linear boundary, where a strict comparison's residual is a
hair positive, and no evaluation is ever judged feasible. Every evaluation is judged where
it is made (driven coordinates settled, as the chord's probes were); the nearest with the
clearance is the answer, the nearest feasible one the consolation, and where no slice can
read a wall the landing is backed off along the projection's own direction — the boundary's
normal — in doublings of the clearance, which is what the chord used to stand in for. The
KKT view is what makes it well-posed: at the projection `x* − p = −Σ λᵢ ∇gᵢ(x*)`, the
displacement lives in the coordinates the *active* constraints name, so the objective has a
slope everywhere and the constraints pull back only where they bite. That is also the cost
follow-up not done here: restrict the solve to the incidence closure of the violated
constraints, growing on demand, and `d²m` shrinks on problems whose violations are local.
Measured: the disc corner converges in 50 evaluations, the spring vertex in 39–47, a
proposal a hair outside the ten-segment beam in 471 at twenty variables (`PROJECTION_EVALS_PER_DIMENSION`
is 64 per `d + 1`); the corner of the disc now lands at 45.00°. The chord's one capability
the projection lacked turned out not to exist: between the two bands of
`(x + 2)(x − 1) == 0`, where the chord answered `Stranded` without an anchor, the projection
reaches the nearer band. Everything below that says "anchor" or "chord" is the record of the
first design.

**2026-09-15: the metric is Euclidean, and a flat constraint gets a reference.** Two SSCCEs
from Artemis 0.13.2 against `587199c` (`docs/user-sscce/`). *`repair_lands_axis_aligned_not_nearest`:*
the clamp is the taxicab projection and `repair` returned the moment it landed, so the
Euclidean projection never ran on exactly the cases an optimizer produces — the slab
`x1 == x2 + 1 ± 0.01` landed `√2×` farther than the nearest point on every row (`x1` moved the
whole gap, `x1` and `x2` should each have moved half), a disc's near-misses 1.03–1.54× farther,
slid along the wall. Taxicab had been chosen for ranking anchors in high dimension; the anchors
are gone and the bias it carries — prefer any landing that touches one coordinate over a
nearer one that touches two — is one an optimizer must not be fed. Now the projection runs
from the clamp's landing on every repair, the clamp's landing and the projection's are both
candidates, released, and the Euclidean-nearer wins — except where the clamp's landing is
*separable* (every constraint naming a moved coordinate names it alone: bounds), where the
axis projection already is the Euclidean one and two hundred bounds would otherwise cost a
solve of minutes. Three tables are the fixture, each against its closed form.
*`repair_strands_on_a_product_constraint`:* Keane's `0.75 − ∏xᵢ < 0` with seven coordinates at
`1e-11`; the product is `1e-59`, the constraint flat to fifty digits in every direction, every
slice empty, COBYLA's linear model of it a flat model — `Stranded` with a feasible point 4.6
units away. No local method sees a flat constraint; what is needed is a feasible point to walk
in from, and it comes without anchors: `find_initial` over the declared box under
`REFERENCE_SEED`, a constant, so the reference is a function of the system; a chord bisected
from it toward the proposal; the projection from the chord's landing, where the constraint is
well-scaled again. The reference decides only which basin, never where in it, and is reached
only when the projection from the point saw nothing feasible. **Cost, measured in release:**
about 0.3 s per repair at fifty variables on the slab and the ball (≈30× that unoptimised),
1.3 s for Keane at 50 including the solve, 4.7 s at 100 — the projection from the flat point
spends its whole budget before the reference is tried, which is why the hundred-variable case
is not in the debug suite. The cost follow-up stands: restrict the projection to the incidence
closure of the constraints active at the landing (`d²m` shrinks to the coupled coordinates —
two on the slab, all of them on Keane), and stop a projection that has made no feasible
evaluation in its first simplex rather than at its budget.

**2026-09-15, later: gradients, and Newton on the KKT system.** The projection is the
college question — the shortest distance from a point to a surface is along its normal — and
COBYLA was answering it by finite differences dressed as linear models at `O(d²m)` a step. No
library takes the gradient-based version off our hands: basin's nonlinear-constraint solvers
are derivative-free, its gradient methods handle linear constraints only, SMT decides and
never differentiates. So the gradient is ours, and it is computed **on the tape, in reverse
mode** (`src/eval/differentiate.rs`): the primal instructions renamed to single assignment
(a fold's accumulator is written once per term), then a backward sweep emitting
`ā += d̄ · ∂op/∂a` as ordinary instructions — one rule per instruction beside the
instruction set, the same table shape as every other backend — with the `Load`s' adjoints as
the partials. Every partial from one run at a fixed multiple of an evaluation, whatever `d`;
the AST route was considered and would have paid `d×` and `n²` on a product. Before
allocation, because the allocator reuses physical registers and an adjoint per physical
register is wrong; `VirtualTape` is that form, `AllocatedTape` the one the executors run,
and both are the evaluator's intermediate representation. The gradient is compiled beside the
expression by `Compiler` (`compile` is its defaults, `Gradient::BestEffort`; `Gradient::Never`
skips it), so "compiled" means every register is allocated and nothing is lowered later;
`CompiledExpression::gradient()` and `CompiledGradient::eval` — the batched Jacobian — are
public, for a caller's own gradient solver. `floor`, `ceil`, `sgn`, `%` and a computed subscript decline the whole tape;
`abs`/`max`/`min`/`Worst` are differentiated almost everywhere through `sgn`. Every
operator is pinned against a central finite difference of the compiled expression, and a
constraint's gradient against a finite difference of its compiled *residual*, so the sign
conventions hold end to end. `Constraint::gradient` is computed once in `ConstraintSystem::new`.

`src/cvg/newton.rs` is the projector: the active set is the constraints violated where the
point stands (bounds are rows like any other); each solve is `(J Jᵀ) λ = J (p − uₖ) + g`,
`uₖ₊₁ = p − Jᵀ λ`, a `|A| × |A|` LU; the fixed point you described adds a row the nominated
point violates and drops one whose multiplier came out negative, until the KKT conditions
hold — verified, not assumed. Fallible everywhere: a biting constraint without a gradient, a
singular solve, no convergence in the budget, all `None`, and COBYLA runs only then. The
iteration converges *linearly* on a curved constraint, since it models the objective's
Hessian and not the constraint's — the missing `Σ λᵢ ∇²gᵢ` is the contraction factor, a half
on the sine band — which is why `NEWTON_ITERATIONS` is 128 and not 32: the ball oracle
(below) caught the sine band landing on the clamp's point because Newton had not converged
at 32 and COBYLA was skipped. **Cost, measured in release, per repair:** slab-50 **15 µs**
(was 0.3 s), ball-50 near-miss 19 µs, a hair outside the ten-segment beam 0.85 ms, the
hundred-segment beam 132 ms; Keane keeps its reference path and lands by Newton from the
chord's landing. Every existing fixture is unchanged; the geometry ones tightened from
`1e-4` to `1e-10`.

**The ball oracle**, `tests/cvg_repair.rs::no_point_in_the_repairs_own_ball_is_nearer`:
random proposals against six shapes with no closed form, twenty thousand uniform points in
the ball of the repair's own radius, none feasible with the clearance may be nearer. It is a
test and not a stage because uniform points in a ball localise nothing past a handful of
dimensions. It earned its keep on the first run.

Follow-ups: `prune` over a ball as the low-dimensional global check; a true Newton with the
constraint Hessians if the linear rate ever shows.

**2026-09-15, later still: the sampling box.** A hunt for a fixture every stage strands on
found none — the reference path's feasibility search is hard to defeat on a fat region — but
found the failure that matters instead: on a constraint with a jump (`floor`, `%`, `sgn`)
every projector declines and the chord from the reference lands *wherever it crossed in*:
0.40 of the box from the proposal on `floor(x1·100) % 7 == 0` where a cell sat at 0.20; 0.235
on a two-comb checkerboard where one sat at 0.007. The ball oracle over jump shapes is the
red test (`no_point_in_the_repairs_own_ball_is_nearer_across_a_jump`, allowance 5 % — a
sampling method's, not a projection's). The backstop is the box you described: uniform
draws around the point under `SAMPLING_SEED`, doubled from a sixty-fourth of the box while
empty, shrunk onto the nearest hit once not, twenty rounds of 256 draws, then the chord from
the nearest hit toward the point and the usual step off the wall. It runs beside the
reference — only when clamp, Newton and COBYLA all produced nothing — and the aggregator
picks. The comb now lands at 2.016 where the nearest cell is 2.000 away, the checkerboard at
0.05 where the oracle's best was 0.07. The earlier rejection of a shrinking box stands for
what it was about — thin regions, high dimension, and as the *first* mechanism; those are the
projection's now. Nothing in this crate reaches `Stranded` on a fat region any more.

**2026-09-16: latency, telemetry, and the stage order.** Z3 is gone and the batch stream has
no user, so the async on `solve` is a shape left over from both; before reshaping it, a clear
performance picture. A `tracing` span per stage of `repair` (and per Newton/COBYLA run),
carrying counts — iterations, active-set rounds, evaluations, draws, and which stage won —
at `debug`, so `RUST_LOG=sojourn=debug` gives a stage-by-stage account and a span-timing
subscriber gives the clock without a clock in our code (`tracing-subscriber` is a
dev-dependency for exactly that). The trace found three wrong-shaped latencies, all order,
not capability, all now fixed:

- *Keane-50, 400 ms → 3.7 ms.* COBYLA spent its whole budget on the flat product before the
  box (1 ms) got a turn. The box now runs first off the smooth path; COBYLA follows only where
  the box found nothing, or the dimension is small (`COBYLA_CHEAP`), where it is a cheap
  cross-check the box needs on a thin many-branched region and the box would otherwise
  shrink-lock onto the wrong branch (`(x+2)(x-1) == 0` regressed exactly there mid-change).
- *beam-100 hair, 187 ms → 3.8 ms.* The clamp spent 90 ms narrowing the deflection constraint
  (names 200 coordinates) per coordinate per sweep, only to fail — no single-coordinate clamp
  satisfies it. The clamp is now skipped where any constraint names more than `CLAMP_LOCAL`
  coordinates; Newton from the point does the work.
- *spring vertex, 2.3 ms → 59 µs.* Newton converges there in fourteen iterations; it now runs
  from the point first and its clear landing is returned before COBYLA or the box.

So the order is: clamp (where cheap, exact where separable) → Newton from the point (return
if clear) → box then COBYLA (off the smooth path) → reference (nothing feasible at all). Newton
returns its wedge bisector as the inward direction, because `p → x*` leans out of a sharp
wedge like the spring's. Repair latency on Artemis's shapes is now tens of microseconds to a
few milliseconds. The async question is next: `repair` is bounded by counts and belongs
synchronous; `solve`'s opening can still be 375 ms at 200 variables and brute force is
unbounded, so its shape is the open one.

**2026-09-16: `solve` is synchronous, the stream is gone, `sample` is a design.** The async
`solve`, the worker thread, the channel, `take`/`try_take`/`available`/`close`/`Status` and
the `Drop` that joined the worker all existed for Z3's hangs and for a batch consumer.
Z3 is gone, and the one consumer (Artemis) called `take` once, for an initial design of a
handful of points at a dimension usually larger than the count. So:

- `ConstraintSolver::solve` is a plain fn that runs the opening on the calling thread and
  returns `FeasibleRegion { system, points, reference }` — an immutable value. Its budgets
  are all counts, and that is the shape of "this may take a while": a search that spends
  them without a point is `NotFound`, never a call that does not return. Brute force keeps
  its `std::thread::scope` fan-out as a private detail — fan out, work, join — and nothing
  runs after any call returns. `Cancellation` and every `cancel`/`stop` parameter went
  with the future they watched. `futures-channel` is gone; `pollster` remains only under
  the `gpu` feature for wgpu's adapter requests.
- `FeasibleRegion::sample(existing: MatRef, count, seed) -> Result<Mat, SampleError>` is a
  space-filling design: a pool of candidates (the region's own points, a round of uniform
  proposals, hit-and-run for the rest, under streams derived from `seed`), then
  farthest-first selection in box-normalised Euclidean distance, seeded by `existing` —
  the caller's own points, spread away from and never returned. Artemis repairs the box
  centre, hands it in, and concatenates. A function of (region, existing, count, seed).
  `Degenerate { found, wanted }` when the region runs out of distinct points: a
  point-sized region, or a fully driven one (`abs(x1) == 1` is two points, and a design
  of two hundred over it is two). Not a Latin hypercube: at `count < dims` an LHS
  guarantees nothing, and "far from each other and from what I have" is the whole
  requirement. *(2026-10-21: overstated. An LHS's one-axis guarantee holds at any count;
  see "What farthest-first does to a design".)* The pool is `2·count + 128`: the walk is the cost, and a small design
  needs a floor of candidates to be a choice at all.
- `repair`'s reference stage reads the region's `reference` instead of running
  `find_initial` per call: the opening's local solve from the box centre where it ran one,
  else one run in `solve` under a fixed seed, else the witness. The centre matters: a
  random probe hit of Keane's region can stand where the product is already nearly flat,
  and a chord from there strands the projection (1.3 s against 0.8 s from the centre;
  the rest of that 0.8 s is COBYLA from the point spending its budget before the
  reference stage runs at all — the next latency to look at).
- `tests/cvg_benchmarks.rs` — the stream's uniformity, autocorrelation and radial oracles —
  is retired: a design is deliberately not a uniform sample. Its two lessons (lag
  `CHAIN_COUNT`, pooled effective sample size) are kept in `AGENTS.md`.

Measured in release: disc `sample(10)` 1.5 ms; slab-20 `sample(10)` 28 ms, `sample(256)`
48 ms; Keane-50 `sample(10)` 8 ms; beam-100 (200 variables) `sample(10)` 21 s — the
walker's burn-in, `max(2000, 16·d)` steps on eight chains, twice. That burn-in is the one
cost on the design path that is not milliseconds, and it is paid per call, which is the
price of a design that is a pure function of its seed. The default `solve` on beam-100 is
35 s, all of it the coverage bisection at 200 variables (`DEFAULT_PRUNE_BUDGET` is a
count of contractions, and a contraction at 200 variables walks every constraint's tree
per coordinate); without `Strategy::Prune` it is 21 ms. Both are pre-existing, both are
recorded here as the next two performance items rather than fixed.

**2026-09-16: a wall the proposal never left.** Artemis 0.13.4 on the c06 slab corner:
`repair` moved `x15` — in no constraint, on the box wall — by 0.14, and by a seeded amount at
every `n ≥ 10` while `n ≤ 8` was exact. The trace (`tests/regression_fixture.rs`,
*a_wall_coordinate_in_no_constraint_stays_put*): both the clamp and Newton landed feasible
*without* the clearance, so the sampling box's random draw was the only clear landing, and at
`n ≤ 8` COBYLA's cross-check happened to beat it. Newton's step-in walked the bisector of its
*active* rows, and a wall the proposal stood on is tight, not violated — not active — so no
rung could clear it. Fixed by summing every row the landing *stands on* into the bisector
(`newton::TIGHT`); the first rung now clears every wall at once and Newton wins at every `n`.
Left as it is: the clamp's own failure on that point — `x1` on the box wall and the slab's
edge at once gets a slice one clearance wide, aims for its middle, and ping-pongs with `x2`
by halves through all eight sweeps. A "step in from the wall you stand on" rule instead of
the middle would converge in two; not needed while Newton lands, noted in case a
non-differentiable constraint puts the clamp back in front.

**2026-09-18: Newton from the chord, and a step that stops short of a wall.** Artemis 0.13.5
on Keane's bump at a hundred variables (`docs/user-sscce/`, kept as
`regression_fixture::repair_strands_on_a_product_constraint::the_100_variable_proposal_with_eleven_zeros_is_projected_not_searched`):
a wolf's proposal with eleven coordinates clamped to 0 has a product of exactly 0 and a
product gradient of exactly 0 in every coordinate, so Newton from the point is singular by
construction; the box missed in twenty rounds; COBYLA from the point spent 2084 evaluations
and landed infeasible; COBYLA from the reference's chord spent 1551 more and landed feasible
— 5.4 s, and 40% farther than the nearest feasible point, which is the proposal with its zeros
lifted to `3.7e-6` (`e^11 · P = 0.75`, `P ≈ 1e62` the product of the other eighty-nine). Two
things were wrong in `newton::nearest`, found in that order by tracing Newton from the chord
landing: (1) the active-set guess was "what `from` violates", which at a feasible chord landing
is nothing, so the first step jumped to the target and its zero gradient — the guess is what the
*target* violates now, the same set for every caller that starts at the target; (2) with the
right guess the linearisation at the chord landing sent the eleven coordinates through zero —
a step that would cross a box wall now stops at nine tenths of the way (`WALL_FRACTION`, the
fraction-to-the-boundary rule) and linearises again from inside, and the coordinates fall
geometrically to `e`, converging in fourteen iterations. `repair` runs that Newton from the
reference's chord right after Newton from the point declines, as a *candidate* — the reference
still decides only which basin, and `the_nearer_band_wins` caught the early return that forgot
it — and skips COBYLA-from-the-point where the chord's projection is clear and the box found
nothing. 5.4 s → 0.02 s, and the landing is the closed-form optimum to a ten-thousandth. The
module doc's reference stage had described "the projection from the chord's landing" since
2026-09-15; the code had been running COBYLA there because Newton from anywhere but the
target could not work. The stage is now what the doc said.

**2026-09-16: where a design's time goes, by count and by flame graph.** A `judged` tally on
the walker (`burn_in` and `walk` spans at `debug`) and a `samply` run on
`tests/profiling.rs::a_design_on_the_100_segment_beam` (200 variables, two dense
constraints, 14 s in release). The counts: burn-in is 8 chains × 3200 steps, 39k candidates
judged, 4.1 s; the walk is 148 pool points × 400 steps of thinning, 107k judged, 9.9 s; one
`is_feasible` is 20 µs. So a step costs ~100 µs per candidate judged, and the flame graph
says where the other 80 µs are: `axis_chord` → `interval::slice` → `narrow`, the HC4 pass
over the *AST* of the two deflection sums, with `alloc::vec` under every node — `Narrowing::
backward` re-runs `forward` on each subtree it descends into, and `in_expr` collects a `Vec`
per fold. `is_feasible` is ~10%, all of it the count (the walker judges every constraint on
every candidate; an axis move can change four of 201). In order of expected payoff:

- [x] **The interval tape.** Compile each constraint's narrowing once, at
      `ConstraintSystem::new`, to the *virtual* (SSA, unallocated) tape `eval::irgen` already
      produces — every intermediate written once, which is what HC4's backward pass needs —
      and run it as two straight-line sweeps over a preallocated frame of intervals: forward,
      one interval per register; backward, in reverse, each instruction narrowing its
      operands from its result through the `invert_*` rules that exist today. No recursion,
      no re-evaluation, no allocation per call; the root's truth (`<= 0`, `± t`) is pushed
      onto the two sides' registers, and the wanted coordinate is read off its `Load`s. The
      shape of `eval/differentiate.rs`, over the same instruction set; `Gather` declines as
      it does there and the constraint narrows nothing, as now. Lives in `cvg`, beside the
      interval arithmetic it drives. Expected 5–10× on the narrowing, which is ~75% of a walk.

      **Spiked 2026-09-16** (a test-only HC4 over the virtual tape, deleted after the
      measurement): on beam-100's deflection constraint — 200 symbols, 2802 instructions —
      the AST walk is 174–185 µs a narrowing and the tape 21.8 µs, **8.5×**; on a local
      two-variable constraint 431 ns against 221 ns, 2×. Three things the spike settled:
      *(i)* the tape's forward pass is the floor, 18.6 µs at ~7 ns an interval op with the
      ulp padding; the backward pass was the other 30 µs until a per-slot bitset of the
      inputs each value depends on, computed once at compile time, let it skip every
      instruction that cannot reach the wanted input — a `sum` hands a requirement to every
      term and only one term holds the coordinate — after which it is ~3 µs. *(ii)* The
      emitter's `x^n → x·x·…·x` is the right transform for evaluation and a lossy one for
      narrowing: the inverse through repeated multiplication by `h ∈ [5, 100]` gave
      `[5, 100]` where `invert_power`'s root enclosure gives `[26.1, 100]`. The interval
      tape therefore keeps `Pow` with a literal exponent as one instruction (an option on
      `lower`), forward by `power`, backward by `invert_power`. Exactly where an AST
      transform happens turns out to matter per consumer; accepted as a per-consumer option
      rather than a second lowering, and revisited only when a test needs the interval side
      to reason harder. *(iii)* `a == b ± t` needs no special root: the emitter already
      emits it as two `Compare`s under `Worst`, and `max ≤ hi ⇒ both ≤ hi` recovers
      `a − b ∈ [−t, t]` exactly. Amdahl: ~3× on a 200-variable design from the tape alone,
      4–5× with burn-in in `solve` and affected-only judging. The floor after that is the
      forward pass, and the only way under it is incremental evaluation across the walker's
      axis moves (one coordinate changes per move) — noted, not planned.

      **Built 2026-09-16**: `cvg/hc4.rs`. `hc4::compile` lowers the canonical tree
      as parsed, whole powers intact (the evaluator unrolls them on its own copy first —
      see the next item), takes it to single assignment (`VirtualTape::single_assignment`,
      now shared with `differentiate`), numbers the registers into dense slots and computes
      the per-slot dependency bitset; `narrow` is the two sweeps over caller-owned `Frames`;
      `slice` moved in beside it, leaving `interval.rs` the arithmetic.
      `Constraint` carries its narrower beside its tape and gradient; `slice_conditioned`
      and `prune::contract` run it; the AST walker (`in_expr`, `Narrowing`) is gone. The
      whole interval test suite — containment against the evaluator, the sweep-based
      soundness of every inverse, the whole-power root — passes unchanged on the tape, and
      `a_whole_power_narrows_through_its_root` alone fell from 14 s to under a second in
      debug. Release: the 200-variable beam design 14.1 s → **3.4 s**; the 99-equation chain
      2.1 → 1.8 s; the full test suite 61 s → 31 s. The conjunction rule
      (`worst(a, b) ≤ hi ⇒ a ≤ hi ∧ b ≤ hi`) is the `Combine { Worst }` instruction's
      inverse wherever it sits, and a user's `max(a, b)` still answers `ENTIRE`.
- [x] **Powers get a canonical form at parse — the whole-power half.** `a*a*a`, `a^3`
      and `a^(6/2)` reached every consumer as written, so narrowing quality depended on
      spelling: `h*h` inverted through `Mul` (weak) where `h^2` inverted through its root.
      Built 2026-09-16, test-first: `rewrite::canonicalize` names the pass sequence `parse`
      runs and ends with `collect_powers` — `f * f`, `(f * f) * f`, `f * (f * f)`,
      `f^n * f^m`, `prod(1, n, i -> f)` become `Pow(f, n)` whatever the grouping. The one
      pass that is not bit-exact, decided so: the unroll multiplies left to right, so a
      right-associated spelling can move by an ulp, and one power with one tape and one
      bound was worth more than a spelling's rounding (a `powf` would round differently
      again). The reds that
      drove it: the parsed tree is a power; `h * h < 9` narrows `h` to `[0, 3]`, the same
      bound as `h^2 < 9`; the tapes of `f*f*f*f` and `f^4` are the same length. Then
      `WholePowers` went: `rewrite::unroll_powers` is the evaluator's own pass, applied in
      `eval::bind` on a copy of the tree, `(t·t)·t` with a compound base bound to a fresh
      `let` (the first pass to mint a slot), and the emitter knows nothing about powers. A
      property test pins that the tree before and after collecting evaluates to the same
      bits where the order was kept and within four ulps where it was not. The narrower's
      slices moved by ulps against the AST walk's (its chain inverse
      through a sum is the Gauss–Seidel form where the walk was Jacobi), so the walker's
      tallies on the beam shifted by under one percent; both are sound supersets.

- [ ] **Canonical form, the rest.** Candidates, each wanting a red test first:
      - *Rational and half exponents.* `expr^0.5` and `expr^(1/2)` are `sqrt(expr)`, which
        has an inverse (`invert_unary`) and a cheap kernel where a real-exponent `Pow` has
        neither; `^(1/3)` is `cbrt`. `^(3/2)` and the general `expr^(p/q)` want an
        `nth_root` the arithmetic does not have — `root_enclosure` is the interval half of
        one; the evaluator half is `powf`, which is what it does today, so only the
        narrowing side gains.
      - *The unary spellings.* `sqr(x)`, `cube(x)` and `x^2`, `x^3` are one thing; fold the
        unary forms into `Pow` (or the other way) so the evaluator's unroll and the
        narrower's root see one node kind. `abs(x)^2` is `x^2`, exactly.
      - *Operand order.* `a * b` and `b * a` (and `+`) are the same bits in IEEE, so sorting
        a commutative node's two operands by a canonical order costs nothing and makes the
        two spellings one shape — the precondition for the next item to find them.
        Reassociating a chain is not exact and is not this.
      - *Duplicate subexpressions into a `let`.* A non-leaf subtree that occurs twice
        (structurally, `same_shape`) is computed once: bind it in the nearest enclosing
        block and read the local. `collect_powers` is the special case where the two
        occurrences are the factors of one product; `unroll_powers`'s slot-minting is the
        mechanism. Open questions before building: a depth or cost threshold below which a
        recomputation is cheaper than a slot; scoping when the occurrences sit in different
        blocks; and fault order — a shared subtree faults once, at one span, where the
        duplicates faulted at the first evaluated, which the diagnostics tests pin. Arguably
        the evaluator's concern alone, but the narrower reads the same tree and a value
        computed once is one slot for it too.
      - *Identities that are exact.* `x * 1`, `x / 1`, `x^1` are `x` to the bit; `x + 0` is
        not (`-0.0 + 0.0` is `0.0`), so it stays.
- [x] **Burn-in into `solve`.** The walker was fresh per `sample` call so that a design is
      a pure function of `(region, existing, count, seed)`; the chains' burn-in is a
      function of the region alone. Done 2026-09-16: `Ladder` draws one more stream after
      the strategies' (so every seeded fixture is what it was), `solve` burns the walker in
      from the opening's points on `Satisfied`, the region holds it, and `sample` clones and
      reseeds it under the design's seed — still pure, still seeded, and two designs from
      one region walk from the same chains. Beam-100: `solve` 21 ms → 1.26 s, `sample`
      4.0 s → 3.0 s, the sum what it was. The debug suite is ten seconds slower, since
      every `solve` now burns in whether or not anything samples — accepted: solving a
      space is where the library thinks about it. The fit's data is the burn-in itself,
      not the opening's point count, so more starts would not sharpen it; a longer burn-in
      or a cheaper estimator past ~30 variables would, if a fixture ever asks (95% of use
      is under thirty variables, and the diagonal the fit degrades to at two hundred is
      what the sphere-plus-axis moves want anyway).
- [ ] **Thinning for a design.** `THINNING_PER_DIMENSION = 2` decorrelates a *uniform
      sample*; a design is chosen farthest-first from the pool and wants spread, not
      independence. A count knob to measure after the tape lands, since it changes what the
      pool is rather than how fast it is made.
- [x] **The rng at the API.** A seed on the builder *and* on `sample` was unidiomatic;
      `solve(&system, &mut rng)`, `sample(existing, count, &mut rng)` — and `repair` needs
      none — is the standard shape, and makes "same generator state, same answer" the
      caller's contract. Done 2026-09-16: `with_seed`/`with_rng` and the OS-entropy default
      are gone, the builder is settings only and `Clone`, `solve` takes `&self`. Each door
      draws one `Xoshiro256PlusPlus::from_rng(rng)` and the engine is as it was — nothing
      past the boundary is generic, and the caller's generator advances by thirty-two bytes
      per call whatever the search spends. `rand` is re-exported (`sojourn::rand`) since
      `Rng` in the signature makes it a public dependency. The two constant seeds went too:
      `REFERENCE_SEED` guarded a "function of the system alone" that did not hold (the
      opening's own local solve, which usually *is* the reference, drew from the caller's
      stream already), and a rule for "parameter here, constant there" was a paragraph to
      justify. Now the region keeps one stream from the ladder, drawn last; the reference's
      extra starts draw from it at `solve` and every repair's sampling box draws from a
      clone, so a repair is a function of the region, the point and the clearance, and
      everything the region does is a function of the region. Every seeded fixture's stream
      changed once (the engine now seeds from drawn bytes rather than `seed_from_u64`
      directly); no verdict moved.
The consumer's side is Artemis's design note *"the constraint-handling trait"* (2026-09-09),
which is the contract everything below is written against. Not to be confused with
[Repairing a point rather than discarding it](#repairing-a-point-rather-than-discarding-it),
which is about nudging a *solver's witness* onto the `f64` side of a boundary so the walker can
seed from it. This is the other direction: Artemis proposes points that are nowhere near
feasible, thousands of times a run, and needs a feasible one back near each.

## What Artemis asks for, in one paragraph

Three functions and a contract. `check(x)` returns feasible, infeasible with a **rank** (an `Ord`
newtype, never a number Artemis can do arithmetic on), or *indeterminate* when the expressions
have a hole in their domain at `x`. `repair(x)` returns a point that passes `check`, near `x`.
`residuals(x)` is a vector of `g_i <= 0` values for a constrained local solve, and its arity is
**one** in v1 — a single `max` over every constraint. Artemis learns a point count, a dimension
count and that arity, and nothing else. The contract on `repair`: deterministic, total over the
declared box, microseconds-ish, idempotent, and *strictly* feasible by the same oracle `check`
uses — a point one ulp on the wrong side is a failed CFD run, not a slightly worse one. The
initial design is not a method: Artemis takes a feasible matrix from the caller, which is
`FeasibleSamples::take(n)` already (one column per sample, no transpose). Bound tightening from
the census extents is Artemis's follow-up 2, not ours.

## What is already here

- `check` is `ConstraintSystem::is_feasible`: every constraint compiles to a residual whose sign carries
  truth, and `<= 0` is the `g(x) <= 0` form the optimiser wants. `residuals` is that vector
  before `all` collapses it; the v1 arity of one is `max` over it.
- The initial design is `FeasibleSamples::take`. "Region is empty" is already a startup verdict
  (`Satisfiability::Unsatisfiable`), not a runtime error.
- The zero-tolerance rejection Artemis's note relies on is real: `frontend/syntax.rs` refuses
  `tolerance <= 0.0`, so every equality is a slab with volume.
- The walker's chord is the *idea* repair is built from — a segment from a feasible point, with
  feasibility found along it — but not the code: the walker's shrinkage *draws* its step, and a
  repair may draw nothing, so the chord here is a plain bisection. Hit-and-run itself has no
  traction (it starts *inside*); the chord does.
- `ConstraintSystem::slice` computes a coordinate's conditional feasible range with the others held, which
  is exactly an axis projection when a point is clamped into it — and it already serves both the
  free coordinates (the walker's axis moves) and the driven ones (`retract`), which is why the
  "drive" and "clamp" stages first sketched below turned out to be one stage.

## Where the note is wrong about babel — three mismatches to absorb in the adapter

1. **`+INFINITY` is not a value here.** Contract 8 says an infinite residual is valid and
   well-ordered. The evaluator faults on *any* non-finite intermediate (`lane.rs`, `checked`),
   inf included. The fault carries the value, so split it: an **inf** fault is
   `Infeasible(Violation::INFINITY)` — a rank meaning "far away", which is what `exp`
   overflowing far out in the box *is* — and a **NaN** fault (`0/0`, `ln` of a negative,
   `inf - inf`) is `Indeterminate`. This matters because v1 aborts the run on `Indeterminate`
   and contract 3 promises points nowhere near feasible; without the split, one overflow kills
   a run.
2. **Determinism needs the census pinned.** Repair toward census anchors is deterministic only if
   the census is, and `ConstraintSolver` seeds from OS entropy by default. The adapter must pass
   Artemis's seed through `with_rng`. Nothing inside `repair` itself draws randomness; even the
   chord shrinkage uses a fixed sequence.
3. **Distances are L1 in box-normalised coordinates.** Artemis thinks in its unit cube; "near"
   in real units is meaningless when one variable is metres and another pascals. L1 rather than
   L2 because nearest-neighbour contrast collapses in high dimension and collapses fastest for
   high-order norms (Aggarwal, Hinneburg & Keim 2001); the nearest-anchor search over a
   10⁴-point census at 50–200 dimensions is exactly that case. The other half of that folklore —
   *raise* the power when aggregating violations so the worst offender is not averaged away —
   is also true and is why the feasibility predicate is a `max`, not a sum. Different question.

## The API — a function over the system, not a method on the pool

```rust
impl FeasibleRegion {
    pub fn repair(&self, anchors: MatRef<'_, f64>, point: &[f64], clearance: f64)
        -> Result<Point, RepairError>
}
```

*(2026-09-12: `clearance` and the `Result` were added; see "The clearance" below. Later the
same day `repair` moved onto `FeasibleRegion` — the renamed `FeasibleSamples`, which now holds a
clone of its system — as `region.repair(anchors, point, clearance)`; the free function is gone
from the crate root. A region that could not be solved has nothing to repair toward, and the
one caller with anchors in hand is the one holding a region. The paragraph that follows predates
both changes and reads `None` for what is now `Err(RepairError::Stranded)`.)*

The brief said "on `FeasibleSamples`, it owns the census and the problem". It owns neither: the
compiled system is moved into the worker thread and the buffer is drained by `take`. What made a plain
function possible is that `ConstraintSystem::new` was already compiling every constraint to prove
it binds and throwing the tape away. **It keeps it now** — each constraint as written and as
compiled in one `Constraint`, plus the drive plan and the incidence graph — so the system *is*
the compiled handle (`src/system.rs`) that every strategy takes, the SMT logic rides on the `Ladder`
instead of on a wrapper type, and `repair` costs nothing per call beyond the algorithm. Anchors are an argument, one column per
point in the shape `take` returns, because determinism has to be "same system, same anchors, same
point" and the caller is the one holding a seeded census. `None` means clamping alone could not
land and no anchor was feasible; with the census the caller starts from, it does not happen.

## The algorithm — as built

```
repair(system, anchors, x, eps) -> Result<Point, RepairError>
  0. is_feasible(x, eps)                     -> Ok(x) unchanged       (idempotence)
  1. clamp, at most CLAMP_SWEEPS (8) rounds:
       for every coordinate, its slice with the others held at the current point,
       shrunk by eps * width at each end, and the L1-normalised cost of clamping into
       it (a slice too narrow to shrink aims for its middle). Apply cheapest first,
       cumulatively; nudge each landing inward through a doubling ladder of ulps until
       the coordinate's own constraints pass with the clearance; is_feasible(eps) after
       each -> done.
  2. shotgun: the ANCHOR_SHOTS (8) nearest feasible anchors by L1-normalised distance.
       From each, bisect t in [0, 1] along anchor -> current (t=0 feasible, t=1 not),
       settling driven coordinates at every probe; CHORD_BITS (60) halvings. Keep the
       feasible end and clamp it as in 1, which from a feasible point is the step off
       the walls; where no slice can read a wall, back off along the chord in doublings
       of eps until is_feasible(eps). The candidate nearest x wins; an anchor with the
       clearance is a candidate itself.
  3. release: put each moved coordinate back to x's value where that keeps the clearance.
  4. something feasible seen but nothing with the clearance -> Err(Cramped { nearest })
     nothing feasible at all                                -> Err(Stranded)
```

Three things changed from the sketch. **Cheapest first, not schema order.** The L1 projection
onto a half-space moves the single coordinate with the steepest normal component, and greedy-by-
cost reproduces that; schema order answered several times farther on the first fixture. **Driven
coordinates are not privileged.** On `2*x1 + x2 == 3 +/- 0.001` from `(3, 3)`, driving `x2`
costs twice what clamping `x1` does; the drive-first stage would have picked wrong, so it went.
The settled-mask ordering in `retract` is for *travelling along* a surface, not landing on it
once; the deterministic `settle` is used only in the chord's probes. **A release pass.** A clamp
is computed against neighbours that then move too, and a chord carries every coordinate when one
constraint was active; putting each back where feasible is one check per coordinate and only ever
shortens the answer.

Two things were learned landing on bounds. The nudge ladder is in ulps of the **box width**, not
the value: a bound at zero has ulps of `5e-324` and a strict comparison is satisfied only past an
absolute `f64::MIN_POSITIVE`, which no ladder of those reaches. And `x^2` is a `^`, which
narrowing declines, where `sqr(x)` is inverted (both branches, intersected with the box) — the
disc fixture is written with `sqr` for that reason, and the gap is on the list below.

Guarantees, in the order Artemis relies on them: the result passes `is_feasible` (which already
includes `in_box`, so the declared box is never left — Artemis's sub-box hole is entirely theirs)
*with the clearance*; unchanged input if it already has the clearance, hence idempotent;
deterministic given system, anchors, point and clearance, bit for bit; **never farther than the
nearest anchor with the clearance among the shots**, because that anchor is itself a candidate.
There is no randomness anywhere in it, not even a seed.

**The clearance** *(2026-09-12)*. Artemis's `e03-spring-3` found the ulp ladder wanting: its
wrapper stores points normalised to `[-1, 1]` and denormalises before evaluating, the round trip
is one ulp off for some values, and one ulp on the spring's wire diameter moves two residuals by
`1e-8` (`d^4` in a term of order `1e5`). A landing a few ulps inside — the ladder's, or the
sixty-bit bisection's along a chord — was `-1e-15` in the residual at the vertex where the
deflection and shear constraints meet, and infeasible again by the time Artemis looked. Contract 1
asked for "a final deliberate step inward"; a margin in ulps of the coordinate is not that at a
vertex. So `repair` takes a `clearance`: a fraction of each variable's box width, `eps * width_i`
on coordinate `i`, which is the per-axis tolerance without an array type, in the unit cube Artemis
normalises into and the metric repair measures in. A point *has* it when it and its `2d` axis
neighbours at that distance all pass — and on a convex region that is the whole L1 ball. The
oracle did not grow a second family for it: `is_feasible` and `is_feasible_after` take the
clearance, and it is a property of how *one constraint* is judged (`ConstraintSystem::holds`: the
residual at the point and at `eps * width` either way along each variable the constraint names,
with the box shrunk by the same step), so the after-a-move form consults the affected constraints
alone exactly as it did at zero, and zero is the plain question. The slices construct it (clamp into the slice
shrunk by the step; a slice is an outer enclosure, so this aims and never proves), the ladder
breaks the tie a padded edge leaves against `is_feasible_after` with the clearance, and the chord backs off in
doublings of `eps` where narrowing declines the constraint and no slice can read the wall.
`1e-12` is the recommendation for a normalising caller; `0.0` is the old contract, bit for bit.
`Err(Cramped)` names the nearest feasible point when the room is narrower than the clearance,
which is the region being thinner than the caller's own noise floor. What was not done: a
finite-difference normal (ill-defined at exactly the vertices this is for, and the axis and the
chord are inward directions already in hand), and re-checking only the neighbours that share a
constraint with a reverted coordinate in the release pass, which is the exact cheaper form of
its full check if it ever shows in a profile. One softness, pinned by
`a_feasible_point_without_clearance_is_moved_inward`: a point *on* a wall ties every
coordinate's clamp at cost `eps`, the tie goes to rounding, and a coordinate that moved first
without clearing another's neighbour is not always put back — the answer has the clearance and
moved at most `eps` per coordinate, which is what is promised, not L1-optimality, which never was.
The SSCCE is kept as `tests/regression_fixture.rs::repair_lands_too_close_at_a_vertex`.

Cost: stage 1 is `sweeps × d` slices plus up to `d` feasibility checks a sweep, stage 2 is
`K × CHORD_BITS` feasibility checks. Tens of microseconds on a simple tape; a millisecond at 200
dimensions with transcendentals. There is no gradient anywhere — babel has no derivatives, and a
finite-difference projection would be `d + 1` evaluations per step for a quality gain the
shotgun already buys.

## Rejected, and why — so nobody re-proposes them

- **Z3 `minimize` distance in the inner loop.** Fails contract 4 by orders of magnitude, and
  worse than latency: transcendentals are outside every decision procedure it has, and even
  polynomial constraints mostly come back `unknown` under `Optimize`. It has a job in the
  *tests* (below), not here.
- **Shrinking-box brute force** (blast a box around `x`, take the nearest hit, shrink the box
  to it, repeat). Two structural kills. It re-solves find-a-first-point on every call, which is
  the expensive step this whole module is organised around — a slab of width 1e-3 in a box of
  width 10 hits at 1e-4 per slab, two slabs and it never lands, and slabs are the primary case.
  And brute force cannot localise in high dimension: nearest-hit distance among `N` uniform
  samples scales as `N^(-1/d)`, so a million samples at `d = 50` land the nearest hit three
  quarters of the box away and the shrink does not shrink. Works at five dimensions with loose
  constraints, nowhere that matters.
- **Biasing hit-and-run chains toward `x`.** The shotgun in stage 3 is this idea done cheaply:
  a chain biased toward a target is a random-walk optimiser of distance, and one directed chord
  per anchor is the whole of what it would find. The "long thin tube between two regions" case
  is a census coverage problem, and the multi-seeded chains are the answer there; repair never
  cares.
- **Two residual channels per slab.** Babel lowers `a == b +/- t` to `max(a-b-t, b-a-t)`, and
  that kink sits at the slab centre, where COBYLA's interpolation points straddle it constantly;
  its linear model of a V goes flat, it steps out, the point comes back for repair. Real, and
  deliberately **not v1**: arity one is the control, it makes nlopt's one-closure-per-constraint
  question vanish, and the fix when the slab benchmark shows it is a sojourn-side change to
  `residual_count` with nothing new crossing the seam. The tell: repair count on points coming
  out of the constrained solve no lower than for the unconstrained one.
- **Smooth single-channel aggregates.** `Σ max(0, g_i)²` is C¹ with zero gradient on the
  boundary, so the linearisation is flat for a different reason. Log-sum-exp has to be
  sharpened to well below the slab width to not swallow the slab, which brings the kink back.

## Literature, for the record

The anchor-and-segment design is GENOCOP III (Michalewicz & Nazhiyath 1995): a set of feasible
reference points, a search point repaired along the segment toward one. They saw the clustering
problem too, which is the argument for *nearest* references over random ones. Stage 2 is hull
consistency from interval constraint programming (Benhamou et al., HC4). The radial decoder
(Koziel & Michalewicz 1999, homomorphous mappings) — mapping Artemis's cube *bijectively* onto
the region — is the alternative to repair, not a complement to it: needs no repair at all but
needs a star-shaped region and a centre. It becomes interesting only if the repair-distance trace
shows snapping collapsing the search along a curved boundary, which is the residual risk
Artemis's note already names.

## Work items

- [x] **Public `repair`** — as a function over `ConstraintSystem` and an anchor matrix, for the
      reasons above. `Driven by:` `tests/cvg_repair.rs`, all ten green. *2026-09-12: now
      `FeasibleRegion::repair`, the anchors still an argument; see the note under the API.
      2026-09-14: the anchors are gone; see the status note at the top of this section.*
- [x] **A public `worst_residual`.** Artemis's wrapper was recompiling every constraint to
      grade a point itself. `ConstraintSystem::worst_residual` is public as of 2026-09-12:
      `None` outside the box or on a fault, else the largest residual, `<= 0` when feasible.
- [ ] **`check` with the fault split.** Inf → `Infeasible(INFINITY)`, NaN → `Indeterminate`.
      `Driven by:` a fixture with `exp(x)` at the box edge that must rank infeasible, and one
      with `ln(x)` over a box crossing zero that must be indeterminate on the wrong side.
- [ ] **The violation rank.** `max_i g_i / s_i`, with `s_i` the spread of `g_i` over the census
      and the tolerance for a slab (range-scaling drowns exactly the constraints that bind
      hardest). Newtype over a validated `f64`, `Ord`, admits `+INFINITY`, never NaN.
      `Driven by:` a slab fixture where a point one tolerance-width outside outranks a point
      far outside a loose inequality.
- [x] **Seed passthrough.** `ConstraintSolver::with_seed(u64)`, public; `with_rng` stays hidden.
      `Driven by:` the polytope property test anchors on a seeded census.
- [x] **Fixtures.** Closed-form L1 projections for a half-space, a disc, a rotated slab with a
      driven coordinate; two disjoint bands where the nearer wins; a domain hole; two hundred
      bounds landed exactly; properties over a polytope (feasible by an independent
      evaluation, in box, fixed point, bitwise repeatable, never farther than the nearest
      anchor); the no-anchor cases both ways. **Not** a timing fixture: a debug-mode number is
      meaningless, and the 50- and 200-dimension figure belongs in the throughput ledger.
- [x] **`^` with a literal exponent is narrowed.** `x^2 + y^2 < 1` used to give `x` its whole
      box, so a repair against it fell to the chord and landed radially, a tenth farther in L1
      than the clamp; `sqr(x)` was inverted and landed exactly. The diagnosis above was wrong:
      narrowing never saw a `Pow`. `rewrite::expand_powers` had turned `x^2` into a product
      fold of two copies of `x` before any backend looked, and the fold had no inverse. The
      pass is retired. `x ^ n` for a whole `n` reaches every backend as written and each
      lowers it to multiplication itself — the tape (so the shader too), and `cvg::smtlib` —
      keyed on `Expr::whole_exponent`, and `interval` inverts the node through its root: even
      through `sqr`'s two-branch rule, odd as monotone, negative via the reciprocal.
      A compound base is now evaluated once rather than `n` times.
      `Driven by:` the `^` disc fixture in `cvg_repair.rs`, beside the `sqr` one.

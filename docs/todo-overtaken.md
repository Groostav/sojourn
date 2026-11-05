# Todos overtaken by events

Entries that were open in `todo.md` on 2026-09-16 and are moot: overtaken by the
synchronous `solve`, built in another form, or duplicated. Moved here verbatim, each
with a line on why, so `todo.md` lists only what is live. Nothing here is a plan.

## Overtaken by the synchronous `solve` (2026-09-16)

- [x] **Hanging is the worst failure mode; a `panic!` beats a spin.** Three hedges were
      considered on 2026-09-13 against bugs not yet written; none is adopted here, and the
      third is a discipline recorded in `AGENTS.md`.
      1. *Classify every loop as a budget or an invariant, and guard the invariants with a cap
         that panics.* Rejected: it depends on the author correctly deciding a loop is safe,
         which is exactly the judgement that fails when it fails.
      2. *A heartbeat the worker bumps in every loop and a watchdog on the handle side:*
         `take_within(Duration)`, a `Drop` that joins with a deadline and abandons the thread
         with an error-level trace when it passes (the Z3 leash's shape), and
         `Status::Failed("no progress for 30 s in <stage>")`. Converts unknown future hangs into
         a fact the caller can read, changes no result. Practical, not yet committed to: the
         `&& ticker.tick_and_check()` in every loop predicate may make the code structurally
         worse, and it is not clear the crate has a loop that needs it — every loop in the
         engine today is bounded by a named count or a finite set (`GAP_QUERIES`,
         `SHRINK_LIMIT`, `CLAMP_SWEEPS`, `CHORD_BITS`, `LANDING_LADDER`, `ADJUST_SWEEPS`, the
         starts and evaluations of the local solve, burn-in and thinning by dimension, the
         proposal budget; `classify::plan`'s ordering consumes a finite set; `narrow` is a
         single pass by design). The two places the failure mode actually lives are foreign
         code (Z3, leashed; COBYLA, budgeted per evaluation but not per iteration) and the
         handle waiting on the worker (`take` unbounded, `Drop` joins unconditionally — the
         burn-in that ignored the stop flag was found by exactly that).
      3. *Every foreign call and every API with a remote chance of combinatorial explosion gets
         a resource limit, or failing that a wall-clock ceiling set so conservatively that
         hitting it means a bug, and hitting it panics rather than waits.* Adopted as a
         discipline; the leash on Z3 was the model until Z3 left, and with it went the last
         wall-clock ceiling in the crate — every loop that remains is a count, branch-and-prune
         included (a contraction budget, a visit cap, a progress threshold). COBYLA's callback
         is the one place today that lacks a per-iteration bound.
      *Why moot:* The worker, its handle and `take_within` are gone: `solve` runs on the calling thread and returns when its count budgets are spent. The one discipline this entry kept — a `panic!` beats a spin — is in `AGENTS.md`.

- [x] **Concurrent coverage: start the walker from the first hit, cover gaps beside it.**
      Settled in discussion on 2026-09-12 as the follow-up to the budget above. Cases: (A)
      sampling carries — coverage is already skipped; (B) the solver finds the seed and the
      walker carries — today it waits for coverage, up to one limit's worth of Z3, before a
      point flows; (C) a second component exists that hit-and-run cannot reach — coverage is
      what finds it. Running coverage concurrently and merging a new component's seed by
      burning a chain in under the frozen preconditioner makes B pay nothing, and C gains its
      component later rather than never. The price is exact and worth writing down before it
      is paid: in case C the census becomes a function of *when* the seed arrived relative to
      the walker's batches, so the same seed gives different streams on different runs, which
      the two-run oracle in `cvg_benchmarks` and Artemis's same-seed test both rest on. In A
      and B the output is identical either way. To be chosen, not drifted into.

      The split that made it work: **one-shot and ongoing are different asynchronies.** "Crack the
      first point" has a completion and belongs to a `Future`; "keep filling between requests" has
      none and belongs to a worker. Trying to make one `Future` carry both is why it previously fit
      neither.
      Today's `ConstraintPool` became `Generator`, private to the worker and never shared, so there
      is nothing to lock. The public `ConstraintPool` is a handle: a `Receiver`, a buffer, and a
      stop flag. `std::sync::mpsc::sync_channel` supplies the rest for free — a bounded channel
      *is* the high-water mark, and `TryRecvError`'s `Empty`/`Disconnected` *is* the
      starved-versus-exhausted distinction that decides whether a caller waits or gives up.
      `generate` blocks until it has the count or the pool is exhausted, which cannot hang because
      exhaustion is detectable, and `status` covers the rest. `found()` and `try_generate` were
      both deleted: the first had no callers, and the second was speculative — letting every call
      block is simpler and the blocking is bounded anyway. `Status::Failed` carries the panic
      message rather than sitting beside a separate `failure()` accessor.
      *Why moot:* Nothing runs after `solve` returns, so there is nothing to run coverage concurrently with; coverage is part of the opening, and a design is walked from the region's burnt-in chains on demand.

- [x] **Suite runtime is 31 seconds**, nearly all of it `top_corner_200d` at 26 — which was
      already 23 before the worker landed, because burn-in for a 200-dimensional chain is genuinely
      expensive. The extra three seconds are read-ahead: `BATCH_SIZE * CHANNEL_CAPACITY` points get
      produced whether or not anybody asks for them, and at 200 dimensions a point is about four
      hundred walker moves. Both constants are deliberately small for that reason.
      Plus about four minutes on a cold build for Z3's C++, which caches.
      `.config/nextest.toml` sets a 60s slow-timeout with `terminate-after`, so a stuck test reports
      instead of stalling CI. The threshold has to clear the slowest *honest* test by a wide margin
      — an earlier 30s sat close enough to flag `top_corner_200d` as slow, which just teaches people
      to ignore the warning. Each emitted point costs about
      one feasibility evaluation per shrink, times thinning, and the distribution oracles each cost
      a whole extra solve. `TopCorner200D` alone is ~10s. Tolerable now; worth watching.
      *Why moot:* Read-ahead does not exist. The suite is ~45 s in debug, most of it the walker's burn-in on every `solve`; `tests/profiling.rs` has the release numbers.

- [x] **`Solution` wants an accessor.** Getting the pool out means writing
      `Solution::Satisfied(pool) | Solution::Unknown { pool, .. } => pool` at every call site. The
      Kotlin had a `Worthwhile` supertype over exactly those two cases; a `fn pool(&mut self)` or
      `fn into_pool(self)` is the Rust equivalent and removes the repetition.
      *Why moot:* `Solution` became `Result<FeasibleRegion, Infeasibility>`; `points()` and `witness()` are the accessors.

## Built, in another form

- [x] **A typed `SolveError`.** `solve` still returns `anyhow::Result`, which is
      right for a binary and loose for a library.
      *Why moot:* `ConstraintSolver::solve` returns `Result<FeasibleRegion, Infeasibility>`.

- [x] **Reverse-mode autodiff over the tape.** Roughly 100 lines once the tape exists, and probably
      worth more to the expensive-constraint / penalty-function work than raw throughput.
      *Why moot:* `src/eval/differentiate.rs`: reverse mode over the single-assignment tape, one rule per instruction; what Newton-KKT repair reads.

- [x] **GPU** via wgpu or CubeCL, once the batch tape shows the shape is right.
      *Why moot:* `src/cvg/sieve.rs`: wgpu, one compute shader per problem, candidates drawn on the device.

- [x] **`UNSAT` as a babel diagnostic, not just a sampling verdict.** Separate from everything
      above, and probably the higher-value half of wiring up a solver. A user who writes two
      constraints that cannot both hold currently gets silence — the pool searches, finds nothing,
      and reports having found nothing, which looks identical to a region that is merely hard. A
      solver's `check()` distinguishes "no point exists" from "we did not find one", and that is a
      *compile-time* answer: report it through `ProblemKind` alongside the syntax errors, where the
      user is already looking.
      **Most of the machinery now exists**: the emitter names its assertions, `Z3Backend` reads
      unsat cores back, and `Solution::Unsatisfiable` carries the conflicting constraints. What is
      missing is only the *entry point* — `compile()` sees one expression and no input variables, so
      reporting this through `ProblemKind` needs somewhere new for a caller to hand over a whole
      constraint set. Notably it needs no pool, no sampling and no strategy selection. It also
      applies to constraint sets that sample perfectly well, which is the case the
      "solver only when sampling is hard" framing misses entirely. Nothing in sojourn-CVG does
      this today; it is new work this repo is now positioned for.
      *Why moot:* `Infeasibility::Unsatisfiable` names the conflicting constraints when branch-and-prune proves the system empty, and `cvg_pools::contradictory_constraints_are_reported_as_unsatisfiable` pins it; Z3 is gone. What was not built is the framing — a compile-time answer with no solve — and it is not wanted: the proof is a contraction of the whole system, which is a `solve` with a prune budget.

- [x] **Translation is currently infallible, and its `Result` is a lie.** Every error path in the
      translator was an `unsupported` problem; with those gone there is no reachable `Err`, and
      `SemanticTranslator` has no fields left — a unit struct with `&self` methods.
      **Deliberately left alone.** The semantic checks above make it genuinely fallible again and
      give the struct real state back (it needs the source to build problems). Revisit once they
      land: if they do not materialise, collapse the `Result` and turn the methods into free
      functions.
      *Why moot:* the revisit happened by itself. `SemanticTranslator { source }` has its field back and two reachable `Err`s — a non-finite or non-positive tolerance on `a == b +/- t` (`src/frontend/parse.rs`) — so the `Result` is true again. All `pub(crate)`; nothing here was ever public.

- [x] **Split `Display` on the `{}` / `{:#}` boundary.** `Problem` always renders the full
      caret block, which is wrong for a log line. Plain `{}` should be the one-line summary and
      `{:#}` the block with source and caret.
      Note this does not recover Kotlin's `abbreviatedProblemText`, which elided lambda bodies
      (`sum(0/0,20,i->...)`) using the parse tree. `Display` has only source and span, so it
      renders `source[span]`. Whitespace collapsing is reachable; lambda elision is not.
      *Why moot:* built. `impl Display for Problem` (`src/diagnostics.rs`) renders the one-liner under `{}` and the caret block under `{:#}`, every wrapper forwards the flag, and `compile_errors::plain_display_is_a_single_line` / `alternate_display_renders_a_caret_block` pin both. The lambda-elision caveat stands.

- [x] **A BLAS evaluator over `faer` matrices**, taking a `MatRef` and never
      handing out a `MatMut`: no mutation of a matrix not allocated in the same
      lexical scope. Downstream of the tape, since it needs the flat form.
      *Why moot:* `CompiledExpression::eval` takes a `faer::MatRef` and runs the tape over it a tile at a time; nothing hands out a `MatMut`.

- [x] **General constant folding.** Only aggregate bounds fold today. Kotlin folded more broadly.
      An optimisation, not semantics.
      *Why moot:* `rewrite::fold_constants` folds every constant subtree bottom-up through the evaluator's own `apply`, `corpus.rs` pins the values, and `NonFiniteConstant` is its refusal path. "Only aggregate bounds" was true once.

## Gone with the Kotlin tree

- [x] **The JVM tree does not compile** on this branch, and the gap has widened.
      `db9add8` commented out four `locals [...]` declarations `rewriters.kt`
      needs; the grammar has since gained `scalarBlock` / `scalarReturnStatement`
      and pointed `lambdaExpr` at the first, which the Kotlin front end knows
      nothing about. Restoring it now means teaching `rewriters.kt` the new rules
      as well. Deleting the tree is looking like the honest answer — but capture
      the `jvm-11-map` ledger rows' provenance first, since they cannot be
      reproduced without it.
      *Why moot:* The tree was deleted in `1c26ed9` (see "Delete the Kotlin tree", ticked, under Tooling); the ledger rows keep their provenance note.

- [x] **Panama bindings** for the existing Java codebase. Mechanical, and the i64/f64 split will
      force changes on that side — but its model for variables is higher fidelity than "string", so
      it should bridge the gap without much trouble.
      *Why moot:* Will not fix: there is no JVM consumer. Artemis is Rust and links the crate.

- [x] **`Expression::evaluate` is slower than the JVM's, and it should not be.** 4680 against 9700
      points/ms on `x1 + x2`; the JVM wins the small cases outright and only loses once expressions
      get dear enough to hide the difference. The cause is not the evaluator, it is that the
      convenience wrapper builds a whole `Schema` per call — `Schema::new` clones a `String` for
      every name, so the 200-variable case allocates two hundred strings *per evaluation*, where
      the JVM merely hashes into a map it already has.
      Fixable without touching the evaluator: resolve symbols against the supplied pairs directly
      rather than constructing a `Schema` and binding. Worth doing, because this is the method
      whose name makes it the one a newcomer reaches for.
      *Why moot:* No such method exists: `compile` and a batch `eval` are the only path, the per-call `Schema` this described went with the convenience wrapper, and the JVM it was measured against is gone.

## Gone with the distribution benchmarks (`cvg_benchmarks`)

- [x] **Effective sample size is estimated, not exact.** `autocorrelation_time` uses Sokal's
      automatic windowing over the autocorrelation function. It has to, because emission is
      round-robin across chains and so the correlation sits at the chain count rather than at lag
      one — the conventional "truncate at the first non-positive lag" rule stops at lag one and
      reports full independence for a sequence that has none. It did exactly that, and briefly made
      a correlated sample look like grounds for suspecting the walker. A per-chain estimate would be
      exact, but the test cannot see chain boundaries; exposing them is a public-API question.
      *Why moot:* `autocorrelation_time` and the ten-seed family went with `cvg_benchmarks.rs` on 2026-09-16; a design is chosen farthest-first now, and the instrument to revive is the ESS one if a uniformity oracle ever returns.

- [x] **Nearest-neighbour clustering, or random projections, for a sample that
      spreads without filling.** `Case::occupancy` measures a joint grid over one
      or two coordinates, which is right at that size and does not scale: past
      about three coordinates the cell count runs away from the point count,
      every point lands in its own cell, and occupancy saturates at `n` for good
      and bad samples alike.

      A per-coordinate variant existed briefly, for a twenty-four dimensional
      row C case, and went when that row was refused rather than built. It was
      not *stronger* — both catch a handful of clumped seeds, and both are blind
      to a sample strung along a curve through the sheet, which is the Latin
      hypercube's own weakness seen from the other side. It was only cheaper.

      What would actually see a sample that spreads without filling is a
      nearest-neighbour count (`k` seeds make `k` clumps whatever the dimension;
      costs a radius) or KS on random projections (already written, in
      `cvg_benchmarks::assert_same_distribution`; costs a reference sample, which
      these regions cannot provide).
      `Driven by:` nothing yet, and deliberately — neither should be built until
      a case fails without it.
      *Why moot:* `Case::occupancy` is gone with the harness.

- [x] **Occupancy over an expression rather than a coordinate grid.** Geoff's
      idea, and it is the one that scales: collapse the space with a scalar
      function and measure occupancy of *its* histogram. A coordinate is the
      simplest such function, so this generalises what is there rather than
      replacing it, and being one-dimensional it is unchanged at 200 dimensions.
      A test writer picks something that varies over the region — `x1 + 3*x3` for
      the row-C sheet, `sum(xi)` for a high-dimensional band — and babel already
      compiles and evaluates arbitrary expressions, so the harness change is
      small.

      **Three things it must not be.** *Not the constraint's own residual*: on a
      tight equality the feasible set is a level set, so every feasible point has
      a residual in `[-t, 0]` and a whole sheet collapses to a `1e-9` interval —
      maximally uninformative, precisely here. (On an *inequality* the residual
      does vary usefully, which is the asymmetry that makes this tempting and
      wrong.) *Not a point check on the median*: "is there a point whose `f` is
      near the known median" is one bit, where the distribution of `f` is the
      real claim. *Not necessarily a derived CDF*: comparing against an
      analytically-known distribution is the strongest form and it is also
      circular for the regions we cannot sample — knowing `f`'s distribution over
      a region means knowing the region. Bin occupancy needs no CDF, because `n`
      seeds fill at most `n` bins whatever their values.

      **The blind spot, kept in view:** any one-dimensional collapse is satisfied
      by a sample that spreads in `f` while collapsing in `x`, the same way the
      diagonal defeats marginals. It complements the input-space instruments and
      does not dominate them.
      `Driven by:` nothing yet — it is a harness change, and the case that would
      justify it is the same high-dimensional row-C or row-E case above.
      *Why moot:* Same harness. The idea — collapse the space with a scalar babel expression and measure the histogram of *that* — is the one that scales, and is kept here for when a distribution oracle returns.

## Gone with Z3

- [x] **Z3 holes: report the `(^ x 617/500)` hang upstream.** Found 2026-09-11 while deciding
      how backends lower powers. SSCCE, through the crate's own binding with `rlimit` 100 000
      and `timeout` 5 000 set on the solver:
      `(declare-const x Real) (assert (= (^ x 1.234) 9.0)) (assert (> x 0.0))`.
      Parse 2 ms; `check()` 119 s before `unknown`; a first run with rlimit alone went eight
      minutes before it was killed. `(^ x 0.5)` and `(^ x y)` answer `unknown` in 30 ms, so it
      is the rational exponent — presumably the degree-617 encoding — in a loop that never
      polls the cancel flag. Both budgets are that one flag, so `Z3_interrupt` will not land
      either; the leash above is what covers it. Report with the SSCCE and offer a fix if it is
      a missing checkpoint. Where one such hole was found there will be more: this list is
      where they go, and `tests/torture_tests.rs` is where the crate proves it survives them.
      *Why moot:* Z3 is not a dependency any more; the SSCCE is preserved here for whoever does report it, but it is not this repository's bug to carry.

- [x] **The Z3 judge, tests only.** Taxicab distance is piecewise linear — one auxiliary per
      coordinate, two linear constraints each, minimise the sum — so `Optimize` can take it.
      When it answers `unknown`, fall back to the certificate form: assert the constraints and
      `|x - x0|_1 < r`, binary-search `r` on `unsat`. Needs only the satisfiability engine,
      which is the robust half. Seconds per fixture is fine. **The judge's norm must match the
      norm repair claims to be near in**, or the score is meaningless; L1 throughout.
      `Driven by:` nothing yet — this *is* test infrastructure.
      *Why moot:* No Z3, and the premise went too: `repair` lands in Euclidean distance over box-normalised coordinates, not taxicab, so an L1 judge would measure the wrong thing. The closed-form fixtures beside it are the judge.

## Gone with Z3 and the emitter, and with the distribution benchmarks (moved 2026-11-01)

Open in `todo.md` until 2026-11-01, whose premises left with the solver, the SMT emitter and
`cvg_benchmarks`. Moved verbatim when the crate was readied to stabilise.

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
      *Why moot:* The residue it measured was what the SMT emitter could not translate, and the emitter and Z3 are gone. The matching it asked for was built anyway, for driving rather than for a solver: `classify::plans`' maximum bipartite matching (2026-09-16).

- [ ] **A design of experiments over driven arguments.** Latin hypercube or
      Sobol over the argument expression's variables. A correctness issue, not a
      tuning one: pick one `x` and every point in the pool shares a `y`, which
      is a constant rather than a sample.
      The reasoning — why one `x` makes `y` a constant, not a sample — is in `todo-overtaken.md` under "Duplicates".
      *Why moot:* The pool it would have fanned out was the solver's proposals, which are gone. A driven coordinate is now a Gibbs draw from its band (`classify::retract`), so points no longer share a `y`, and a design is `FeasibleRegion::sample`, farthest-first over a walked pool.

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
      *Why moot:* `ConstraintSystem::adjusted` and the Z3 boundary witness that made it necessary are both gone; repair is `FeasibleRegion::repair`.

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
      *Built, in another form:* `FeasibleRegion::repair` projects by Newton on the KKT system over the active set (`cvg/newton.rs`), with gradients from the tape's reverse sweep (`eval/differentiate.rs`), and COBYLA where there is no derivative. The premise, a solver nominating a point `f64` rejects, went with Z3.

- [x] ~~**`take` should be `async`.**~~ Moot: `take`, the stream and the
      worker are gone (2026-09-16, below). `solve` is synchronous and
      `sample` is a design, not a stream.
- [x] ~~**`impl Stream for FeasibleSamples`**~~, likewise.
      *Why moot:* Already struck through in place on 2026-09-16; moved so `todo.md` lists only what is live.

- [ ] **A pipeline to hang these on.** Causalization splits one problem in two
      and the DOE turns one into `n`; both are `SolverProposal -> Vec<_>`, so a
      `flat_map` chain is the shape. Deliberately not designed yet — the trigger
      is the *second* fan-out pass.
      *Why moot:* `SolverProposal` is gone, and both fan-out passes it was waiting for (causalization, the DOE over driven arguments) went with the solver.

- [ ] **Capability metadata per backend.** The cheap half — `emit` taking a
      capability set rather than an implicit one — when a second backend exists.
      *Why moot:* `emit` is gone, and so is the second backend it was waiting for.

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
      *Why moot:* `cvg_benchmarks` was retired on 2026-09-16 (see "Gone with the distribution benchmarks"): a design is deliberately not uniform, so a KS verdict on walker output has no test to live in. Rotational preconditioning, which this implicated, landed 2026-09-11.

## Decided, and recorded as a decision

- [x] **The rest of it — desugaring in the *AST* — is refused.**
      **It depends on interval propagation above, and must not precede it.**

      `a == b +/- t` would become `And[a - b <= t, a - b >= -t]` — one
      constraint and not two, which keeps the one-to-one constraint-to-`cN`
      mapping an unsat core reads back through.

      **The blocker is `classify::shape`, and it is not incidental.** An
      equality does not merely *bound* a variable, it **determines** one:
      `x1 + x2 < 3` bounds `x1` where `x1 + x2 == 3 +/- t` computes it. That is
      a dependency claim no pair of inequalities makes, and it is what driving
      is built on. Recovering it from two `Compare` nodes means re-pairing them
      by structure — matching `lhs - rhs` with opposite-signed bounds — which
      `fold_constants` or `invert_monotone` can disturb on one side and not the
      other, enforced by nothing.

      A static test that would survive the desugaring was looked for and not
      found. "Is the root target interval bounded?" reads the same on both
      forms, but `Kind::And` already exists for `invert_monotone`'s domain
      guards, where `ln(x) < 2` becomes `And[x < e^2, x > 0]` and narrows `x` to
      a bounded `(0, e^2]` while determining nothing. **Bounded is not
      determined.**

      What was left after the eval half landed: `rewrite.rs` 14 sites of
      structural recursion that any node costs, `emit` 7, `classify` 3,
      `interval` 2, `parse` and `ast` 3.

      **The precondition for this was going to be retiring `Plan` and `Shape`,
      and that turned out to be wrong.** The argument was that a Gibbs sweep
      "updates one coordinate at a time and always reads current values, so the
      topological sort has nothing left to do". Reading current values *is* the
      failure. On `y == sin(x) +/- t` with `z == y + 1 +/- t`, conditioning `y`
      on the current `z` pins it within `t` of `z - 1`, and then `z` is pinned
      within `t` of the new `y`: the pair shuffles by `t` a sweep instead of
      travelling. Measured, while building `retract`'s intersection:
      **three occupied cells of eighty where twenty-four are wanted.**

      So `Plan` carries two things that per-coordinate conditioning cannot
      reconstruct — the evaluation order, and *which coordinates are not yet
      safe to condition on*. `ConstraintSystem::retract` now marks a driven coordinate
      settled only once it has been drawn, and skips any constraint naming an
      unsettled one. A pure Gibbs sweep cannot traverse a chain of tight
      equalities, and `plan`'s topological order has more to do rather than
      less.
      `Driven by:` `cvg_equalities::two_coupled_equalities_are_traversed`

      **What it would cost, done anyway.** `classify` sees two comparisons,
      `shape` answers `Opaque`, `plan` answers `None`, `retract` becomes a
      no-op, and the walker silently goes back to jittering beside a
      measure-zero surface — every taxonomy row regressing at once, detected
      only by a statistical coverage assertion that reports `0.0000%` without
      reporting why. `NearEq` is what makes "a band of half-width `t`"
      unforgeable, and it earns its keep until no consumer wants the pre-image.

      **The opposite direction — recognising a facing pair of inequalities and
      promoting it to an equality — was raised and dropped, and then measured.**
      `2.9999999 < x1 + x2 < 3.0000001` as two inequalities occupies **nine
      cells of forty** where the same band written as an equality occupies
      thirty-two: nothing is driven, because nothing is an equality, so the walk
      jitters inside a band 2e-7 wide. So the capability gap is real and the
      size of it is known.

      Still not built, and the reason is unchanged: it pays only for someone who
      wrote the pair *instead of* `==`, which babel's own grammar discourages.
      Kept as a number rather than a red test, because a red test for a feature
      nobody has asked for is noise on the bar. Revisit when a real formulation
      turns up written that way — the measurement is here to save re-deriving
      what it would buy.

      **The rule this is an instance of:** desugar when nothing downstream needs
      what was desugared — `sum` unrolls to arithmetic and no pass ever asks
      whether it was a fold — and keep the node when something does and
      reconstruction is a pattern a later pass can break. `var[i]` resolution
      was judged the same way: it was right because `classify` needed no edit.
      *Why moot:* A refusal, not a task. The safe half — `eval` desugars `a == b +/- t`, the AST keeps it — landed; the rest stays refused for the reason given, which `classify::shape` and the plans-per-branch work since then have only confirmed.

- [x] **The two rows below were planned on top of this and are now in doubt.
      Read the note under the desugaring entry before doing either.**
      *Why moot:* Both rows it warned about are ticked.

- [x] **Judge only what a move touched.** An axis move changes one coordinate;
      `Incidence::affected` names the constraints that read it, and the walker calls
      `is_feasible` over all of them. ~2× on the axis half of the judgements.
      *Why moot:* Not pursued. Its twin, "Find what the walker actually spends its time on", records that the restricted check was built, bought 3–6%, re-measured inside noise and removed on 2026-09-12. Performance here is dev ergonomics, and tuning an algorithm one does not yet fully understand without a compelling number is the wrong trade (2026-09-17).

- [x] **Batch the shrink loop.** Judge several draws along the chord through the SIMD tile
      at once and take the first feasible in order; where the "SIMD × cores" question (rayon
      for brute force, a parked global pool or a per-call one) would land.
      *Why moot:* Not pursued, for the same reason: no number says the shrink loop is where time goes, and the SIMD-times-cores question is not one this crate needs answered yet (2026-09-17).

## Duplicates

- [x] **The JVM tree does not compile on this branch**, so the Kotlin benchmark cannot be run in
      place. Commit `db9add8` ("POrting to rust") commented out four `locals [...]` declarations in
      `BabelParser.g4` — `availability`, `closedValue`, `value` — which `rewriters.kt` still
      depends on, giving nine unresolved references. Presumably the Rust ANTLR codegen would not
      accept them.
      The numbers above came from a throwaway `git worktree` at `db9add8^`, the last commit where
      it built; the worktree has been removed. `ThroughputBenchmarks.kt` is committed to the real
      tree and will run the moment the grammar is restored — or it can be deleted along with the
      rest of the JVM tree, which is already on this list.
      *Why moot:* The standing entry is *The JVM tree does not compile* under "Wave 3"; this was the older copy, kept for its detail on which grammar rules went and where the ledger rows came from.

- [x] **Integer-only exponents, and one rewrite to go with them.** Restricting `a ^ b` so `b` is
      integer-typed buys three separate things:
      *Evaluator speed* — measured, a single `^2` costs about what `sin`+`cos`+`sqrt`+`abs` costs,
      because `powf` is a libm call (`x1 + x2 > 20 - x3^2` at 9163 pts/ms against 9360).
      *SMT coverage* — `Pow` leaves the `untranslated` list entirely, since every integer power
      expands to multiplication.
      *One uniform rewrite* rather than a special case per backend.
      Rewrite straight to `Kind::Fold` rather than to a `prod` aggregate: `Fold` is already the
      post-unroll n-ary form both the emitter and evaluator consume, and going via `prod` means
      emitting a node whose only purpose is to be rewritten again — a fixed point you would then
      have to prove terminates. Same pass that already unrolls aggregates.
      *Why moot:* Duplicate of "Restricting `a ^ b` to an integer `b`" under Wave 3, which now points here for the three reasons.

- [x] **Causalization, for the terms no solver will take.** The trick is to stop asking the solver
      about a transcendental at all: if `y == sin(x) +/- t` and `y` appears nowhere else awkward,
      then `y` is *determined* — choose `x`, evaluate, done. `sin(sin(x))` is fine too, being still a
      function of `x`. What breaks it is a term constraining its own argument, `sin(x) == x/2`,
      where `x` is inside and outside and inversion is unavoidable.
      This is **causalization** in the Modelica sense and the algorithms are mature: bipartite
      **matching** of equations to variables, **BLT decomposition** (Tarjan SCC) for a dependency
      order, and **tearing** to shrink the algebraic loops that remain. Acyclic blocks evaluate;
      strongly-connected blocks need Newton. The solver is then only wanted for the SCCs, and only
      when the question is UNSAT rather than "give me a point".
      *Why moot:* Duplicate of "Causalization — the matching third only" under Wave 3.

- [x] **A piecewise sine belongs to the evaluator, not the emitter.** Table lookup with quadratic
      interpolation is `O(h^3)` error, SIMD-friendly, and much cheaper than libm — good for the
      tape. Choose coefficients by **Remez/minimax**, not Taylor: Taylor is optimal at a point,
      minimax across the interval. Bhaskara I's 7th-century rational approximation is the classic
      no-polynomial reference and is already good to ~0.0016.
      **Do not emit it to a solver.** A hundred pieces is a hundred-way `ite` split, and the modulo
      range reduction drags an integer variable in, pushing QF_NRA to QF_NIRA. Solvers tolerate
      degree far better than disjunction, so this would be worse than the Taylor series the JVM
      tried.
      *Why moot:* Duplicate of "A fast sine for the evaluator" under Parallel.

- [x] **Capability metadata per backend, eventually.** Which rewrites to apply depends on what the
      target can accept, and today that is hardcoded as "refuse the transcendentals". The cheap half
      is worth doing whenever the second backend appears: give `emit` a capability set rather than
      an implicit one, so the refusal becomes data. The expensive half — runtime-pluggable solvers
      with discoverable feature flags — should wait for a second backend to actually exist, since
      a plugin system with one plugin is a guess about the second.
      *Why moot:* Duplicate of "Capability metadata per backend" under Wave 3.

- [x] **4 — Causalization**, scoped by what 1–3 leave behind rather than by ambition. Details in the
      section above. The corpus residue after three steps is small and instructive: `y == sin(x)`
      and `y > sin(theta)` are feed-forward and yield to matching alone; `sin(x1) <= 0` is a
      periodic set, decomposable into intervals on a bounded domain but not by inversion; and
      `x1 > sin(ln(cos(2.1^x1)))` is implicit and will remain the thing nothing helps with. Build it
      when the residue is measured, not before — the shape of the leftovers should choose the
      algorithm.
      *Why moot:* Duplicate of "Causalization — the matching third only" under Wave 3, which has the measurement.

- [x] **5 — A design of experiments over driven arguments.** Once causalization says "choose `x`,
      then `y = sin(x)` follows", *how* `x` gets chosen is a real question and one point is the wrong
      answer. The driven variable is a deterministic function of its argument, so the distribution
      of `y` is entirely decided by the distribution of `x` — pick one `x` and every point in the
      pool shares a `y`, which is not a sample, it is a constant. What is wanted is a set of
      arguments spanning the feasible range, which is a space-filling design: Latin hypercube or
      Sobol over however many variables the argument expression contains, usually one.
      Worth flagging that this is not the sampler's job as currently written. The walker moves in
      the free variables and the driven ones are evaluated afterwards, so the design has to be over
      the *arguments*, and its quality shows up in `cvg_benchmarks` as the marginal of `y`.
      Good news: the oracles already there will measure it without modification.
      *Why moot:* Duplicate of "A design of experiments over driven arguments" under Wave 3, which now points here for the reasoning.

- [x] **6 — A fast sine, for the evaluator only.** Unchanged from the section above, with the
      accuracy question answered: **yes, fp32-ULP is comfortably achievable, and rather better.**
      SLEEF ships 1-ULP and 3.5-ULP variants of `sin` at `f64` using Cody-Waite argument reduction
      to `[-pi/2, pi/2]` and a nine-term polynomial; at `f32` four or five terms reach +/-1 ULP. The
      Intel hardware-table memory is real but points somewhere unhelpful: Tang and Story's IA-64
      work is *table-driven reduction followed by a polynomial*, around 0.6 ULP, and modern SIMD
      libms have mostly dropped the table because a multiply is cheaper than a cache miss. The x87
      `FSIN` instruction is the cautionary half of that story — microcoded, and it reduces against a
      66-bit pi, so near multiples of pi it is not approximating `sin x` at all. Intel documented
      its worst-case error as 1 ULP for years; the true figure is about 1.3 quintillion.
      So the tradeoff is not "fast or accurate". It is reduction quality against argument magnitude,
      and for constraint arguments in any sane range a short minimax polynomial is both faster than
      libm and accurate to the last bit or two. Objective functions can keep the exact path
      regardless; nothing here asks them to give up precision.
      *Why moot:* Duplicate of "A fast sine for the evaluator" under Parallel, which now points here for the accuracy notes.

- [x] **Not done: relevance-filtered parameters.** Planned as this wave's optional tail
      and skipped. `RuntimeProblem::parameters` carries the whole row where it could
      carry only the variables the failing subexpression reads — walk the program for the
      node matching `fault.span`, collect its `Kind::Global` ids, map them through
      `global_positions`. Error path only, so free on the happy path. `locals` is the
      harder half and needs a slot-to-name table the AST discards.
      *Why moot:* Duplicate of "Relevance-filtered parameters in a runtime error" under Wave 3.

- [x] **`RuntimeProblem.locals` ships empty.** Kotlin printed `local-variables{x=3.0}` — covering
      `var x = …` bindings as well as lambda parameters, since both lived in the same runtime heap.
      Filling it needs a slot-to-name table the AST deliberately discards. `parameters` is
      populated; this is the remaining half.
      *Why moot:* Duplicate of "Locals in a runtime error" under Wave 3.

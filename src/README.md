# One AST, two backends

Sojourn has two consumers with genuinely different needs, and the crate is laid out
to say so. In the middle is an `Ast`; on either side is a backend that takes it
somewhere.

```
                                   +-- eval --> CompiledExpression   runs a batch,
   source -->  frontend  -->  Ast -+                                 and its gradient
                                   +-- cvg  --> FeasibleRegion       searches for points
```

**`frontend` lowers as little as it can.** Everything it does is
meaning-preserving: it produces the canonical form of what the author wrote and
nothing more. A pass that makes the tree easier to *analyse* belongs there.

**`eval` lowers as hard as it can**, in the name of speed. It flattens the tree
to a three-address tape — its intermediate representation, `eval/tape.rs`: a
`VirtualTape` over virtual registers, which `eval/differentiate.rs` transforms
into the tape of the gradient by a reverse sweep, then an `AllocatedTape` with
the temporaries packed into registers — and runs it a tile of 256 samples at a
time, each instruction one kernel across the lanes. The kernels (`eval/simd.rs`) are explicit SIMD through `pulp`, with
AVX2 chosen at run time and a scalar backend otherwise; the operators that have
no vector form — libm, `%`, `pow`, rounding — run in kernels named `*_scalar`,
so a loop that is not vectorised says so in the code rather than being left to
the compiler. The type is opaque, so what runs the tape can change without
an API change.

**`cvg` keeps the tree open**, because its whole job is reading structure: which
variables another determines, which comparison can be inverted into a bound,
what interval an expression can take over a box.

neither backend's lowering is visible to the other**. 

## The front end

`syntax::raw_ast`, then `rewrite::canonicalize`: four passes in a fixed order, which
`parse` in [`frontend/mod.rs`](frontend/mod.rs) runs and reads top to bottom.
It is crate-private: a caller hands source text to `compile`, to
`compile_system` or to `ConstraintSystem::new`, and the tree between is
nobody's business but the two backends'. (`compile_system`, in
[`nodes.rs`](nodes.rs), is `compile` over a document: it parses every
expression, resolves subscripts against the inputs alone, reads the symbol
lists for the edges between expressions, and binds each to a row of its own
reads — the graph facts are the front end's symbol tables put side by side,
and the tapes are `eval`'s as ever.)

```
              fold_constants   check_subscripts   invert_monotone   unroll_aggregates   collect_powers
 source ─►  ──────────────►  ───────────────►  ──────────────►  ──────────────►  ─────────────►  Ast
        raw_ast (fallible)      (fallible)                          (fallible)
            └───────────────────────────── rewrite::canonicalize ─────────────────────────────┘
```

The output is an `Ast`, not an evaluable thing: turning one into something that
runs is `eval::bind`'s job (`compile` is parse then bind), and one directory over
is `cvg`, which never lowers it at all.

| phase | entry point | what it does |
|---|---|---|
| parse & lower | `syntax::raw_ast` | ANTLR parse tree to a `RawAst`; resolves names to `GlobalId`/`LocalSlot`, records `is_constraint` |
| fold constants | `rewrite::fold_constants` | every subtree made only of literals becomes one `Kind::Literal` |
| check subscripts | `rewrite::check_subscripts` | a subscript is integral by construction (`ast::is_integral`) or a compile error; nothing is rewritten |
| invert monotone | `rewrite::invert_monotone` | `f(u) op c` becomes `u op' c'` for the strictly monotone `f` |
| unroll aggregates | `rewrite::unroll_aggregates` | every `Kind::Aggregate` becomes `Kind::Fold`; a bound that is not a constant, or a span past the cap, is a compile error |
| collect powers | `rewrite::collect_powers` | a term multiplied by itself becomes `Pow(term, n)` — `f * f`, `f * (f * f)`, `f^2 * f^3`, `prod(1, n, i -> f)` — so a power has one form whatever was written; the one pass that may move a value by an ulp, since the unroll multiplies left to right whatever the spelling's grouping |

The order is not arbitrary. Folding runs first because it makes *"is this
constant?"* stop being a question anywhere else — afterwards a statically known
value **is** a `Kind::Literal`, which is why inversion and unrolling can both
pattern-match instead of carrying evaluators of their own. Inversion has to see
`Kind::Compare`, which it does, because nothing eliminates one any more.

Powers are decided in three places, deliberately. The canonical tree holds
`Pow(base, n)` for a whole `n`, whichever way it was spelled. The evaluator
alone wants multiplications — `powf` has no vector form and the shader's `pow`
is NaN for a negative base — so `eval::bind` runs `rewrite::unroll_powers` on
its own copy of the tree before lowering: `(t·t)·t`, with a compound base bound
to a fresh `let` so it is evaluated once. Interval narrowing wants the power
kept — `x·x·x` over intervals is wider than `x³`, and a chain of
multiplications inverts by dividing where a power inverts through its root — so
`cvg::hc4` emits the canonical tree as it stands. The emitter itself knows
nothing about powers; a `Pow` that reaches it is a real or variable exponent.
(An earlier tree-level unroll in `parse` was removed for duplicating a compound
base and for leaving nothing to invert; the `let` answers the first and applying
it only on the eval path the second.)

Three passes are fallible, and all refuse rather than defer:

- `fold_constants` rejects a constant subexpression that works out to NaN or an
  infinity — `sqrt(-1)`, `1/0`, and the literal `1.0e400`, which babel's grammar
  admits and `f64` cannot hold — and a literal subscript that is zero, negative
  or fractional.
- `check_subscripts` rejects a subscript the row decides unless it is a whole
  number *by construction* — `floor`/`ceil` of anything, an aggregate's
  parameter, exact integer arithmetic over those; the table is
  `ast::is_integral`, the crate's one type judgement. Every form in it is exact
  in `f64`, so the runtime has no rounding check: a subscript that reaches a
  gather is an integer, and the only fault left there is "past the end".
- `unroll_aggregates` rejects a bound that is not a constant, one that is a
  constant but not a usable index (`sum(1, 20/3, …)`), and an aggregate wider
  than its cap. `sum` is big-sigma over a fixed index set, not a loop; after
  this pass no aggregate exists, and neither backend knows one ever did.

## Nothing non-finite travels

One rule, enforced wherever it can be seen:

| phase   | where                                                                                          | catches                                                    |
|---------|------------------------------------------------------------------------------------------------|------------------------------------------------------------|
| compile | `rewrite::fold_constants` → `ProblemKind::NonFiniteConstant`                                   | what is provable: `sqrt(-1)`, `1/0`, `1.0e400`             |
| runtime | every checked instruction in `eval/tile.rs` and `eval/lane.rs` → `ProblemKind::NonFiniteValue` | the rest: `ln(x)` at `x = 0`, overflow, a non-finite input |

The runtime check is on **every instruction**, not only the operations that can
produce a non-finite value, so the error names the innermost subexpression that
went wrong rather than the whole constraint — instruction order is post-order,
so the first faulting instruction is the innermost node. It also catches a
non-finite *input* at its `Load`, and an unwritten local: the registers are
primed with NaN as a sentinel and a local the emitter cannot prove assigned gets
an explicit `Check`, so reading one is a slot-allocation bug rather than a value.

Infinities are included deliberately, and `rewrite::monotone` is why: while
`ln(0)` was allowed to evaluate to `-inf`, zero satisfied *any* upper bound, and
the inversion pass had to carry a domain floor of `u >= 0` where the mathematics
asks for `u > 0`. Refusing the infinity is what lets the guards be the textbook
ones. `sqrt` keeps its inclusive floor, and the asymmetry is now principled:
`sqrt(0)` is a finite answer, `ln(0)` is not an answer.

**Eager per lane, per instruction.** This is the CPU evaluator's contract, not
the language's. The batched executor fuses the finite test into each
instruction's loop as an or-reduction, so the happy path pays nothing, and it
records each lane's first fault rather than stopping; the lowest faulted column
is reported at the end with the innermost span, exactly as the tree-walker it
replaced did. A future GPU sieve is the one place this is allowed to be
*coarse*: check the output buffer, re-run an offending column through this
evaluator for the span, and do not "fix" the kernel to match.

The policy is the `is_finite` test on every checked instruction in
`eval/tile.rs` and `eval/lane.rs`. If a real use for a saturating infinity turns
up, that is where it changes.

## The one type

`ast::Kind` is a single enum spanning every phase. That is deliberate: it keeps
every pass a composable `Program -> Program`, which is what makes the rewriter
pluggable. A separate post-rewrite type would make every pass change types, and
each new pass would need converting on both sides.

`Kind::Compare`, `Kind::NearEq` and `Kind::And` reach both backends intact, and
each lowers them its own way.

The tree's shape is written once, in `Expr::children`, exhaustively; a new
`Kind` goes there or no traversal sees it. `Expr::iter_preorder` walks it —
parents first, source order — and is what a *query* uses: what a constraint
reads, whether a subscript appears, where a symbol is first referenced, each a
`filter`/`any`/`count` over the walk. A *pass* that builds a tree recurses,
because its recursion is the traversal state in the language's own syntax.

## The boolean convention belongs to `eval`

Babel has no boolean values at run time, so **the evaluator** turns a comparison
into arithmetic whose *sign* carries the truth value: `<= 0` is true. A violated
constraint then reports how badly it was violated rather than merely that it
was, which is the canonical `g(x) <= 0` form an optimizer wants.

Strictness rides on a nudge: `a < b` evaluates as `(a - b) + ε` with ε being
`f64::MIN_POSITIVE`, which vanishes into rounding at any real magnitude and
survives only when the difference is exactly zero — precisely where strict and
non-strict differ.

**This is one backend's convention, not the language's.** `cvg::interval`
shares none of it: a comparison is read as a target interval for the
difference, an equality as the band `[-t, t]`. The SMT emitter that existed
before it used to receive `(< (- 5.0 x) 0.0)` and have to *detect* a
three-hundred-digit denormal to recover the strictness, and an equality arrived
as `(<= (expr_max …) 0.0)` — an `ite` where a conjunction was meant. Both went
with the pass that caused them.

`Kind::And` exists for the same reason. `invert_monotone` needs a conjunction
for its domain guard — `ln(x) < 2` means `x < e²` **and** `x > 0` — and used to
build `max(residual, residual) <= 0` by hand, which is the residual convention
leaking into the front end. A variant it can emit without knowing costs one arm
per backend.

## What the evaluator is held to

`tests/corpus.rs` (every construct at an ordinary input, exact unless it routes
through libm), `tests/runtime_errors.rs` (where a fault lands) and
`tests/special_values.rs` (signed zeros to the bit, operations that go
non-finite, every fault kind planted in a batch). Hand-written expectations
only. The tree-walking evaluator that preceded the tape was used once as a
differential oracle over a few thousand random rows and then deleted; a
recorded-output file was considered and rejected, because once the walker is
gone such a file is only the tape agreeing with itself.

## Who consumes the result

- [`eval/`](eval) — `compile(&str, &[names])`, then
  `CompiledExpression::eval(MatRef)`. **One column per sample, one row per schema
  variable**, which is the shape `cvg` produces, so a generated batch is directly
  an input matrix with no transpose. There is no scalar entry point in the public
  API: the crate-internal `eval_row` exists because the walker is sequential by
  nature, and it is the same tape through the per-lane executor, not a second
  implementation.
- [`cvg/interval.rs`](cvg/interval.rs) — evaluates and narrows over intervals.
  What it cannot narrow through it answers `ENTIRE` to rather than guessing,
  which is most of why the two passes above exist: every constraint they
  rewrite is one an enclosure can then see.

## Files

| | |
|---|---|
| [`ast.rs`](ast.rs) | `Program`, `Block`, `Expr`, `Kind`, and the operator semantics in `UnaryOp::apply` / `BinaryOp::apply` |
| [`frontend/`](frontend) | text to `Ast`: `parse` and the `Ast` type in `mod.rs`, `syntax.rs` (the ANTLR walk, to a `RawAst`), the `rewrite.rs` passes, the ANTLR output |
| [`eval/`](eval) | `compile`, the tape (`tape.rs`, `irgen.rs` — IR generation, the tree walked once and instructions emitted, Clang's name for the step — `regalloc.rs`), `differentiate.rs` (the reverse sweep: a tape's gradient as a tape), its two CPU executors (`tile.rs`, `lane.rs`), and `wgsl.rs`, which turns a tape into the view that `templates/wgsl/` renders as a WGSL function for the GPU sieve |
| [`diagnostics.rs`](diagnostics.rs) | `ProblemKind`, spans, and rendering |
| [`generated.rs`](generated.rs) | ANTLR output, not hand-edited |
| [`../templates/wgsl/`](../templates/wgsl) | the WGSL, as askama templates: `operators.wgsl.jinja` (one macro arm per babel operator and its domain guard), `function.wgsl.jinja` (a tape as a function), `prelude.wgsl.jinja`, `harness.wgsl.jinja` (the sieve's entry points and bindings) |
| [`system.rs`](system.rs), [`solve.rs`](solve.rs), [`repair.rs`](repair.rs) | the generator's API: a validated set of constraints over a box, which answers whether a point is feasible and nothing harder; the solver builder and the solved region it returns, which hands out samples and repairs a point against its system; the repair algorithm |
| [`cvg/`](cvg) | the search engine, private: `progress.rs` (what the search has in hand, as a value), `sampling.rs` (probe, deliver, brute force), `local.rs` (a local solve for the first point, COBYLA), `walking.rs` (hit-and-run), `classify.rs`/`interval.rs`/`incidence.rs` (reading the constraints' structure), `support.rs` (each variable's *support*, the values it may take: an interval or a list of levels, and the level arithmetic every rung goes through), `hc4.rs` (HC4-revise over the constraint's tape — `IntervalTape`, the evaluator's IR run over intervals — and `slice`, which asks it of every constraint naming a coordinate), `prune.rs` (interval branch-and-prune: the proof, the blame, the pieces), `newton.rs` (the projection by Newton on the KKT system, for `repair`), `sieve.rs` (the GPU sieve, behind the `gpu` feature); `mod.rs` holds the ladder, the opening (`open`) and the design (`design`) |

# Sojourn

Write down a box and some constraints, and Sojourn finds points that satisfy them.
You get a witness, a space-filling design spread across the region, and the nearest
feasible point to any point you give it. Or you get a proof that no such point exists,
naming the constraints that conflict.

The constraints are written in babel, a small expression language: `x1 + x2 * cos(x3)^2`
is a transform, `x1 < x2 + x3` is a constraint, and `y == sqrt(x) +/- 1.0e-9` is an
equality with the tolerance it is held to. The crate is pure Rust, with no C toolchain
and no SMT solver. Source text goes in, and no syntax tree comes out.

```rust
use faer::Mat;
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = ConstraintSystem::new(
        vec![InputVariable::new("x", -2.0, 2.0), InputVariable::new("y", -2.0, 2.0)],
        ["x^2 + y^2 < 1", "x + y > 0.5"],
    )?;

    // The generator is yours: seed it, and the run is the same on every machine.
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
    let region = sojourn::solve(&system, &mut rng)?;

    // An optimizer stepped outside. Bring it back to the nearest feasible point,
    // a hair (1e-12 of the box) clear of every wall.
    let fixed = region.repair(&[3.0, 4.0], 1e-12)?;
    println!("{fixed:.3?}"); // [0.600, 0.800]: straight down the radius
    let fixed = region.repair(&[-1.0, -1.0], 1e-12)?;
    println!("{fixed:.3?}"); // [0.250, 0.250]: along the half-plane's normal

    // Six more points, spread away from that one and from each other.
    // One column per point, one row per variable.
    let existing = Mat::from_fn(2, 1, |row, _| fixed[row]);
    let design = region.sample(existing.as_ref(), 6, &mut rng)?;
    assert_eq!((design.nrows(), design.ncols()), (2, 6));
    Ok(())
}
```

Sojourn is consumed as a cargo git dependency; it is not on crates.io. Matrices are
[`faer`](https://crates.io/crates/faer)'s.

```toml
[dependencies]
sojourn = { git = "https://github.com/groostav/sojourn", tag = "v0.2.0" }
faer = "0.24"
```

## Things it does

### Proves a contradiction and names the constraints involved

```rust
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = ConstraintSystem::new(
        vec![InputVariable::new("x1", 0.0, 10.0), InputVariable::new("x2", 0.0, 10.0)],
        ["x1 < 3", "x2 > 5", "x1 + x2 < 20", "x1 > x2"], // that last one was meant to be `<`
    )?;
    let verdict = sojourn::solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(1)).unwrap_err();
    println!("{verdict}");
    // no point satisfies these constraints together: `x1 < 3`, `x2 > 5`, `x1 > x2`
    Ok(())
}
```

The answer takes tens of microseconds. It comes from a proof: interval contraction
emptied a coordinate, and the blame is the trace of that proof. `x1 + x2 < 20` is not
named because it narrowed nothing. When nothing can be proved, Sojourn says so. A
contradiction too thin for an enclosure to see (`x + y <= 1` against
`x + y >= 1.000000001`) spends its budget and comes back as `Infeasibility::NotFound`,
which reads *"no feasible point was found: nothing proved the region empty and sampling
found nothing"*. That is a different sentence and it sends you somewhere else, so it
is a different variant.

### Samples measure-zero sets

```rust
use faer::Mat;
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = ConstraintSystem::new(
        vec![InputVariable::new("x", -2.0, 2.0), InputVariable::new("y", -2.0, 2.0)],
        ["x * y == 0 +/- 1.0e-9"], // a cross a few billionths wide
    )?;
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(7);
    let region = sojourn::solve(&system, &mut rng)?;
    let design = region.sample(Mat::zeros(0, 0).as_ref(), 400, &mut rng)?;

    let on_arm = |axis: usize| (0..400).filter(|&c| design[(axis, c)].abs() < 1e-6).count();
    println!("{} on x = 0, {} on y = 0", on_arm(0), on_arm(1));
    // 200 on x = 0, 201 on y = 0 (the crossing is on both)
    Ok(())
}
```

Rejection sampling lands one point here in about two hundred million proposals. Sojourn
reads the equality first. `x * y == 0` can be solved for `x` and also for `y`, so there
is one *plan* per arm. Each point uses the plan that pins it hardest, which is the arm
it stands on, and the walker computes the driven coordinate by drawing from the
tolerance band instead of searching for it. At the crossing, the walker's chains split
across both arms. A variable named on both sides (`a == b + a/2`) cannot be driven, and
`ConstraintSystem::new` refuses it by name, giving the rearrangement (`a/2 - b == 0`).

### Scales to engineering problems

Vanderplaats' stepped cantilever has a width and a height for each of a hundred
segments: 200 variables and 201 constraints, the last of which couples all of them.

```rust,no_run
use faer::Mat;
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n = 100;
    let segment = 500.0 / n as f64;
    let mut variables: Vec<InputVariable> =
        (1..=n).map(|i| InputVariable::new(format!("b{i}"), 1.0, 5.0)).collect();
    variables.extend((1..=n).map(|i| InputVariable::new(format!("h{i}"), 5.0, 100.0)));

    let mut constraints = Vec::new();
    for i in 1..=n {
        let moment = 50_000.0 * (500.0 - (i - 1) as f64 * segment);
        constraints.push(format!("6 * {moment} / (b{i} * h{i}^2) < 14000")); // bending stress
        constraints.push(format!("h{i} < 20 * b{i}")); // aspect ratio
    }
    // Tip deflection: every segment's bending, summed. `var[i]` is b_i, `var[n + i]` is h_i.
    let limit = 2.54 * 2.0e7 / (50_000.0 * segment.powi(3));
    constraints.push(format!(
        "sum(1, {n}, i -> (12 * ({n} - i + 0.5)^2 + 1) / (var[i] * var[{n} + i]^3)) < {limit}"
    ));

    let system = ConstraintSystem::new(variables, constraints)?;
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(2026);
    let region = sojourn::solve(&system, &mut rng)?; // about 2 s
    let design = region.sample(Mat::zeros(0, 0).as_ref(), 10, &mut rng)?; // about 3 s more
    assert_eq!((design.nrows(), design.ncols()), (200, 10));
    Ok(())
}
```

Timings are from a release build on a laptop, and much of the solve is the walker's
burn-in. That cost is paid once, and every design afterwards skips it. `sum` and `prod`
are big-sigma over constant bounds, unrolled at compile time.

### Evaluates in batches, with gradients

The constraint engine runs on an evaluator you can use on its own:

```rust
use faer::Mat;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let compiled = sojourn::compile("x1 + x2 * cos(x3)^2", &["x1", "x2", "x3"])?;

    // One column per sample, one row per variable.
    let samples = Mat::from_fn(3, 4, |row, column| (row + column) as f64);
    let values = compiled.eval(samples.as_ref())?;

    // Reverse-mode over the same tape: one row per symbol, one column per sample.
    let gradient = compiled.gradient().expect("every operator here has a derivative");
    let jacobian = gradient.eval(samples.as_ref())?;
    assert_eq!(gradient.symbols(), ["x1", "x2", "x3"]);
    assert_eq!((jacobian.nrows(), values.nrows()), (3, 4));
    Ok(())
}
```

The expression is lowered to a three-address tape and run in explicit SIMD, with the
instruction set picked at run time. `x1 + x2 > 20 - x3^2` evaluates at about 175
million points a second on one thread in batches of 256
([`performance-records/`](performance-records)). A boolean evaluates to a residual
whose sign is its truth value: `<= 0` is true. A violated constraint therefore also
tells you how badly it is violated.

### Points at your mistakes

```rust
use faer::Mat;

fn main() {
    let failure = sojourn::compile("x1 + y * cos(x2)", &["x1", "x2"]).unwrap_err();
    println!("{failure:#}");
    // Error in 'y': unknown variable.
    // x1 + y * cos(x2)
    //      ~ no input variable by that name

    let root = sojourn::compile("sqrt(x1 - 5)", &["x1"]).unwrap();
    let failure = root.eval(Mat::from_fn(1, 1, |_, _| 3.0).as_ref()).unwrap_err();
    println!("{failure:#}");
    // Error in 'sqrt(x1 - 5)': this evaluated to something not a number.
    // sqrt(x1 - 5)
    // ~~~~~~~~~~~~ evaluates to NaN
    // local-variables{}
    // parameters{x1=3}
}
```

Nothing non-finite travels. NaN and infinity are compile errors where they can be
proved, and run-time errors otherwise, located at the innermost subexpression that
produced them.

### Compiles a document of named expressions

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let nodes = sojourn::compile_system(
        &["width", "height"], // inputs: the design vector
        &["drag"],            // externals: produced elsewhere, by a simulation say
        &[
            ("area", "width * height"),
            ("fits", "area < 10"),
            ("efficient", "drag / area < 3"),
        ],
    )?;
    for node in &nodes {
        println!("{:<9} cheap: {:<5} reads {:?}", node.name(), node.is_cheap(), node.reads());
    }
    // area      cheap: true  reads [Input("width"), Input("height")]
    // fits      cheap: true  reads [Output("area")]
    // efficient cheap: false reads [External("drag"), Output("area")]
    Ok(())
}
```

`fits` names only an output, but it is *cheap*: it can be decided from the inputs
alone, so a design that fails it can be rejected before the drag simulation runs.
Cycles, duplicates, unbound names and a constraint read as a number are all reported
in one pass, each under the name it belongs to. Sojourn hands back the edges and no
scheduler. [`tests/dependency_diagram.rs`](tests/dependency_diagram.rs) shows the
caller's side.

## The language

| | |
|---|---|
| arithmetic | `+` `-` `*` `/` `%` `^` |
| comparisons | `<` `<=` `>` `>=`, and `a == b +/- 0.01`: equality always carries its tolerance |
| functions | `sin` `cos` `tan` `cot` `asin` `acos` `atan` `sinh` `cosh` `tanh` `ln` `log` (base 10) `log(b, x)` `sqrt` `cbrt` `sqr` `cube` `abs` `sgn` `floor` `ceil` `max` `min` |
| constants | `pi`, `e` |
| aggregates | `sum(1, 20, i -> var[i]^2)`, `prod(...)`: the bounds are constants |
| subscripts | `var[i]` is the `i`th input, from 1; computed subscripts like `var[floor(x1)]` work too |
| statements | `var r = sqrt(x^2 + y^2); return r * cos(t)` |
| names | Unicode identifiers: `π`, `测试` and `☕` are all legal variable names |

## How a solve searches

`solve` climbs a ladder, and each rung spends only on what the rungs before it left
undecided:

1. **Contract.** HC4 interval propagation shrinks the box under every constraint. A
   coordinate that empties is a proof, with the constraints that emptied it as the blame.
2. **Probe.** A batch of uniform samples, which takes tens of microseconds. When enough
   of them land, that is the answer.
3. **Local solve.** COBYLA (pure Rust, via [`basin`](https://crates.io/crates/basin))
   from the box centre and a few seeded starts, stopping at the first feasible point.
4. **Bisect.** Split the contracted box and prune the halves. Each dead box is a proof
   that contraction alone missed. The boxes that survive are the separate pieces of
   the region, and each one seeds a chain.
5. **Brute force.** The uniform sampler on every core, for a billion proposals by
   default, or on the GPU behind the opt-in `gpu` feature. The GPU only screens
   candidates: every survivor is re-judged exactly on the CPU.

Every rung is bounded by a count (contractions, evaluations, proposals) and never by a
clock. So a run that does not return is a bug, and the same seed gives the same answer
on every machine whatever the thread count. Designs come from a hit-and-run walker
whose chains are burnt in at `solve`. Each one is a farthest-first selection, in
box-normalised Euclidean distance, from the region's points, a round of uniform
proposals and the walk. Every stage reports through
[`tracing`](https://crates.io/crates/tracing) at `debug`. A run that seems stuck is
located by its last log line.

### Why there is no Z3

Sojourn used to ask Z3 for a seed point and for proofs of infeasibility. Z3 was dropped
in September 2026 because it was too slow. Its stage alone cost 85 s on the ten-segment
beam in a debug build, where bisection now costs 13 s. It also spun on some queries
without being reliably interruptible, it cannot compute `ln 10`, and it needed a C++
build. Interval branch-and-prune replaced it. It is pure Rust, bounded by a count, and
it names the constraints in its proof. Two classes of contradiction are beyond it and
come back as `NotFound`: a *thin* one (`x + y <= 1` against `x + y >= 1 + 1e-9`) and an
*algebraic* one (`x*x - 2*x*y + y*y < 0`). [`docs/todo.md`](docs/todo.md) has the
measurements, under "The solver question, settled" and "Z3's fate".

## Building

Everything is a [`just`](https://github.com/casey/just) recipe, and CI runs exactly
`just ci`.

```text
just build          compile the crate and every test target
just test           run the test suite with nextest, then the doctests
just lint           rustfmt drift and clippy, warnings denied
just bench          release-mode throughput, written to performance-records/
```

The lexer and parser are generated from [`grammar/`](grammar) by `build.rs` at build
time. The GPU sieve is behind the `gpu` feature and is off by default.

## Reading further

- [`src/README.md`](src/README.md): the architecture, one AST and two backends.
- [`AGENTS.md`](AGENTS.md): conventions, layout, and the reading order for the design notes.
- [`docs/todo.md`](docs/todo.md): the roadmap and the reasoning behind the decisions.
- [`docs/compile-system.md`](docs/compile-system.md): the spec for `compile_system`.

## License

Apache-2.0. See [LICENSE](LICENSE). The constrained random vector generator descends from
the Apache-2 [sojourn-CVG](https://github.com/Groostav/sojourn-CVG) project; its original
notes are under [`docs/sojourn/`](docs/sojourn).

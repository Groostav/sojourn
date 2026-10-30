# Sojourn

> Prune with intervals, polish with Newton, and when all else fails, roll a billion dice.

Give Sojourn a box (bounds on each variable) and some constraints, and it finds points
that satisfy them: a first feasible point, a space-filling design across the feasible
region, and the nearest feasible point to any point you hand it. When the constraints
contradict each other it can often prove so, and names the ones that conflict.

Constraints are written in babel, a small expression language: `x1 + x2 * cos(x3)^2`
is a transform, `x1 < x2 + x3` is a constraint, and `y == sqrt(x) +/- 1.0e-9` is an
equality with the tolerance it must hold to. You pass source text and never build a
syntax tree. The crate is pure Rust, with no C toolchain.

Add it alongside [`faer`](https://crates.io/crates/faer), whose matrices the API takes
and returns:

```toml
[dependencies]
sojourn = "0.4"
faer = "0.24"
```

## What `solve` gives back

One of three things: a region to sample points from, a proof that there is none naming
the constraints that conflict, or an honest shrug when the search spent its budget and
could tell neither way.

### A region to sample

```rust
use faer::Mat;
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = ConstraintSystem::new(
        vec![InputVariable::new_continuous("x", -3.0, 3.0), InputVariable::new_continuous("y", -3.0, 3.0)],
        ["x^2 + y^2 < 4", "x^2 + y^2 > 1", "y > x - 1"], // a ring, with a bite out of it
    )?;
    // The generator is yours: seed it, and the run is the same on every machine.
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(3);
    let region = sojourn::solve(&system, &mut rng)?;

    // One column per point, one row per variable.
    let design = region.sample(Mat::zeros(0, 0).as_ref(), 6, &mut rng)?;
    for c in 0..design.ncols() {
        println!("({:.2}, {:.2})", design[(0, c)], design[(1, c)]);
    }
    // (1.10, 0.24)
    // (-1.99, -0.20)
    // (-0.55, 1.92)
    // (-0.23, -1.22)
    // (1.00, 1.70)
    // (-0.90, 0.53)
    Ok(())
}
```

A design is spread out on purpose: each point is picked from a pool of feasible
candidates to be far from the points already chosen, including any you pass in.
Carrying on with the same generator gives the next design.

### A proof, naming the constraints

```rust
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = ConstraintSystem::new(
        vec![InputVariable::new_continuous("x1", 0.0, 10.0), InputVariable::new_continuous("x2", 0.0, 10.0)],
        ["x1 < 3", "x2 > 5", "x1 + x2 < 20", "x1 > x2"], // that last one was meant to be `<`
    )?;
    let verdict = sojourn::solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(1)).unwrap_err();
    println!("{verdict}");
    // no point satisfies these constraints together: `x1 < 3`, `x2 > 5`, `x1 > x2`
    Ok(())
}
```

This takes tens of microseconds. Interval contraction emptied a coordinate, and the
constraints named are the ones that emptied it; `x1 + x2 < 20` narrowed nothing, so it
is not blamed.

### A shrug

```rust
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = ConstraintSystem::new(
        vec![InputVariable::new_continuous("x", 0.0, 1.0), InputVariable::new_continuous("y", 0.0, 1.0)],
        ["x + y <= 1", "x + y >= 1.000000001"], // a contradiction a billionth thick
    )?;
    let verdict = sojourn::solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(1)).unwrap_err();
    println!("{verdict}");
    // no feasible point was found: nothing proved the region empty and sampling found nothing
    Ok(())
}
```

Too thin for an interval enclosure to see, so the search spends its whole budget (a
billion proposals by default, under two seconds in release) and returns
`Infeasibility::NotFound`. That claims nothing: the region may be empty, or just too
small to hit. Algebraic contradictions such as `x*x - 2*x*y + y*y < 0` end here too.
`ConstraintSolver::with_proposal_budget` sets how long to keep trying.

## What else it does

### Repairs a point

```rust
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = ConstraintSystem::new(
        vec![InputVariable::new_continuous("x", -2.0, 2.0), InputVariable::new_continuous("y", -2.0, 2.0)],
        ["x^2 + y^2 < 1", "x + y > 0.5"],
    )?;
    let region = sojourn::solve(&system, &mut Xoshiro256PlusPlus::seed_from_u64(42))?;

    // An optimizer stepped outside. Bring it back to the nearest feasible point,
    // a hair (1e-12 of the box) clear of every wall.
    println!("{:.3?}", region.repair(&[3.0, 4.0], 1e-12)?); // [0.600, 0.800]: straight down the radius
    println!("{:.3?}", region.repair(&[-1.0, -1.0], 1e-12)?); // [0.250, 0.250]: along the half-plane's normal
    Ok(())
}
```

Nearest is Euclidean over the box-normalised coordinates, and a repair depends only on
the region, the point and the clearance: the same call lands in the same place.

### Walks along equalities

```rust
use faer::Mat;
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = ConstraintSystem::new(
        vec![InputVariable::new_continuous("x", -2.0, 2.0), InputVariable::new_continuous("y", -2.0, 2.0)],
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

Rejection sampling lands one point here in about two hundred million proposals.
Sojourn reads the equality first: `x * y == 0` can be solved for either variable, so
the walker computes the driven coordinate inside the tolerance band instead of
searching for it, on whichever arm the point stands. A variable named on both sides
(`a == b + a/2`) cannot be driven, and `ConstraintSystem::new` refuses it, suggesting
the rearrangement (`a/2 - b == 0`).

### Takes discrete variables

```rust
use faer::Mat;
use sojourn::rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use sojourn::{ConstraintSystem, Domain, InputVariable};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let system = ConstraintSystem::new(
        vec![
            InputVariable::new_discrete("bolts", 3.0, 1.0, 12.0), // 3, 4, ..., 12
            InputVariable::new("diameter", Domain::Levels(vec![6.0, 8.0, 10.0, 12.0, 16.0, 20.0])),
            InputVariable::new_continuous("load", 0.0, 3000.0),
        ],
        ["load == 0.6 * bolts * diameter^2 +/- 1", "load > 1000"],
    )?;
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(7);
    let region = sojourn::solve(&system, &mut rng)?;
    let design = region.sample(Mat::zeros(0, 0).as_ref(), 4, &mut rng)?;
    for c in 0..4 {
        println!("{} bolts of M{} carry {:.1}", design[(0, c)], design[(1, c)], design[(2, c)]);
    }
    // 9 bolts of M16 carry 1381.8
    // 12 bolts of M20 carry 2881.0
    // 5 bolts of M20 carry 1199.4
    // 12 bolts of M12 carry 1036.0

    let system = ConstraintSystem::new(
        vec![InputVariable::new_discrete("teeth", 10.0, 1.0, 40.0)],
        ["teeth > 22.2", "teeth < 22.8"],
    )?;
    println!("{}", sojourn::solve(&system, &mut rng).unwrap_err());
    // these constraints can be met continuously, at [22.654019658506105], but at no
    // combination of the declared levels: `teeth > 22.2`, `teeth < 22.8`
    Ok(())
}
```

A variable is continuous, a lattice (`lower, step, upper`), or a list of levels, and a
discrete one takes its levels and nothing between them. When the constraints could be
met between levels, the verdict is `Infeasibility::BetweenLevels` with a continuous
witness: the formula is fine, and the levels are what needs to change. If you map
points into a space of your own and back, `ConstraintSystem::snap` puts each discrete
coordinate back on its level before you judge it.

### Evaluates in batches, with gradients

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

The expression is lowered to a three-address tape and run in explicit SIMD:
`x1 + x2 > 20 - x3^2` evaluates at about 175 million points a second on one thread
([`performance-records/`](performance-records)). A comparison evaluates to a residual,
`<= 0` meaning true, so a violated constraint also says by how much.

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

NaN and infinity never travel: they are a compile error where that can be proved, and a
run-time error otherwise, pointing at the innermost subexpression that produced them.

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

`fits` names only an output, but it is *cheap*: it can be decided from the inputs alone,
so a design that fails it is rejected before the drag simulation runs. Sojourn returns
the dependency edges and leaves the scheduling to you;
[`tests/dependency_diagram.rs`](tests/dependency_diagram.rs) shows the caller's side.

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

1. **Contract.** Interval propagation shrinks the box under every constraint. A
   coordinate that empties is a proof, with the constraints that emptied it as the blame.
2. **Probe.** One batch of uniform samples, tens of microseconds. If enough land, done.
3. **Local solve.** COBYLA from the box centre and a few seeded starts, stopping at the
   first feasible point.
4. **Bisect.** Split the box and prune the halves. Each dead box is a proof contraction
   alone missed, and each surviving box seeds a chain of the walker that draws designs.
5. **Brute force.** Uniform sampling on every core, a billion proposals by default, or
   on the GPU behind the opt-in `gpu` feature. The GPU only screens candidates; every
   survivor is re-judged exactly on the CPU.

Every rung is bounded by a count, never a clock, so the same seed gives the same answer
on any machine at any thread count, and a run that does not return is a bug. On
Vanderplaats' stepped cantilever, 200 variables and 201 constraints, `solve` takes
about 2 s in release and a ten-point design about 3 s more. Every stage logs through
[`tracing`](https://crates.io/crates/tracing) at `debug`.

## Building

Everything is a [`just`](https://github.com/casey/just) recipe, and CI runs exactly
`just ci`.

```text
just build          compile the crate and every test target
just test           run the test suite with nextest, then the doctests
just lint           rustfmt drift and clippy, warnings denied
just bench          release-mode throughput, written to performance-records/
```

`build.rs` generates the lexer and parser from [`grammar/`](grammar).

## Reading further

- [`src/README.md`](src/README.md): the architecture, one AST and two backends.
- [`AGENTS.md`](AGENTS.md): conventions, layout, and the reading order for the design notes.
- [`docs/todo.md`](docs/todo.md): the roadmap and the reasoning behind the decisions.
- [`docs/compile-system.md`](docs/compile-system.md): the spec for `compile_system`.

## License

Apache-2.0. See [LICENSE](LICENSE). The constrained random vector generator descends from
the Apache-2 [sojourn-CVG](https://github.com/Groostav/sojourn-CVG) project; its original
notes are under [`docs/sojourn/`](docs/sojourn).

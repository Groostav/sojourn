//! SSCCE: `repair` lands a point on a vertex with residuals of -1e-15, one rounding error inside.
//!
//! Found by Artemis's `e03-spring-3` benchmark on 2026-09-11. Drop into `tests/` of the sojourn
//! crate (branch `v0-artemis`) and run `cargo nextest run --test repair_lands_too_close_at_a_vertex`.
//!
//! The tension/compression spring (Arora 1989 via Coello 2000): `d` wire diameter, `D` coil
//! diameter, `N` active coils, four constraints. Its optimum is a VERTEX where the deflection
//! constraint and the shear-stress constraint are both active. `repair` of a point just outside
//! that vertex comes back with both residuals at about `-1e-15`: feasible, but by less than the
//! rounding of a single arithmetic operation on the coordinates.
//!
//! Artemis stores points normalized to `[-1, 1]` per coordinate and hands the evaluator the
//! denormalized value: `x -> lo + (((x - lo) / (hi - lo) * 2 - 1) + 1) / 2 * (hi - lo)`. That
//! round trip is exact for most values and one ulp off for some. One ulp on `d` moves the two
//! residuals by about `1e-8` (`d` appears as `d^4` in a term of order `1e5`), which is seven
//! orders of magnitude more than the margin `repair` left. So the point Artemis evaluates is
//! infeasible by its own oracle, and its wrapper has to walk the point back in.
//!
//! The contract (Artemis's design note, contract 1) asks for *strictly* feasible with "a final
//! deliberate step inward". A margin of a few ulps of the coordinate is not that step at a
//! vertex: it has to be measured in the residual, not in the coordinate, or it has to be large
//! enough that any one-ulp perturbation of the point stays feasible.
//!
//! Observed (first test): `repair` of the vertex point returns it with the shear residual at
//! exactly -2.2e-16 -- one ulp inside -- and the frame round trip puts it at +2.9e-15.
//! Expected: a margin that survives one-ulp perturbation of the coordinates. The second test
//! shows the margin IS sufficient one step away from the vertex (it passes today); the defect is
//! specific to the landing where two constraints are active at once.

use faer::Mat;
use sojourn::{compile, repair, ConstraintSolver, ConstraintSystem, InputVariable, Satisfiability};

const NAMES: [&str; 3] = ["d", "D", "N"];
const LO: [f64; 3] = [0.05, 0.25, 2.0];
const HI: [f64; 3] = [2.0, 1.3, 15.0];
const CONSTRAINTS: [&str; 4] = [
    "1 - (D^3) * N / (71785 * (d^4)) < 0",
    "(4 * (D^2) - d * D) / (12566 * (D * (d^3) - (d^4))) + 1 / (5108 * (d^2)) - 1 < 0",
    "1 - 140.45 * d / ((D^2) * N) < 0",
    "(D + d) / 1.5 - 1 < 0",
];

fn system() -> ConstraintSystem {
    let vars = NAMES.iter().zip(LO).zip(HI).map(|((n, lo), hi)| InputVariable::new_continuous(*n, lo, hi)).collect();
    ConstraintSystem::new(vars, CONSTRAINTS.iter().map(|s| s.to_string())).expect("the spring binds")
}

/// Every constraint's residual at `x`, through the public evaluator (the same numbers the
/// user's evaluator would see).
fn residuals(x: &[f64]) -> Vec<f64> {
    let sample = Mat::from_fn(3, 1, |i, _| x[i]);
    CONSTRAINTS.iter().map(|src| compile(src, &NAMES).unwrap().eval(sample.as_ref()).unwrap()[0]).collect()
}

/// Artemis's frame round trip, per coordinate.
fn round_trip(x: &[f64]) -> Vec<f64> {
    (0..3).map(|i| {
        let n = (x[i] - LO[i]) / (HI[i] - LO[i]) * 2.0 - 1.0;
        LO[i] + (n + 1.0) / 2.0 * (HI[i] - LO[i])
    }).collect()
}

async fn anchors() -> Mat<f64> {
    match ConstraintSolver::new().with_seed(0x50_50_1E_5E_ED).solve(system()).await.unwrap() {
        Satisfiability::Satisfied { mut samples } => samples.take(256),
        Satisfiability::Unsatisfiable { because } => panic!("{because:?}"),
    }
}

/// The point Artemis's wrapper received from `repair` on 2026-09-11 (its own output, passed back
/// in) is a fixed point of `repair` with both binding residuals at -1e-15, and it does not
/// survive Artemis's frame round trip.
#[pollster::test]
async fn the_spring_vertex_is_landed_one_rounding_error_inside() {
    let system = system();
    let anchors = anchors().await;
    let landed = [0.052986411203565915_f64, 0.38873876446629, 9.632040910614];
    let back = repair(&system, anchors.as_ref(), &landed).expect("repairable");
    let g = residuals(&back);
    eprintln!("repair(vertex)  = {back:?}");
    eprintln!("residuals       = {g:?}");
    let rt = round_trip(&back);
    let g_rt = residuals(&rt);
    eprintln!("after round trip= {rt:?}");
    eprintln!("residuals       = {g_rt:?}");
    assert!(g.iter().all(|v| *v <= 0.0), "repair's own output is not feasible: {g:?}");
    assert!(
        g_rt.iter().all(|v| *v <= 0.0),
        "repair's output is feasible by {:.1e} but infeasible after a one-ulp frame round trip ({:.1e})",
        g.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        g_rt.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
    );
}

/// The general statement, which passes today away from the vertex and is here so a fix for the
/// first test does not break it: from a point just outside, `repair` lands somewhere that any
/// one-ulp move of any coordinate leaves feasible.
#[pollster::test]
async fn a_repaired_point_should_survive_a_one_ulp_perturbation() {
    let system = system();
    let anchors = anchors().await;
    // Just outside the vertex: a thicker wire violates the deflection constraint (c1, which
    // grows with d^4 in the denominator) while the others stay satisfied.
    let outside = [0.0529 * 1.02, 0.3887, 9.632];
    let g0 = residuals(&outside);
    assert!(g0.iter().any(|v| *v > 0.0), "the starting point should be infeasible: {g0:?}");
    let landed = repair(&system, anchors.as_ref(), &outside).expect("repairable");
    let g = residuals(&landed);
    eprintln!("landed    = {landed:?}\nresiduals = {g:?}");
    assert!(g.iter().all(|v| *v <= 0.0));
    for i in 0..3 {
        for dir in [-1, 1] {
            let mut p = landed.clone();
            p[i] = if dir < 0 { p[i].next_down() } else { p[i].next_up() };
            let gp = residuals(&p);
            let worst = gp.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            assert!(
                worst <= 0.0,
                "one ulp {} on {} makes the landed point infeasible by {worst:.1e}; repair's margin is {:.1e}",
                if dir < 0 { "down" } else { "up" }, NAMES[i],
                g.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            );
        }
    }
}

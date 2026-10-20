//! SSCCE: `FeasibleRegion::repair` lands off the nearest feasible point wherever the clamp can
//! land -- by `repair.rs`'s own account, the clamp is an axis (L1) projection and runs first.
//!
//! Written from Artemis 0.13.2 (2026-09-15) against sojourn `587199c` (repair by projection),
//! after `c06-rosenbrock-50-slab` came out worse at every concurrency level and `c05`/`c02`/`c08`
//! (balls) did not move. Copy into sojourn's `tests/` and run with nextest. Three shapes whose
//! L2-nearest feasible point has a closed form, and a table per shape of where repair lands:
//!
//! | shape | proposal | landing (measured on `587199c`) |
//! |---|---|---|
//! | `sum x_i^2 == 1 +/- 1e-4`, `[0,1]^20` | anywhere | exact radial projection, ratio 1.000 |
//! | `(x1-3)^2 + (x2-3)^2 < 2.25`, `[-10,10]^50` | 3-8 units outside | exact, ratio 1.000 |
//! | same disc | 0.01-0.3 outside (the GWO step-over case) | 1.03-1.54x farther; up to 0.32 off the nearest point on a radius-1.5 circle |
//! | `x1 == x2 + 1 +/- 0.01`, `[-10,10]^50` | every gap, 0.02 to 5 | sqrt(2)x farther, every time: `x1` moves the whole gap instead of `x1`, `x2` half each |
//!
//! So the *projection* stage is the L2-nearest point where it runs (the shell, the far disc), and
//! the *clamp* stage -- which runs first and wins whenever one coordinate can reach -- lands the
//! L1-nearest point. `repair.rs` says that is the design ("the L1 projection onto a half-space
//! moves the single coordinate with the steepest normal component"), and `857cf91` clamped first
//! too, so this is not what changed between them. It is recorded because the consumer's question
//! is L2: a GWO wolf that steps 0.01 over a curved wall wants to be put back where it stepped
//! from, and an L1 landing walks it along the wall instead. Whether that is sojourn's contract to
//! change or Artemis's to live with is the open question; the assertions here are L2's.
//!
//! Expected (L2): every landing feasible with the clearance and no farther from the proposal than
//! the closed-form nearest point. Actual: the shell test passes; the disc near-miss and the slab
//! tests fail with the ratios above.

use rand::{RngExt, SeedableRng};
use sojourn::{ConstraintSolver, ConstraintSystem, InputVariable, RepairError};

const CLEARANCE: f64 = 1e-12;

fn l2(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt()
}

fn system(n: usize, lo: f64, hi: f64, cs: &[&str]) -> ConstraintSystem {
    let vars = (1..=n).map(|i| InputVariable::new_continuous(format!("x{i}"), lo, hi)).collect();
    ConstraintSystem::new(vars, cs.iter().map(|s| s.to_string())).unwrap()
}

fn region(system: &ConstraintSystem) -> sojourn::FeasibleRegion {
    let mut r = pollster::block_on(ConstraintSolver::new().with_seed(7).solve(system)).unwrap();
    assert!(r.take(8).ncols() == 8);
    r
}

/// For each proposal: the landing must be feasible with the clearance, and no farther from the
/// proposal than the closed-form nearest feasible point (a 1e-6 relative allowance for the
/// clearance and the solve's tolerance). Prints every landing first so a failure reads as a
/// table, not one number.
fn probe(name: &str, system: &ConstraintSystem, proposals: &[Vec<f64>], nearest: impl Fn(&[f64]) -> Vec<f64>) {
    let region = region(system);
    println!("== {name}");
    let mut worst = 0.0f64;
    let mut failures = Vec::new();
    for (k, p) in proposals.iter().enumerate() {
        let q = nearest(p);
        assert!(system.is_feasible(&q, 0.0), "closed-form nearest is not feasible: {q:?}");
        let d_star = l2(p, &q);
        match region.repair(p, CLEARANCE) {
            Ok(r) => {
                let d = l2(p, &r);
                let off = l2(&r, &q);
                let ratio = d / d_star;
                worst = worst.max(ratio);
                let feasible = system.is_feasible(&r, CLEARANCE);
                println!("  #{k:2} |p-nearest| {d_star:9.4}  |p-repair| {d:9.4}  ratio {ratio:8.3}  |repair-nearest| {off:9.4}  feasible(clr) {feasible}");
                if !feasible { failures.push(format!("#{k}: landed without the clearance")); }
                if ratio > 1.0 + 1e-6 { failures.push(format!("#{k}: landed {ratio:.3}x farther than the nearest feasible point")); }
            }
            Err(RepairError::Stranded) => { println!("  #{k:2} |p-nearest| {d_star:9.4}  STRANDED"); failures.push(format!("#{k}: Stranded")); }
            Err(RepairError::Cramped { .. }) => { println!("  #{k:2} |p-nearest| {d_star:9.4}  CRAMPED"); failures.push(format!("#{k}: Cramped")); }
        }
    }
    println!("  worst ratio {worst:.3}");
    assert!(failures.is_empty(), "{name}: {}", failures.join("; "));
}

#[test]
fn a_ball_near_miss_lands_off_the_radial_projection() {
    // c05: (x1-3)^2 + (x2-3)^2 < 2.25 over [-10,10]^50. Nearest: radial onto the circle r=1.5 about (3,3).
    let sys = system(50, -10.0, 10.0, &["(x1 - 3)^2 + (x2 - 3)^2 < 2.25"]);
    let mut rng = rand::rngs::SmallRng::seed_from_u64(1);
    let mut props = Vec::new();
    for scale in [0.01, 0.3, 3.0, 8.0] {
        for _ in 0..3 {
            let mut p: Vec<f64> = (0..50).map(|_| rng.random::<f64>() * ((10.0) - (-10.0)) + (-10.0)).collect();
            let th: f64 = rng.random::<f64>() * ((std::f64::consts::TAU) - (0.0)) + (0.0);
            p[0] = 3.0 + (1.5 + scale) * th.cos();
            p[1] = 3.0 + (1.5 + scale) * th.sin();
            p[0] = p[0].clamp(-10.0, 10.0); p[1] = p[1].clamp(-10.0, 10.0);
            props.push(p);
        }
    }
    probe("ball-50", &sys, &props, |p| {
        let (dx, dy) = (p[0] - 3.0, p[1] - 3.0);
        let r = (dx * dx + dy * dy).sqrt();
        let s = (1.5 - 1e-9) / r;
        let mut q = p.to_vec(); q[0] = 3.0 + dx * s; q[1] = 3.0 + dy * s; q
    });
}

#[test]
fn a_slab_landing_moves_one_coordinate_instead_of_two() {
    // c06: x1 == x2 + 1 +/- 0.01 over [-10,10]^50. Nearest: shift x1,x2 symmetrically onto the nearer face.
    let sys = system(50, -10.0, 10.0, &["x1 == x2 + 1 +/- 0.01"]);
    let mut rng = rand::rngs::SmallRng::seed_from_u64(2);
    let mut props = Vec::new();
    for gap in [0.02, 0.1, 1.0, 5.0, -0.02, -1.0, -5.0] {
        for _ in 0..2 {
            let mut p: Vec<f64> = (0..50).map(|_| rng.random::<f64>() * ((9.0) - (-9.0)) + (-9.0)).collect();
            // set x1 so that x1 - x2 - 1 = gap
            p[0] = p[1] + 1.0 + gap;
            if p[0].abs() > 10.0 { p[1] = 0.0; p[0] = 1.0 + gap; }
            props.push(p);
        }
    }
    probe("slab-50", &sys, &props, |p| {
        let d = p[0] - p[1] - 1.0;
        let excess = d.abs() - (0.01 - 1e-9);
        let mut q = p.to_vec();
        if excess > 0.0 { let sh = excess / 2.0 * d.signum(); q[0] -= sh; q[1] += sh; }
        q
    });
}

#[test]
fn a_sphere_shell_landing_is_the_radial_projection() {
    // c12: sum x_i^2 == 1 +/- 1e-4 over [0,1]^20. Nearest: radial scaling onto the nearer face.
    let sys = system(20, 0.0, 1.0, &["sum(1, 20, i -> (var[i])^2) == 1 +/- 0.0001"]);
    let mut rng = rand::rngs::SmallRng::seed_from_u64(3);
    let mut props: Vec<Vec<f64>> = (0..6).map(|_| (0..20).map(|_| rng.random::<f64>() * ((1.0) - (0.0)) + (0.0)).collect()).collect();
    props.push(vec![1.0; 20]);
    props.push(vec![0.5; 20]);
    props.push((0..20).map(|i| 0.05 + 0.04 * i as f64).collect());
    probe("shell-20", &sys, &props, |p| {
        let r2: f64 = p.iter().map(|x| x * x).sum();
        let target = if r2 > 1.0 { (1.0f64 + 1e-4 - 1e-10).sqrt() } else { (1.0f64 - 1e-4 + 1e-10).sqrt() };
        let s = target / r2.sqrt();
        p.iter().map(|x| x * s).collect()
    });
}

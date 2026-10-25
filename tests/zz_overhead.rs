use faer::Mat;
use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sojourn::{ConstraintSolver, ConstraintSystem, InputVariable};
use std::time::{Duration, Instant};

fn median(mut runs: Vec<Duration>) -> Duration {
    runs.sort();
    runs[runs.len() / 2]
}

#[test]
fn zz_overhead() {
    let thin = ConstraintSystem::new(
        vec![
            InputVariable::new_continuous("x", 0.0, 1.0),
            InputVariable::new_continuous("y", 0.0, 1.0),
        ],
        ["x + y <= 1", "x + y >= 1.000000001"],
    )
    .unwrap();
    let brute = median(
        (0..5)
            .map(|seed| {
                let t = Instant::now();
                let _ = ConstraintSolver::new()
                    .with_proposal_budget(100_000_000)
                    .solve(&thin, &mut Xoshiro256PlusPlus::seed_from_u64(seed));
                t.elapsed()
            })
            .collect(),
    );

    let names: Vec<String> = (1..=20).map(|i| format!("x{i}")).collect();
    let simplex = ConstraintSystem::new(
        names
            .iter()
            .map(|n| InputVariable::new_continuous(n.clone(), 0.0, 1.0))
            .collect(),
        ["sum(1, 20, i -> var[i]) <= 1".to_owned()],
    )
    .unwrap();
    let region = ConstraintSolver::new()
        .solve(&simplex, &mut Xoshiro256PlusPlus::seed_from_u64(1))
        .unwrap();
    let walk = median(
        (0..5)
            .map(|seed| {
                let t = Instant::now();
                let _ = region.sample(
                    Mat::<f64>::zeros(0, 0).as_ref(),
                    200,
                    &mut Xoshiro256PlusPlus::seed_from_u64(seed),
                );
                t.elapsed()
            })
            .collect(),
    );

    let n = 80;
    let mut vars: Vec<InputVariable> = (1..=n)
        .map(|i| InputVariable::new_continuous(format!("x{i}"), 0.0, 1.0))
        .collect();
    for j in 1..=3 {
        vars.push(InputVariable::new_discrete(format!("k{j}"), 0.0, 1.0, 9.0));
    }
    let sources: Vec<String> = (1..=n)
        .map(|i| {
            format!(
                "x{i} + 0.1 * k{} <= 1.2 - 0.001 * x{}",
                1 + i % 3,
                1 + i % n
            )
        })
        .collect();
    let coupled = ConstraintSystem::new(vars, sources.iter().map(String::as_str)).unwrap();
    let region = ConstraintSolver::new()
        .solve(&coupled, &mut Xoshiro256PlusPlus::seed_from_u64(1))
        .unwrap();
    let point: Vec<f64> = (0..n + 3).map(|i| if i < n { 1.15 } else { 9.0 }).collect();
    let repair = median(
        (0..5)
            .map(|_| {
                let t = Instant::now();
                for _ in 0..50 {
                    let _ = region.repair(&point, 0.0);
                }
                t.elapsed()
            })
            .collect(),
    );
    println!("OVERHEAD brute {brute:?} walk {walk:?} repair {repair:?}");
}

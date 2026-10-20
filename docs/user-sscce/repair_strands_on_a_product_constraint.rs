//! SSCCE: `FeasibleRegion::repair` answers `Stranded` on Keane's product constraint where a
//! feasible point is a few units away.
//!
//! Found by Artemis `c10-keane-bump-50` and `c11-keane-bump-100` at 0.13.2 (2026-09-15), sojourn
//! `587199c` (repair by projection, no anchors). Copy into sojourn's `tests/` and run with
//! nextest. Both runs aborted at concurrency 1 (one seed's proposal is enough to end a level);
//! the two points below are the proposals Artemis handed to repair, verbatim.
//!
//! The system is Keane's bump's: `x in [0, 10]^n`, `0.75 - prod x_i < 0`, `sum x_i - 7.5 n < 0`.
//! The feasible fraction of the box is ~1 -- a uniform point is feasible with overwhelming
//! probability -- and the census has no trouble. The proposals are GWO/RBF points with several
//! coordinates driven to the lower wall plus the clearance (`1e-11 = 1e-12 * 10`): the product
//! is `4.6e-59` at `n = 50` and `3.5e-44` at `n = 100`, against the `0.75` needed.
//!
//! What a feasible point nearby looks like: at `n = 50` the 43 coordinates above `1e-3` already
//! multiply to `1.08e3`, so lifting the seven small ones to `0.36` each (L2 move 0.94) is
//! feasible; lifting every coordinate below 1 to 1 (27 of them, L2 move 4.6) is feasible at
//! either size. `v0.1-artemis` landed these by chording to a census anchor, which is always
//! feasible here.
//!
//! Expected: `Ok(point)` with `prod >= 0.75` (with the clearance), no farther than the lifted
//! point. Actual: `Err(Stranded)`. The hypothesis: the per-coordinate clamp finds every interval
//! empty (with the other coordinates' product at `1e-50`, no single `x_i` in `[0, 10]` can
//! reach 0.75), and the projection -- COBYLA on `min ||u - p||^2` from `p` -- sits where the
//! product's gradient is `~1e-50` in every direction and exhausts its budget without ever
//! seeing the constraint move.

use sojourn::{ConstraintSolver, ConstraintSystem, InputVariable, RepairError};

const CLEARANCE: f64 = 1e-12;

const POINT_50: [f64; 50] = [
    0.08928918738349267, 0.8128352441524971, 9.423980250530082, 8.201514214434269e-7, 0.074313852199003, 9.787239247939075,
    1.4211922856333103e-6, 0.1897163472654313, 9.84180144604797, 9.365967211219166, 0.050729876983561795, 9.964434055086267,
    9.99999999999, 9.998420069515479, 0.6367844193595209, 1.000177718424311e-11, 0.2817004031539234, 0.08928918738349179,
    0.04463556293675364, 9.888842446816353, 9.755546509597457, 9.993325488268553, 0.04070536976125716, 9.982578066498466,
    6.753114992849506, 9.916261792957172, 9.917970006505211, 9.84180144604797, 9.965996110813085, 0.04224343643094741,
    0.08928918738349179, 0.0892891873834909, 9.995418246406189, 1.000177718424311e-11, 0.45955550570587533, 0.07810019738970642,
    3.6437449990600612e-6, 9.84180144604797, 0.09112711432073706, 9.992216303186801, 9.5554706945567, 1.000177718424311e-11,
    0.021898415600998256, 0.047301883444006876, 10.0, 1.000177718424311e-11, 0.0575386197917398, 9.980275812968333,
    9.994656793123873, 0.21906571038074585,
];

const POINT_100: [f64; 100] = [
    0.17254578921850872, 9.847544237268336, 2.0521462694660904, 9.998476568313237, 9.76509275017996, 9.850646770930963,
    9.996393506044793, 10.0, 0.3205469515262518, 0.22019689702190348, 0.10910633798575908, 1.6150612543341936e-5,
    3.7116243034791196, 7.321527629920714, 9.728315006404275, 0.20150607661669007, 10.0, 0.17801234505628738,
    9.781963578457491, 0.02295628842870201, 0.04463556293675364, 9.989393779363073, 2.963614537019205, 9.829712337107315,
    9.94178651080093, 9.99750417081147, 0.09340781041695045, 0.04242684147396236, 9.849243124942896, 0.09418976995315997,
    3.3114549281361843, 10.0, 9.991962886896662, 0.018675786031829844, 0.04574268681044913, 0.06537103995314375,
    9.885891910534452, 9.97120509364014, 0.0739859783351573, 0.10072017225057994, 0.09771396100312746, 9.921515396325125,
    0.0044404944124520895, 1.000177718424311e-11, 9.955405564200245, 0.1907643650899571, 0.1166567682530637, 9.675787351863239,
    0.13972615304147418, 9.888097649349508, 0.16431233484910468, 9.971398133058544, 0.15458082940541118, 0.019965876786665504,
    9.30586180378567, 9.98422754460475, 9.924204242569346, 5.251523720772866e-6, 0.07149581405804017, 0.04524056575319069,
    9.97258594389993, 0.10404989602332915, 1.000177718424311e-11, 0.003899453529749408, 10.0, 0.2214747694598156,
    9.999977057168755, 0.023295827303008387, 0.33796421096362383, 0.25489057481690747, 0.03942431403784141, 9.966128451036527,
    3.386902292969065, 9.619027967766208, 0.03019840151393094, 0.022291509229087403, 0.10109105531713958, 9.925950983729551,
    9.893692068337426, 0.15361721576050336, 0.09293107743228646, 0.03816301885874829, 0.06348329020397259, 9.794511175116297,
    0.13889958852751594, 0.014363407752428614, 0.04166180538376718, 0.02120532449144985, 9.965819606548731, 0.22915812910300737,
    9.99999999999, 9.987478647541776, 9.777482049114209, 0.11126635161483467, 9.730111214688964, 2.8933292614219397e-5,
    9.956858866428668, 9.727571143799182, 9.718735982172214, 9.9831122879468,
];

fn keane(n: usize) -> ConstraintSystem {
    let vars = (1..=n).map(|i| InputVariable::new_continuous(format!("x{i}"), 0.0, 10.0)).collect();
    ConstraintSystem::new(vars, [
        format!("0.75 - prod(1, {n}, i -> var[i]) < 0"),
        format!("sum(1, {n}, i -> var[i]) - {} < 0", 7.5 * n as f64),
    ])
    .unwrap()
}

fn l2(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt()
}

async fn stranded_where_a_lift_is_feasible(n: usize, point: &[f64]) {
    let system = keane(n);
    let mut region = ConstraintSolver::new().with_seed(0x50_50_1E_5E_ED).solve(&system).await.unwrap();
    assert_eq!(region.take(256).ncols(), 256, "the census itself is fine here");

    // A feasible point within a few units: every coordinate below 1 lifted to 1 (and the ones
    // sitting exactly on the upper wall pulled in, so the clearance has room there too).
    let lifted: Vec<f64> = point.iter().map(|&x| x.max(1.0).min(10.0 - 1e-9)).collect();
    assert!(system.is_feasible(&lifted, CLEARANCE), "the lifted point should be plainly feasible");
    let reach = l2(point, &lifted);

    match region.repair(point, CLEARANCE) {
        Ok(p) => {
            assert!(system.is_feasible(&p, CLEARANCE), "landed without the clearance: {p:?}");
            assert!(l2(point, &p) <= reach + 1e-9, "landed farther than the lifted point ({} > {reach})", l2(point, &p));
        }
        Err(RepairError::Cramped { nearest, .. }) => {
            panic!("Cramped on a region that is ~all of the box; nearest = {nearest:?}");
        }
        Err(RepairError::Stranded) => {
            panic!("Stranded at n = {n}, with a feasible point {reach:.2} away (every coordinate below 1 lifted to 1)");
        }
    }
}

#[pollster::test]
async fn keane_50_proposal_is_repaired() {
    stranded_where_a_lift_is_feasible(50, &POINT_50).await;
}

#[pollster::test]
async fn keane_100_proposal_is_repaired() {
    stranded_where_a_lift_is_feasible(100, &POINT_100).await;
}

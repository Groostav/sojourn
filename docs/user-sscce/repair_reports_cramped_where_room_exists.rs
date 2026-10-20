//! SSCCE: `FeasibleRegion::repair` answers `Cramped` on a slab with a thousand clearances of room.
//!
//! Found by Artemis `c06-rosenbrock-50-slab` at 0.11.2 (2026-09-14), sojourn `v0.1-artemis`
//! (857cf91). Copy into sojourn's `tests/` and run with nextest.
//!
//! The slab `x1 == x2 + 1 +/- 0.01` over the box `[-10, 10]^50`. The point below is the GWO
//! corner Artemis handed to repair, verbatim: `x1 = x2 = 10` (off the slab, and on the box wall),
//! `x15 = 10` (on the box wall), the other 47 coordinates interior and free. With clearance
//! `1e-12` (box widths) repair returns `Err(Cramped { nearest, .. })`, whose message says the
//! feasible room is narrower than the clearance. It is not: at `x1 = 10 - 2e-11` the slab allows
//! `x2` anywhere in `[8.99, 9.01]`, and `nearest` itself -- `(9.99999999998, 9.00999999996, ...)`
//! -- is feasible and sits at the slab's upper edge. The same two variables alone, without the
//! other 48, repair fine to `(9.99999999982, 9.0099999998)`.
//!
//! Expected: `Ok(point)` with `x1 - x2 - 1` strictly inside `(-0.01, 0.01)` by the clearance and
//! every coordinate `2e-11` inside the box.
//! Actual: `Err(Cramped)`. The hypothesis is that the clamp lands on the slab's *edge* (`x2` is
//! clamped downward to the first feasible value, the upper wall of the slab) and the clearance
//! check then fails at that edge, while the shotgun toward the eight nearest anchors -- all of
//! which have the clearance -- is not reached or not credited.

use sojourn::{ConstraintSolver, ConstraintSystem, InputVariable, RepairError};

const CLEARANCE: f64 = 1e-12;

const POINT: [f64; 50] = [
    10.0, 10.0, 0.037934591907416576, -1.4526100941029996, -2.3893010265475714, 1.361931567811031,
    4.11689052283946, 4.494252410345063, -2.005518602274471, 4.1188793056627, -5.95116686164018,
    2.492842031275674, -1.3234596891714148, 3.347236617051133, 10.0, -1.1891807306029345,
    -1.516681144741728, 4.431115884399351, -0.5874735872910958, -3.930237559691323, -6.366025452051,
    -2.4629519762105225, 0.8073114586925813, -5.0137779123063, -1.644508933427449, -1.1598311363591725,
    -4.302614890261344, -0.49317672900232257, -1.256139640975735, 3.234262566994011, 4.187426934577074,
    -0.5465312579226764, -1.5543093106643513, 2.393501938545735, -0.44097708303743244, 1.2391729285165716,
    1.5688822265030176, -1.578121285453353, -6.8546281947342855, -3.864870815187305, 7.000175360769164,
    -0.9191642874867916, 1.1534444997183808, -4.166591487377443, 1.0350269446341147, -4.887948004425278,
    -0.30389172137436576, -7.919489276088157, 6.2641372922625385, -8.73885329592165,
];

fn system() -> ConstraintSystem {
    let vars = (1..=50).map(|i| InputVariable::new_continuous(format!("x{i}"), -10.0, 10.0)).collect();
    ConstraintSystem::new(vars, ["x1 == x2 + 1 +/- 0.01"]).unwrap()
}

#[pollster::test]
async fn repair_lands_on_a_slab_with_room_to_spare() {
    let system = system();
    let mut region = ConstraintSolver::new().with_seed(0x50_50_1E_5E_ED).solve(&system).await.unwrap();
    let anchors = region.take(256);
    assert_eq!(anchors.ncols(), 256);

    let verdict = region.repair(anchors.as_ref(), &POINT, CLEARANCE);
    match verdict {
        Ok(p) => {
            assert!(system.is_feasible(&p, CLEARANCE), "landed without the clearance: {p:?}");
            let gap = p[0] - p[1] - 1.0;
            assert!(gap.abs() < 0.01, "not inside the slab: {gap}");
        }
        Err(RepairError::Cramped { nearest, clearance }) => {
            // The refusal is wrong: `nearest` is plainly feasible and the slab is 0.02 wide here.
            assert!(system.is_feasible(&nearest, 0.0), "nearest is not even plainly feasible");
            panic!(
                "repair answered Cramped (clearance {clearance}) on a slab 0.02 wide; nearest = ({}, {}, ...)",
                nearest[0], nearest[1]
            );
        }
        Err(RepairError::Stranded) => panic!("Stranded, with 256 feasible anchors"),
    }
}

/// The same two variables alone repair fine: the other 48 -- one of them on the box wall -- are
/// what turns the answer into `Cramped`.
#[pollster::test]
async fn the_two_slab_variables_alone_repair_fine() {
    let system = ConstraintSystem::new(
        vec![InputVariable::new_continuous("x1", -10.0, 10.0), InputVariable::new_continuous("x2", -10.0, 10.0)],
        ["x1 == x2 + 1 +/- 0.01"],
    )
    .unwrap();
    let mut region = ConstraintSolver::new().with_seed(0x50_50_1E_5E_ED).solve(&system).await.unwrap();
    let anchors = region.take(64);
    let p = region.repair(anchors.as_ref(), &[10.0, 10.0], CLEARANCE).unwrap();
    assert!(system.is_feasible(&p, CLEARANCE), "{p:?}");
}

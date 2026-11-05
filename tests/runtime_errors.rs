//! Run-time diagnostics, ported from `BabelRuntimeErrorFixture.kt`.
//!
//! As with the compile-time fixture, only structured fields are asserted; the
//! Kotlin tests compared rendered messages that also embedded the local-variable
//! and parameter maps.
//!
//! One Kotlin case is deliberately **not** ported: `when running with an
//! un-ordered hashmap as globals should eagerly throw`. That test guards a
//! failure mode created by the JVM API taking a `Map<String, Double>` whose
//! iteration order might be arbitrary. [`sojourn::Schema`] is an ordered
//! `Vec<String>` by construction, so the failure mode does not exist here.

mod common;

use sojourn::diagnostics::{EvaluationFailure, ProblemKind, Span};

#[test]
fn dynamic_index_out_of_bounds() {
    // The literal subscripts the aggregate unrolls into are bound like names
    // and must exist; the miss is the computed `var[floor(x2)]`, which only a row can
    // reveal.
    let expr = "sum(1, 2, i -> var[i] + var[floor(x2)] + i) + var[floor(x2)]";
    let err = common::eval_one(expr, &[("x1", 3.0), ("x2", 4.0)])
        .expect_err("var[4] with only 2 parameters should fail");

    match err {
        EvaluationFailure::Runtime(p) => assert_eq!(
            p.problem.kind,
            ProblemKind::DynamicIndexOutOfBounds {
                requested_1index: 4,
                available: 2
            }
        ),
        other => panic!("expected a runtime problem, got {other:?}"),
    }
}

#[test]
fn missing_statically_referenced_symbol_is_reported_at_bind_time() {
    // The JVM implementation re-checked this on every evaluate(); here it is a
    // property of the binding, so it surfaces once — and with a span, like
    // any other compile-time problem.
    let failure = sojourn::compile("x1 + x2", &["x1"]).expect_err("binding without x2 should fail");

    assert_eq!(failure.problems.len(), 1, "{:#?}", failure.problems);
    let problem = &failure.problems[0];
    assert_eq!(
        problem.kind,
        ProblemKind::Unbound {
            name: "x2".to_owned()
        }
    );
    assert_eq!(problem.span, Span::new(5, 7));
    assert_eq!(problem.column_idx, 5);
}
/// The other side of the rule: a non-finite value handed *in* is caught at the
/// variable rather than travelling into the arithmetic.
#[test]
fn a_non_finite_input_is_refused() {
    let expr = "x1 + x2";
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let err = common::eval_one(expr, &[("x1", bad), ("x2", 1.0)])
            .expect_err("a non-finite input should be refused");
        match err {
            EvaluationFailure::Runtime(p) => assert!(
                matches!(p.problem.kind, ProblemKind::NonFiniteValue { .. }),
                "got {:?} for {bad}",
                p.problem.kind
            ),
            other => panic!("expected a runtime problem, got {other:?}"),
        }
    }
}

/// The case the whole decision turns on. `ln(0)` is negative infinity, and
/// letting it travel is what forced `rewrite::monotone` to carry a domain floor
/// of `u >= 0` where the mathematics asks for `u > 0`.
#[test]
fn a_logarithm_of_zero_is_refused() {
    let err = common::eval_one("ln(x1)", &[("x1", 0.0)]).expect_err("ln(0) should be refused");

    match err {
        EvaluationFailure::Runtime(p) => assert_eq!(
            p.problem.kind,
            ProblemKind::NonFiniteValue {
                value: f64::NEG_INFINITY
            }
        ),
        other => panic!("expected a runtime problem, got {other:?}"),
    }
}

/// A fault in the conclusion fails the implication **even where the premise
/// is false**, which is not what the logic says: `x > 0 implies x * ln(x) < 1`
/// should hold vacuously at `x = -1`. (Not `ln(x) < 1`, which
/// `rewrite::invert_monotone` turns into a bound on `x` with no logarithm
/// left to fault.) The tape evaluates every instruction
/// and a fault ends the row, so the guard a reader sees is not one the
/// evaluator honours; guard the domain in the conclusion itself. Pinned so the
/// day it changes is a deliberate one: `docs/todo.md`, "A faulting conclusion
/// under a false premise".
#[test]
fn when_a_conclusion_faults_under_a_false_premise_should_still_be_refused() {
    // act
    let err = common::eval_one("x1 > 0 implies x1 * ln(x1) < 1", &[("x1", -1.0)])
        .expect_err("faults are eager, premise or not");

    // assert
    match err {
        EvaluationFailure::Runtime(p) => assert!(
            matches!(p.problem.kind, ProblemKind::NonFiniteValue { value } if value.is_nan()),
            "{:?}",
            p.problem.kind
        ),
        other => panic!("expected a runtime problem, got {other:?}"),
    }
}

/// Overflow is a non-finite value like any other, and the case most likely to
/// be a nuisance rather than a service. Pinned so that if it ever needs
/// relaxing, a failing test says where the policy lives.
#[test]
fn overflow_is_refused() {
    let err =
        common::eval_one("x1 * x1", &[("x1", 1e200)]).expect_err("overflow should be refused");

    match err {
        EvaluationFailure::Runtime(p) => {
            assert!(matches!(p.problem.kind, ProblemKind::NonFiniteValue { .. }))
        }
        other => panic!("expected a runtime problem, got {other:?}"),
    }
}

//! `compile` over [`Variables`]: inputs, which `var[i]` indexes, and
//! intermediates, which bind by name only.
//!
//! An intermediate is a name an expression may use that is not part of the
//! design vector — a simulation's result, another expression's output — and
//! that may not exist yet when the inputs do. So a row carries every input,
//! always and in declaration order, which is what keeps `var[5]` the same
//! input whatever else the expression names; and it carries an intermediate
//! only where the expression names it, in the order the caller listed them.
//! A plain list is every name an input, as `compile` has always read it.

use faer::Mat;
use sojourn::diagnostics::{CompilationFailure, EvaluationFailure, ProblemKind, Span};
use sojourn::{CompiledExpression, Variables};

const INPUTS: [&str; 3] = ["x1", "x2", "x3"];
const INTERMEDIATES: [&str; 3] = ["ext", "f1", "f2"];

fn classified(source: &str) -> CompiledExpression {
    sojourn::compile(source, Variables::classified(&INPUTS, &INTERMEDIATES))
        .unwrap_or_else(|failure| panic!("expected {source:?} to compile:\n{failure}"))
}

fn refused(source: &str) -> CompilationFailure {
    match sojourn::compile(source, Variables::classified(&INPUTS, &INTERMEDIATES)) {
        Ok(compiled) => panic!(
            "expected {source:?} to fail, got rows {:?}",
            compiled.rows()
        ),
        Err(failure) => failure,
    }
}

fn column(values: &[f64]) -> Mat<f64> {
    Mat::from_fn(values.len(), 1, |row, _| values[row])
}

// ---- rows ----

#[test]
fn when_given_a_plain_list_should_take_every_name_as_a_row_in_order() {
    // setup
    let variables = ["x1", "x2", "x3"];

    // act
    let compiled = sojourn::compile("x2", &variables).expect("binds");

    // assert
    assert_eq!(compiled.rows(), ["x1", "x2", "x3"]);
}

#[test]
fn when_classified_should_take_every_input_then_the_named_intermediates_in_listed_order() {
    // setup
    let source = "f2 * ext + x2";

    // act
    let compiled = classified(source);

    // assert
    assert_eq!(compiled.rows(), ["x1", "x2", "x3", "ext", "f2"]);
}

#[test]
fn when_classified_should_need_no_row_for_an_intermediate_it_does_not_name() {
    // setup
    let compiled = classified("x1 + f1");
    let rows = column(&[2.0, 0.0, 0.0, 10.0]);

    // act
    let values = compiled.eval(rows.as_ref()).expect("evaluates");

    // assert
    assert_eq!(compiled.rows(), ["x1", "x2", "x3", "f1"]);
    assert_eq!(values[0], 12.0);
}

// ---- subscripts index the inputs, and only the inputs ----

#[test]
fn when_a_literal_subscript_names_an_input_should_read_that_input() {
    // setup
    let compiled = classified("var[2] * f1");
    let rows = column(&[0.0, 3.0, 0.0, 5.0]);

    // act
    let values = compiled.eval(rows.as_ref()).expect("evaluates");

    // assert
    assert_eq!(compiled.rows(), ["x1", "x2", "x3", "f1"]);
    assert!(!compiled.uses_dynamic_lookup());
    assert_eq!(values[0], 15.0);
}

#[test]
fn when_a_literal_subscript_passes_the_inputs_should_refuse_at_the_subscript() {
    // setup
    let source = "var[4]";

    // act
    let failure = refused(source);

    // assert
    let [problem] = failure.problems.as_slice() else {
        panic!("expected one problem, got {:#?}", failure.problems);
    };
    assert_eq!(
        problem.kind,
        ProblemKind::DynamicIndexOutOfBounds {
            requested_1index: 4,
            available: 3,
        }
    );
    assert_eq!(problem.span, Span::new(4, 5));
}

#[test]
fn when_a_computed_subscript_lands_past_the_inputs_should_fault_rather_than_read_an_intermediate() {
    // setup: x1 = 4 names the fourth row, which holds f1.
    let compiled = classified("var[floor(x1)] + f1");
    let rows = column(&[4.0, 5.0, 7.0, 12.0]);

    // act
    let result = compiled.eval(rows.as_ref());

    // assert
    assert_eq!(compiled.rows(), ["x1", "x2", "x3", "f1"]);
    match result {
        Err(EvaluationFailure::Runtime(problem)) => {
            assert_eq!(
                problem.problem.kind,
                ProblemKind::DynamicIndexOutOfBounds {
                    requested_1index: 4,
                    available: 3,
                }
            );
            assert_eq!(problem.problem.span, Span::new(4, 13));
        }
        other => panic!("expected an out-of-bounds fault, got {other:?}"),
    }
}

#[test]
fn when_a_computed_subscript_names_an_input_should_read_it() {
    // setup: x1 = 2 names x2.
    let compiled = classified("var[floor(x1)] + f1");
    let rows = column(&[2.0, 5.0, 7.0, 12.0]);

    // act
    let values = compiled.eval(rows.as_ref()).expect("in range");

    // assert
    assert_eq!(values[0], 17.0);
}

// ---- names and where they are ----

#[test]
fn when_a_name_is_in_neither_list_should_report_it_unbound_at_its_reference() {
    // setup
    let source = "x1 + g";

    // act
    let failure = refused(source);

    // assert
    let [problem] = failure.problems.as_slice() else {
        panic!("expected one problem, got {:#?}", failure.problems);
    };
    assert_eq!(
        problem.kind,
        ProblemKind::Unbound {
            name: "g".to_owned()
        }
    );
    assert_eq!(problem.span, Span::new(5, 6));
}

#[test]
fn when_a_name_repeats_should_give_the_span_of_its_first_reference() {
    // setup
    let compiled = classified("f1 + x1 * f1");

    // act
    let spans: Vec<(&str, Span)> = compiled.reference_spans().collect();

    // assert
    assert_eq!(spans, [("f1", Span::new(0, 2)), ("x1", Span::new(5, 7))]);
}

#[test]
#[should_panic(expected = "declared twice")]
fn when_a_name_is_both_an_input_and_an_intermediate_should_panic() {
    // act
    let _ = Variables::classified(&["x1", "f1"], &["f1"]);
}

// ---- what a call site already writes still compiles ----

#[test]
fn when_given_an_array_a_vec_or_a_slice_should_compile_the_same() {
    // setup
    let array = ["x1", "x2"];
    let vec = vec!["x1".to_owned(), "x2".to_owned()];
    let slice: &[&str] = &array;

    // act
    let rows = [
        sojourn::compile("x1 + x2", &array)
            .expect("array")
            .rows()
            .to_vec(),
        sojourn::compile("x1 + x2", &vec)
            .expect("vec")
            .rows()
            .to_vec(),
        sojourn::compile("x1 + x2", slice)
            .expect("slice")
            .rows()
            .to_vec(),
    ];

    // assert
    for row in rows {
        assert_eq!(row, ["x1", "x2"]);
    }
}

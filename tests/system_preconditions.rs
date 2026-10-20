//! `ConstraintSystem::new`'s preconditions on its variables: names that are legal Babel variable
//! names and distinct, bounds that are finite with the lower not above the upper, a step that is
//! positive, lands on the upper bound and can be held exactly, and levels that are finite and
//! strictly ascending. Breaking one
//! is the caller's mistake, not a verdict about the system, so it panics rather than returning a
//! `SystemError`, and before any constraint is read.
//!
//! Each test catches the panic and checks its message exactly, so a panic from anywhere else, or
//! with the wrong words, cannot pass for the one the test is about.

use std::panic;

use sojourn::{ConstraintSystem, Domain, InputVariable};

/// The message `ConstraintSystem::new` panics with over `variables`. Fails the test if it returns
/// instead, or panics with something other than a string.
fn panic_message(variables: Vec<InputVariable>) -> String {
    let outcome = panic::catch_unwind(|| ConstraintSystem::new(variables, ["1 < 2"]));
    let payload = match outcome {
        Ok(built) => panic!("expected a panic, but ConstraintSystem::new returned {built:?}"),
        Err(payload) => payload,
    };
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => panic!("ConstraintSystem::new panicked with a payload that is not a string"),
        },
    }
}

#[test]
fn inverted_bounds_panic_naming_the_variable() {
    // setup
    let variables = vec![
        InputVariable::new_continuous("x", 0.0, 1.0),
        InputVariable::new_continuous("y", 3.0, 1.0),
    ];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "y" has lower bound 3 above its upper bound 1"#
    );
}

#[test]
fn a_nan_bound_panics_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new_continuous("x", f64::NAN, 1.0)];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "x" ranges over [NaN, 1]; bounds must be finite"#
    );
}

#[test]
fn an_infinite_bound_panics_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new_continuous("x", 0.0, f64::INFINITY)];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "x" ranges over [0, inf]; bounds must be finite"#
    );
}

#[test]
fn a_repeated_name_panics_naming_the_repeat() {
    // setup
    let variables = vec![
        InputVariable::new_continuous("x", 0.0, 1.0),
        InputVariable::new_continuous("x", 0.0, 1.0),
    ];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(message, r#"variable 1 repeats the name "x""#);
}

#[test]
fn an_empty_name_panics_naming_its_position() {
    // setup
    let variables = vec![InputVariable::new_continuous("", 0.0, 1.0)];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable 0 is named "", which is not a legal variable name"#
    );
}

#[test]
fn an_illegal_name_panics_naming_its_position() {
    // setup
    let variables = vec![InputVariable::new_continuous("1x", 0.0, 1.0)];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable 0 is named "1x", which is not a legal variable name"#
    );
}

#[test]
fn equal_bounds_fix_the_variable() -> Result<(), sojourn::SystemError> {
    // setup
    let variables = vec![InputVariable::new_continuous("x", 0.25, 0.25)];

    // act
    let system = ConstraintSystem::new(variables, ["x < 0.5"])?;

    // assert
    assert_eq!(system.variables().len(), 1);
    Ok(())
}

#[test]
fn a_non_positive_step_panics_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 0.0, 0.0, 10.0)];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" steps by 0; a step must be positive and finite"#
    );
}

#[test]
fn a_nan_step_panics_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 0.0, f64::NAN, 10.0)];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" steps by NaN; a step must be positive and finite"#
    );
}

#[test]
fn an_upper_bound_off_the_step_panics_naming_both() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 0.0, 0.3, 1.0)];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" steps by 0.3 from 0, which never lands on its upper bound 1; declare its values with Domain::Levels if they are not evenly spaced"#
    );
}

#[test]
fn a_third_is_not_a_step_and_the_panic_suggests_levels() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 0.0, 1.0 / 3.0, 1.0)];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" steps by 0.3333333333333333 from 0 to 1, which is too fine to index exactly in f64; declare its values with Domain::Levels instead"#
    );
}

#[test]
fn a_lattice_too_fine_to_index_exactly_panics() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 1e15, 0.1, 1e15 + 1.0)];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" steps by 0.1 from 1000000000000000 to 1000000000000001, which is too fine to index exactly in f64; declare its values with Domain::Levels instead"#
    );
}

#[test]
fn empty_levels_panic_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new("n", Domain::Levels(vec![]))];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(message, r#"variable "n" has no levels"#);
}

#[test]
fn a_nan_level_panics_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new("n", Domain::Levels(vec![1.0, f64::NAN]))];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" has a level NaN; levels must be finite"#
    );
}

#[test]
fn unsorted_levels_panic_naming_the_first_out_of_order() {
    // setup
    let variables = vec![InputVariable::new(
        "n",
        Domain::Levels(vec![1.0, 3.0, 2.0, 0.5]),
    )];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" lists the level 2 after 3; levels must be strictly ascending"#
    );
}

#[test]
fn a_repeated_level_panics_naming_it() {
    // setup
    let variables = vec![InputVariable::new("n", Domain::Levels(vec![1.0, 2.0, 2.0]))];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(message, r#"variable "n" lists the level 2 twice"#);
}

#[test]
fn a_single_level_fixes_the_variable() -> Result<(), sojourn::SystemError> {
    // setup
    let variables = vec![
        InputVariable::new("x", Domain::Levels(vec![2.0])),
        InputVariable::new_discrete("y", 3.0, 1.0, 3.0),
    ];

    // act
    let system = ConstraintSystem::new(variables, ["x < y"])?;

    // assert
    assert!(system.is_feasible(&[2.0, 3.0], 0.0));
    Ok(())
}

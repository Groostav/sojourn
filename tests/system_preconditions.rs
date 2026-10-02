//! `ConstraintSystem::new`'s preconditions on its variables: names that are legal Babel variable
//! names and distinct, and bounds that are finite with the lower not above the upper. Breaking one
//! is the caller's mistake, not a verdict about the system, so it panics rather than returning a
//! `SystemError`, and before any constraint is read.
//!
//! Each test catches the panic and checks its message exactly, so a panic from anywhere else, or
//! with the wrong words, cannot pass for the one the test is about.

use std::panic;

use sojourn::{ConstraintSystem, InputVariable};

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
        InputVariable::new("x", 0.0, 1.0),
        InputVariable::new("y", 3.0, 1.0),
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
    let variables = vec![InputVariable::new("x", f64::NAN, 1.0)];

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
    let variables = vec![InputVariable::new("x", 0.0, f64::INFINITY)];

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
        InputVariable::new("x", 0.0, 1.0),
        InputVariable::new("x", 0.0, 1.0),
    ];

    // act
    let message = panic_message(variables);

    // assert
    assert_eq!(message, r#"variable 1 repeats the name "x""#);
}

#[test]
fn an_empty_name_panics_naming_its_position() {
    // setup
    let variables = vec![InputVariable::new("", 0.0, 1.0)];

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
    let variables = vec![InputVariable::new("1x", 0.0, 1.0)];

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
    let variables = vec![InputVariable::new("x", 0.25, 0.25)];

    // act
    let system = ConstraintSystem::new(variables, ["x < 0.5"])?;

    // assert
    assert_eq!(system.variables().len(), 1);
    Ok(())
}

//! `ConstraintSystem::new`'s preconditions on its variables: names that are legal Babel variable
//! names and distinct, bounds that are finite with the lower not above the upper, a step that is
//! positive, lands on the upper bound and can be held exactly, and levels that are finite and
//! strictly ascending. Breaking one is refused with a
//! `MalformedConstraintSystemError` naming the variable, before any constraint is read: a caller
//! whose declarations come from a file or a command line cannot check the lattice arithmetic
//! without repeating it, and a panic there takes the caller's process with it.
//!
//! Each test checks the variant and its message exactly, so a refusal for some other reason, or
//! with the wrong words, cannot pass for the one the test is about.

use sojourn::{
    ConstraintSystem, Domain, DomainDefect, InputVariable, MalformedConstraintSystemError,
};

/// The error `ConstraintSystem::new` refuses `variables` with. Fails the test if it returns a
/// system instead.
fn refusal(variables: Vec<InputVariable>) -> MalformedConstraintSystemError {
    match ConstraintSystem::new(variables, ["1 < 2"]) {
        Ok(built) => panic!("expected a refusal, but ConstraintSystem::new returned {built:?}"),
        Err(error) => error,
    }
}

/// The variable and defect of a `MalformedDomain` refusal. Fails the test on any other variant.
fn domain_refusal(variables: Vec<InputVariable>) -> (usize, String, DomainDefect, String) {
    let error = refusal(variables);
    let message = error.to_string();
    let MalformedConstraintSystemError::MalformedDomain {
        index,
        variable,
        defect,
    } = error
    else {
        panic!("expected MalformedDomain, got {error:?}");
    };
    (index, variable, defect, message)
}

#[test]
fn when_bounds_are_inverted_should_refuse_naming_the_variable() {
    // setup
    let variables = vec![
        InputVariable::new_continuous("x", 0.0, 1.0),
        InputVariable::new_continuous("y", 3.0, 1.0),
    ];

    // act
    let (index, variable, defect, message) = domain_refusal(variables);

    // assert
    assert_eq!((index, variable.as_str()), (1, "y"));
    assert_eq!(
        defect,
        DomainDefect::InvertedBounds {
            lower: 3.0,
            upper: 1.0
        }
    );
    assert_eq!(
        message,
        r#"variable "y" has lower bound 3 above its upper bound 1"#
    );
}

#[test]
fn when_a_bound_is_nan_should_refuse_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new_continuous("x", f64::NAN, 1.0)];

    // act
    let (_, _, defect, message) = domain_refusal(variables);

    // assert
    assert!(
        matches!(defect, DomainDefect::NonFiniteBounds { .. }),
        "{defect:?}"
    );
    assert_eq!(
        message,
        r#"variable "x" ranges over [NaN, 1]; bounds must be finite"#
    );
}

#[test]
fn when_a_bound_is_infinite_should_refuse_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new_continuous("x", 0.0, f64::INFINITY)];

    // act
    let (_, _, _, message) = domain_refusal(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "x" ranges over [0, inf]; bounds must be finite"#
    );
}

#[test]
fn when_a_name_repeats_should_refuse_naming_the_repeat() {
    // setup
    let variables = vec![
        InputVariable::new_continuous("x", 0.0, 1.0),
        InputVariable::new_continuous("x", 0.0, 1.0),
    ];

    // act
    let error = refusal(variables);

    // assert
    assert_eq!(error.to_string(), r#"variable 1 repeats the name "x""#);
    assert_eq!(
        error,
        MalformedConstraintSystemError::RepeatedVariableName {
            index: 1,
            variable: "x".to_owned()
        }
    );
}

#[test]
fn when_a_name_is_empty_should_refuse_naming_its_position() {
    // setup
    let variables = vec![InputVariable::new_continuous("", 0.0, 1.0)];

    // act
    let error = refusal(variables);

    // assert
    assert_eq!(
        error.to_string(),
        r#"variable 0 is named "", which is not a legal variable name"#
    );
}

#[test]
fn when_a_name_is_illegal_should_refuse_naming_its_position() {
    // setup
    let variables = vec![InputVariable::new_continuous("1x", 0.0, 1.0)];

    // act
    let error = refusal(variables);

    // assert
    assert_eq!(
        error,
        MalformedConstraintSystemError::IllegalVariableName {
            index: 0,
            variable: "1x".to_owned()
        }
    );
    assert_eq!(
        error.to_string(),
        r#"variable 0 is named "1x", which is not a legal variable name"#
    );
}

#[test]
fn when_bounds_are_equal_should_fix_the_variable() -> Result<(), MalformedConstraintSystemError> {
    // setup
    let variables = vec![InputVariable::new_continuous("x", 0.25, 0.25)];

    // act
    let system = ConstraintSystem::new(variables, ["x < 0.5"])?;

    // assert
    assert_eq!(system.variables().len(), 1);
    Ok(())
}

#[test]
fn when_a_step_is_zero_should_refuse_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 0.0, 0.0, 10.0)];

    // act
    let (_, _, defect, message) = domain_refusal(variables);

    // assert
    assert_eq!(defect, DomainDefect::NonPositiveStep { step: 0.0 });
    assert_eq!(
        message,
        r#"variable "n" steps by 0; a step must be positive and finite"#
    );
}

#[test]
fn when_a_step_is_nan_should_refuse_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 0.0, f64::NAN, 10.0)];

    // act
    let (_, _, _, message) = domain_refusal(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" steps by NaN; a step must be positive and finite"#
    );
}

#[test]
fn when_the_upper_bound_is_off_the_step_should_refuse_naming_both() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 0.0, 0.3, 1.0)];

    // act
    let (_, _, defect, message) = domain_refusal(variables);

    // assert
    assert!(
        matches!(defect, DomainDefect::StepMissesUpperBound { .. }),
        "{defect:?}"
    );
    assert_eq!(
        message,
        r#"variable "n" steps by 0.3 from 0, which never lands on its upper bound 1; declare its values with Domain::Levels if they are not evenly spaced"#
    );
}

#[test]
fn when_the_step_is_a_third_should_refuse_and_suggest_levels() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 0.0, 1.0 / 3.0, 1.0)];

    // act
    let (_, _, defect, message) = domain_refusal(variables);

    // assert
    assert!(
        matches!(defect, DomainDefect::LatticeTooFine { .. }),
        "{defect:?}"
    );
    assert_eq!(
        message,
        r#"variable "n" steps by 0.3333333333333333 from 0 to 1, which is too fine to index exactly in f64; declare its values with Domain::Levels instead"#
    );
}

#[test]
fn when_a_lattice_is_too_fine_to_index_exactly_should_refuse() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 1e15, 0.1, 1e15 + 1.0)];

    // act
    let (_, _, _, message) = domain_refusal(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" steps by 0.1 from 1000000000000000 to 1000000000000001, which is too fine to index exactly in f64; declare its values with Domain::Levels instead"#
    );
}

/// wayfarer's `0:1e-20:1`: every part is a short decimal, but the lattice has 10^20 levels, which
/// no `f64` integer can count. It used to take the caller's process down.
#[test]
fn when_a_step_has_more_levels_than_f64_can_count_should_refuse() {
    // setup
    let variables = vec![InputVariable::new_discrete("n", 0.0, 1e-20, 1.0)];

    // act
    let (index, variable, defect, message) = domain_refusal(variables);

    // assert
    assert_eq!((index, variable.as_str()), (0, "n"));
    assert_eq!(
        defect,
        DomainDefect::LatticeTooFine {
            lower: 0.0,
            step: 1e-20,
            upper: 1.0
        }
    );
    assert_eq!(
        message,
        r#"variable "n" steps by 0.00000000000000000001 from 0 to 1, which is too fine to index exactly in f64; declare its values with Domain::Levels instead"#
    );
}

#[test]
fn when_levels_are_empty_should_refuse_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new("n", Domain::Levels(vec![]))];

    // act
    let (_, _, defect, message) = domain_refusal(variables);

    // assert
    assert_eq!(defect, DomainDefect::NoLevels);
    assert_eq!(message, r#"variable "n" has no levels"#);
}

#[test]
fn when_a_level_is_nan_should_refuse_naming_the_variable() {
    // setup
    let variables = vec![InputVariable::new("n", Domain::Levels(vec![1.0, f64::NAN]))];

    // act
    let (_, _, _, message) = domain_refusal(variables);

    // assert
    assert_eq!(
        message,
        r#"variable "n" has a level NaN; levels must be finite"#
    );
}

#[test]
fn when_levels_are_unsorted_should_refuse_naming_the_first_out_of_order() {
    // setup
    let variables = vec![InputVariable::new(
        "n",
        Domain::Levels(vec![1.0, 3.0, 2.0, 0.5]),
    )];

    // act
    let (_, _, defect, message) = domain_refusal(variables);

    // assert
    assert_eq!(
        defect,
        DomainDefect::DescendingLevels {
            before: 3.0,
            after: 2.0
        }
    );
    assert_eq!(
        message,
        r#"variable "n" lists the level 2 after 3; levels must be strictly ascending"#
    );
}

#[test]
fn when_a_level_repeats_should_refuse_naming_it() {
    // setup
    let variables = vec![InputVariable::new("n", Domain::Levels(vec![1.0, 2.0, 2.0]))];

    // act
    let (_, _, defect, message) = domain_refusal(variables);

    // assert
    assert_eq!(defect, DomainDefect::RepeatedLevel { level: 2.0 });
    assert_eq!(message, r#"variable "n" lists the level 2 twice"#);
}

#[test]
fn when_there_is_a_single_level_should_fix_the_variable()
-> Result<(), MalformedConstraintSystemError> {
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

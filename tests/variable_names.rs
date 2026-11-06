//! Variable-name legality, ported from `VariableNameFixture.kt`.
//!
//! Extended beyond the Kotlin fixture with the Unicode identifiers the
//! expression corpus exercises, since `VARIABLE_START` had to be rewritten for
//! the Rust target — the JVM grammar expressed it in terms of UTF-16 surrogate
//! pairs, which Rust's `char` cannot represent.

fn assert_legal(name: &str) {
    assert!(
        sojourn::is_legal_variable_name(name),
        "{name:?} should be a legal variable name"
    );
}

fn assert_illegal(name: &str) {
    assert!(
        !sojourn::is_legal_variable_name(name),
        "{name:?} should be an illegal variable name"
    );
}

#[test]
fn ascii_names_are_legal() {
    for name in ["x1", "_name", "x_1"] {
        assert_legal(name);
    }
}

#[test]
fn unicode_names_are_legal() {
    for name in ["π", "测试", "☕", "大_da_dai_meaning_big"] {
        assert_legal(name);
    }
}

/// `implies` is a keyword since 0.5.0, reserved the way `max` and `e` are;
/// a name that merely starts with it is still a name.
#[test]
fn when_the_name_is_the_implies_keyword_should_be_illegal() {
    assert_illegal("implies");
    assert_legal("impliesX");
    assert_legal("x_implies");
}

/// `and` and `or` are keywords since 0.5.0, beside `implies`.
#[test]
fn when_the_name_is_a_connective_keyword_should_be_illegal() {
    assert_illegal("and");
    assert_illegal("or");
    assert_legal("andy");
    assert_legal("order");
    assert_legal("x_or");
}

#[test]
fn bare_number_is_illegal() {
    assert_illegal("3");
}

#[test]
fn punctuation_is_illegal() {
    for name in ["$x1", "@x", r"\x", "x$"] {
        assert_illegal(name);
    }
}

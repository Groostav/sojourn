//! Compile-time diagnostics, ported from `BabelCompilerErrorFixture.kt`.
//!
//! Babel forwards ANTLR's syntax diagnostics rather than curating them, so for
//! these cases **ANTLR's own output is the oracle**. A parser recovering from
//! one mistake often emits several diagnostics, and the count is a recovery
//! detail rather than a contract — so each case asserts that *some* reported
//! problem matches, never that exactly one was reported.
//!
//! Departures from the Kotlin fixture:
//!
//! * Structured fields only. Kotlin asserted on rendered caret strings and on
//!   `abbreviatedProblemText`; neither is ported.
//! * `line_idx` and `column_idx` are zero-based and both derived from
//!   `span.start`. Kotlin's `lineNo` was one-based and its `characterNo` was
//!   computed inconsistently between call sites.
//! * Kotlin's `rangeInText` was an inclusive `IntRange`; [`Span`] is half-open.

use sojourn::diagnostics::{BoundKind, CompilationFailure, Problem, ProblemKind, Span};

/// Nothing here binds, so no variables are declared: a parse failure is the
/// only kind these fixtures can produce, and an unbound name would be a bug
/// in the fixture.
const NO_VARIABLES: [&str; 0] = [];

fn compile_to_failure(expr: &str) -> CompilationFailure {
    match sojourn::compile(expr, &NO_VARIABLES) {
        Ok(_) => panic!("expected {expr:?} to fail compilation, but it succeeded"),
        Err(failure) => failure,
    }
}

/// Asserts that at least one reported problem satisfies `predicate`.
fn assert_reports(expr: &str, description: &str, predicate: impl Fn(&Problem) -> bool) {
    let failure = compile_to_failure(expr);
    assert!(
        failure.problems.iter().any(predicate),
        "expected {expr:?} to report {description}, got {:#?}",
        failure.problems
    );
}

/// The common shape: a syntax error at a known place.
fn assert_syntax_at(expr: &str, span: Span, column_idx: u32, from_lexer: bool) {
    assert_reports(
        expr,
        &format!("a syntax error at {span:?} (from_lexer={from_lexer})"),
        |p| {
            matches!(&p.kind, ProblemKind::Syntax { from_lexer: l, .. } if *l == from_lexer)
                && p.span == span
                && p.line_idx == 0
                && p.column_idx == column_idx
        },
    );
}

#[test]
fn empty_expression_fails_eagerly() {
    // The one case where an exact count really is the contract: babel rejects
    // this before ANTLR is ever involved.
    let failure = compile_to_failure("");
    assert_eq!(failure.problems.len(), 1);
    assert_eq!(failure.problems[0].kind, ProblemKind::EmptyExpression);
}

#[test]
fn dangling_operator() {
    assert_syntax_at("x1 + x2 +", Span::new(9, 9), 9, false);
}

#[test]
fn illegal_character() {
    assert_syntax_at("1 + @x1", Span::new(4, 5), 4, true);
}

#[test]
fn equality_without_bound() {
    assert_syntax_at("x1 = x2", Span::new(7, 7), 7, false);
}

#[test]
fn equality_with_non_literal_bound() {
    assert_syntax_at("x1 = x2 +/- x3", Span::new(12, 14), 12, false);
}

#[test]
fn nested_boolean_clause_is_rejected() {
    assert_syntax_at("1+(x > 3) + 2", Span::new(5, 6), 5, false);
}

/// `a implies b implies c` has three readings and a reader holds one of
/// them; see [`ProblemKind::ChainedImplication`]. The caret is the second
/// `implies`, the one that would need a bracket.
#[test]
fn a_bare_chain_of_implications_is_refused_at_the_second() {
    // setup
    let source = "x > 0 implies y > 0 implies z > 0";

    // act
    let failure =
        sojourn::compile(source, &["x", "y", "z"]).expect_err("a bare chain should not compile");

    // assert
    assert!(
        failure
            .problems
            .iter()
            .any(|p| p.kind == ProblemKind::ChainedImplication && p.span == Span::new(20, 27)),
        "{:#?}",
        failure.problems
    );
}

#[test]
fn a_bracketed_chain_of_implications_compiles_either_way() {
    for source in [
        "x > 0 implies (y > 0 implies z > 0)",
        "(x > 0 implies y > 0) implies z > 0",
        "(x > 0) implies (y > 0)",
    ] {
        sojourn::compile(source, &["x", "y", "z"])
            .unwrap_or_else(|e| panic!("{source:?} should compile: {e:#}"));
    }
}

/// An implication is a boolean like any comparison, so it has the same
/// places it cannot go: inside arithmetic and inside a lambda's body.
#[test]
fn an_implication_is_not_a_scalar() {
    for (source, span, column) in [
        ("1 + (x > 0 implies x < 1)", Span::new(7, 8), 7),
        ("sum(1, 3, i -> i > 0 implies i < 2)", Span::new(17, 18), 17),
    ] {
        assert_reports(source, &format!("a syntax error at {span:?}"), |p| {
            matches!(p.kind, ProblemKind::Syntax { .. }) && p.span == span && p.column_idx == column
        });
    }
}

/// `a and b or c` has two readings and no convention every reader shares, so
/// it is refused at the connective that changes, as Dafny refuses it; see
/// [`ProblemKind::MixedConnectives`]. Brackets either way compile.
#[test]
fn when_and_and_or_are_mixed_without_brackets_should_refuse_at_the_change() {
    for (source, span) in [
        ("x > 0 and y > 0 or z > 0", Span::new(16, 18)),
        ("x > 0 or y > 0 or z > 0 and x < 1", Span::new(24, 27)),
    ] {
        // act
        let failure = sojourn::compile(source, &["x", "y", "z"]).expect_err("mixed connectives");

        // assert
        assert!(
            failure
                .problems
                .iter()
                .any(|p| p.kind == ProblemKind::MixedConnectives && p.span == span),
            "{source:?}: {:#?}",
            failure.problems
        );
    }
}

#[test]
fn when_mixed_connectives_are_bracketed_should_compile() {
    for source in [
        "(x > 0 and y > 0) or z > 0",
        "x > 0 and (y > 0 or z > 0)",
        "x > 0 and y > 0 and z > 0",
        "x > 0 or y > 0 or z > 0",
        "x > 0 and y > 0 implies z > 0 or x < 1",
    ] {
        sojourn::compile(source, &["x", "y", "z"])
            .unwrap_or_else(|e| panic!("{source:?} should compile: {e:#}"));
    }
}

/// A conjunction is a boolean like any comparison: not inside arithmetic,
/// not inside a lambda's body, and not missing an operand.
#[test]
fn when_a_connective_is_used_as_a_scalar_or_dangles_should_not_parse() {
    for (source, span, column) in [
        ("1 + (x > 0 and x < 1)", Span::new(7, 8), 7),
        ("sum(1, 3, i -> i > 0 or i < 2)", Span::new(17, 18), 17),
    ] {
        assert_reports(source, &format!("a syntax error at {span:?}"), |p| {
            matches!(p.kind, ProblemKind::Syntax { .. }) && p.span == span && p.column_idx == column
        });
    }
    assert_syntax_at("x > 0 and", Span::new(9, 9), 9, false);
}

#[test]
fn an_implication_missing_its_conclusion_does_not_parse() {
    assert_syntax_at("x > 0 implies", Span::new(13, 13), 13, false);
}

#[test]
fn a_boolean_cannot_be_used_as_a_scalar() {
    // The grammar admits `booleanExpr` only at `returnStatement`, so there is
    // nowhere for the `* 3` to attach and these are syntax errors.
    //
    // Worth pinning: the JVM implementation carried a whole semantic check
    // (`TypeErrorReportingWalker`, and the `BooleanInScalarPosition` problem it
    // raised) for this case, because its rewriter would turn `x1 > 5` into
    // `5 - x1` in place and the surrounding arithmetic would then compile
    // happily. Rejecting at the grammar makes that unreachable, and this test is
    // what lets the check stay deleted.
    for expression in ["(x1 > 5) * 3", "(x1 > 5) * 3 < 0"] {
        assert_reports(expression, "a syntax error", |p| {
            matches!(&p.kind, ProblemKind::Syntax { .. })
        });
    }
}

#[test]
fn chained_equality_without_bound() {
    assert_syntax_at("P1+P2+P3+P4+P5+P6+P7==30", Span::new(24, 24), 24, false);
}

#[test]
fn a_statically_nan_bound_is_caught_at_compile_time() {
    // `0/0` is refused by constant folding before unrolling ever looks at the
    // bound, so the diagnostic points at the division rather than at "the lower
    // bound" — the more useful of the two.
    assert_reports(
        "sum(0/0, 20, i -> i + 2)",
        "a non-finite constant",
        |p| matches!(&p.kind, ProblemKind::NonFiniteConstant { value } if value.is_nan()),
    );
}

/// `a == b +/- 0` is exact `f64` equality: a feasible set with no volume,
/// which nothing downstream can sample. Refused on the value, so every
/// spelling of zero and every negative is one rule.
#[test]
fn a_zero_or_negative_tolerance_is_caught_at_compile_time() {
    for tolerance in ["0", "0.0", "-0.0", "0.0e1", "0.0e5", "-1", "-0.001"] {
        let expr = format!("x1 == x2 +/- {tolerance}");
        assert_reports(&expr, "a degenerate tolerance", |p| {
            matches!(&p.kind, ProblemKind::DegenerateTolerance { .. })
                && p.span.end == u32::try_from(expr.len()).unwrap()
        });
    }
}

/// The tolerance is not an `Expr`, so the folding pass that refuses `1.0e400`
/// elsewhere never sees it; the parser applies the same rule itself.
#[test]
fn a_non_finite_tolerance_is_caught_at_compile_time() {
    assert_reports(
        "x1 == x2 +/- 1.0e400",
        "a non-finite constant",
        |p| matches!(&p.kind, ProblemKind::NonFiniteConstant { value } if value.is_infinite()),
    );
}

/// And a positive one, however small, is a band with a width.
#[test]
fn a_positive_tolerance_compiles() {
    for tolerance in ["0.001", "1.0e-300", "1.0e-309", "pi"] {
        sojourn::compile(&format!("x1 == x2 +/- {tolerance}"), &["x1", "x2"])
            .unwrap_or_else(|e| panic!("{tolerance}: {e:?}"));
    }
}

#[test]
fn a_fractional_bound_is_caught_at_compile_time() {
    // The other half of the same story, and why `IllegalAggregateBound` is
    // still reachable at compile time: `20/3` folds to a perfectly finite
    // 6.666…, which folding is happy with and `to_index` is not. The JVM
    // rounded it to 7 and said nothing.
    assert_reports("sum(1, 20/3, i -> i + 2)", "an illegal upper bound", |p| {
        matches!(&p.kind, ProblemKind::IllegalAggregateBound { .. })
    });
}

/// A literal subscript that no schema could satisfy is refused where it is
/// written, the way an aggregate bound is: subscripts are one-based, so
/// `var[0]` and `var[-1]` name nothing whatever the box declares. The caret
/// lands on the subscript, not on `var`. Zero is the common mistake and gets
/// the fix in the message.
#[test]
fn a_subscript_below_one_is_caught_at_compile_time() {
    assert_reports("var[0] + 1", "a subscript of zero", |p| {
        p.kind == ProblemKind::ZeroIndex
            && p.span == Span::new(4, 5)
            && p.to_string() == "var[0] is not the first parameter (did you mean var[1]?) at '0'"
    });
    assert_reports("var[-1]", "a negative subscript", |p| {
        p.kind
            == ProblemKind::NegativeDynamicIndex {
                requested_1index: -1,
            }
            && p.span == Span::new(4, 6)
    });
    // Folded first, so an expression that works out to zero is caught too.
    assert_reports("var[2 - 2]", "a subscript that folds to zero", |p| {
        p.kind == ProblemKind::ZeroIndex
    });
}

/// A literal subscript past the variable list is a binding failure by
/// position, reported the way an unbound name is: at bind, with a caret on
/// the subscript. `compile` and `ConstraintSystem::new` agree on this.
#[test]
fn a_subscript_past_the_variables_is_caught_at_bind_time() {
    let failure = sojourn::compile("var[2] + x1", &["x1"]).expect_err("there is no var[2]");

    assert_eq!(failure.problems.len(), 1, "{:#?}", failure.problems);
    let problem = &failure.problems[0];
    assert_eq!(
        problem.kind,
        ProblemKind::DynamicIndexOutOfBounds {
            requested_1index: 2,
            available: 1,
        }
    );
    assert_eq!(problem.span, Span::new(4, 5));
    assert_eq!(
        problem.to_string(),
        "attempted to access 'var[2]' (the 2nd parameter) when only 1 exist at '2': evaluates to 2"
    );
}

/// A subscript the row decides is a whole number *by construction* or it
/// does not compile: an integer literal, an aggregate's parameter, `floor`
/// or `ceil` of anything, or exact integer arithmetic over those. Anything
/// else — a bare variable, a division, a power — is refused where it is
/// written, with the fix in the message. There is no runtime rounding check
/// to fall back on, because none is needed.
#[test]
fn a_subscript_the_row_decides_must_say_how_it_rounds() {
    let names = ["x1", "x2", "x3"];
    for (source, span) in [
        ("var[x2]", Span::new(4, 6)),
        ("sum(1, 3, i -> var[i/2])", Span::new(19, 22)),
        ("var[x1 - 0.5]", Span::new(4, 12)),
        ("var[x1^2]", Span::new(4, 8)),
        // Integral operands do not rescue an operator outside the table.
        ("var[floor(x1) / 2]", Span::new(4, 17)),
        ("var[2 ^ floor(x1)]", Span::new(4, 17)),
        ("var[floor(x1) ^ 0.5]", Span::new(4, 19)),
        ("var[sqrt(floor(x1))]", Span::new(4, 19)),
        ("var[log(floor(x1), 2)]", Span::new(4, 21)),
    ] {
        let failure = sojourn::compile(source, &names).expect_err(source);
        assert_eq!(
            failure.problems.len(),
            1,
            "{source}: {:#?}",
            failure.problems
        );
        let problem = &failure.problems[0];
        assert_eq!(problem.kind, ProblemKind::SubscriptNotIntegral, "{source}");
        assert_eq!(problem.span, span, "{source}");
    }
    let problem = &sojourn::compile("var[x2]", &names)
        .expect_err("x2")
        .problems[0];
    assert_eq!(
        problem.to_string(),
        "this subscript is not a whole number by construction at 'x2': \
         wrap it in floor() or ceil() to say which"
    );
}

/// The forms that are whole by construction — one of each — every one exact
/// in `f64`: `floor`, `ceil` and `sgn` by definition; `+`, `-`, `*`, `%`,
/// `abs`, negation, `sqr`, `cube`, `max`/`min` of integers below 2^53 because
/// the exact result is representable and IEEE arithmetic is correctly
/// rounded; a whole power because the backends make repeated multiplication
/// of it. An aggregate's parameter, an aggregate of these, and a `var` bound
/// to any of them count too.
#[test]
fn a_subscript_that_is_whole_by_construction_compiles() {
    let names: Vec<String> = (1..=10).map(|i| format!("x{i}")).collect();
    for source in [
        // the two ways to say how a value rounds, and the sign
        "var[floor(x2)]",
        "var[ceil(x1 * 2)]",
        "var[sgn(x1) + 2]",
        // arithmetic over integral operands
        "var[floor(x1) + 1]",
        "var[floor(x1) - 1 + 2]",
        "var[floor(x1) * 2]",
        "var[floor(x1) % 3 + 1]",
        "var[-floor(x1) + 5]",
        "var[abs(floor(x1))]",
        "var[sqr(floor(x1))]",
        "var[cube(floor(x1))]",
        "var[floor(x1) ^ 2]",
        "var[max(floor(x1), 1)]",
        "var[min(floor(x1), 3)]",
        // an aggregate's parameter, an aggregate as the subscript, a binding
        "sum(1, 3, i -> var[2*i - 1])",
        "prod(1, 2, i -> var[i + 1])",
        "var[sum(1, 2, j -> j)]",
        "sum(1, 2, i -> var a = i + 1; var[a])",
        // and the rule applies inside a subscript as well
        "var[floor(var[floor(x1)])]",
    ] {
        sojourn::compile(source, &names)
            .unwrap_or_else(|e| panic!("{source:?} should compile: {e:#}"));
    }
}

/// Every offender is reported, in source order, so one round trip fixes all.
#[test]
fn every_non_integral_subscript_is_reported() {
    let failure =
        sojourn::compile("var[x1] + var[x2 / 2]", &["x1", "x2"]).expect_err("two offenders");
    let spans: Vec<Span> = failure.problems.iter().map(|p| p.span).collect();
    assert!(
        failure
            .problems
            .iter()
            .all(|p| p.kind == ProblemKind::SubscriptNotIntegral),
        "{:#?}",
        failure.problems
    );
    assert_eq!(spans, vec![Span::new(4, 6), Span::new(14, 20)]);
}

/// An aggregate whose parameter runs onto zero puts `var[0]` in the tree
/// when it unrolls, and that is refused like a written `var[0]` rather than
/// left for the first row.
#[test]
fn an_aggregate_that_unrolls_onto_zero_is_refused() {
    assert_reports("sum(0, 2, i -> var[i])", "var[0] from the unrolling", |p| {
        p.kind == ProblemKind::ZeroIndex
    });
}

/// And one that is not a whole number, for the same reason
/// [`a_fractional_bound_is_caught_at_compile_time`] gives: the JVM rounded.
#[test]
fn a_fractional_subscript_is_caught_at_compile_time() {
    assert_reports("var[1.5]", "a fractional subscript", |p| {
        matches!(&p.kind, ProblemKind::DynamicIndexNotAnInteger { value } if *value == 1.5)
            && p.span == Span::new(4, 7)
    });
}

/// `{:#}` is the expanded form — source and caret, monospace assumed. The
/// alternate flag means "more elaborate" throughout the standard library
/// (`{:#?}`, `{:#x}`), so it means that here too.
#[test]
fn alternate_display_renders_a_caret_block() {
    let failure = compile_to_failure("x1 + x2 +");
    let rendered = format!("{:#}", failure.problems[0]);
    let lines: Vec<&str> = rendered.lines().collect();

    assert_eq!(lines[0], "Error in 'end of expression': syntax error.");
    assert_eq!(lines[1], "x1 + x2 +");
    // Zero-width end-of-input span underlines the final character.
    assert!(
        lines[2].starts_with("        ~"),
        "expected the caret under the trailing '+', got {:?}",
        lines[2]
    );
}

/// Plain `{}` is the one-liner a caller puts in a log.
#[test]
fn plain_display_is_a_single_line() {
    let failure = compile_to_failure("x1 + x2 +");
    let rendered = failure.problems[0].to_string();

    assert_eq!(rendered.lines().count(), 1, "got {rendered:?}");
    assert!(
        rendered.starts_with("syntax error at 'end of expression'"),
        "got {rendered:?}"
    );
}

// --------------------------------------------- an unbound name has a span

/// A name the variable list lacks is a problem like any other: located, with
/// a caret under its first reference. One problem per name, however often it
/// is used.
#[test]
fn an_unbound_name_gets_a_caret_at_its_first_reference() {
    let failure = sojourn::compile("x2 + x1*x2", &["x1"]).expect_err("x2 is unbound");

    assert_eq!(failure.problems.len(), 1, "{:#?}", failure.problems);
    let problem = &failure.problems[0];
    assert_eq!(
        problem.kind,
        ProblemKind::Unbound {
            name: "x2".to_owned()
        }
    );
    assert_eq!(problem.span, Span::new(0, 2));
    assert_eq!((problem.line_idx, problem.column_idx), (0, 0));

    let rendered = format!("{problem:#}");
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines[0], "Error in 'x2': unknown variable.");
    assert_eq!(lines[1], "x2 + x1*x2");
    assert_eq!(lines[2], "~~ no input variable by that name");
    assert_eq!(
        problem.to_string(),
        "unknown variable at 'x2': no input variable by that name"
    );
}

/// Every unbound name is reported, in the order the expression first names
/// them, so one round trip fixes all of them.
#[test]
fn every_unbound_name_is_reported() {
    let failure = sojourn::compile("a + b + x1", &["x1"]).expect_err("a and b are unbound");

    let reported: Vec<(String, Span)> = failure
        .problems
        .iter()
        .map(|problem| match &problem.kind {
            ProblemKind::Unbound { name } => (name.clone(), problem.span),
            other => panic!("expected an unbound name, got {other:?}"),
        })
        .collect();
    assert_eq!(
        reported,
        vec![
            ("a".to_owned(), Span::new(0, 1)),
            ("b".to_owned(), Span::new(4, 5)),
        ]
    );
}

// --------------------------------------------- booleans are root-only

/// A comparison in a lambda body is a *parse* error, not a semantic one.
///
/// It used to parse, and then quietly sum constraint residuals as though they
/// were arithmetic: `sum(1, 3, i -> i > 2)` evaluated to `0.0` and
/// `prod(1, 3, i -> var a = i; a < 2)` to `-2.2e-308` — the strictness epsilon,
/// multiplied into a product. The JVM implementation went further and reported
/// the whole thing as a boolean expression.
///
/// `lambdaExpr` takes a `scalarBlock` now, which has no route to `booleanExpr`,
/// so the parser refuses it before meaning is ever assigned.
///
/// **The spans are the point.** ANTLR's wording is its own — "no viable
/// alternative at input …", which is jargon — but the caret lands exactly on
/// the offending operator, which is where a reader looks. If the wording ever
/// needs improving, the fix is an error alternative in the grammar rather than
/// a semantic check here.
#[test]
fn a_comparison_in_a_lambda_body_does_not_parse() {
    for (source, span, column) in [
        ("sum(1, 3, i -> i > 2)", Span::new(17, 18), 17),
        ("sum(1, 3, i -> i == 2 +/- 0.5)", Span::new(17, 19), 17),
        ("prod(1, 3, i -> var a = i; a < 2)", Span::new(29, 30), 29),
        ("sum(1, 3, i -> return i > 2)", Span::new(24, 25), 24),
    ] {
        assert_syntax_at(source, span, column, false);
    }
}

/// The other half, and the failure mode that matters more: a grammar change
/// that rejects too much. A lambda body is still a block, so statements and a
/// scalar result both have to survive.
#[test]
fn a_scalar_lambda_body_still_parses() {
    for source in [
        "sum(1, 3, i -> i)",
        "sum(1, 3, i -> var a = i + 1; a * 2)",
        "prod(1, 3, i -> return i * i)",
    ] {
        sojourn::compile(source, &NO_VARIABLES)
            .unwrap_or_else(|e| panic!("{source:?} should parse: {e:#}"));
    }
    // The subscripts this unrolls into are bound by position, so they need
    // the two hundred variables they name.
    let names: Vec<String> = (1..=200).map(|i| format!("x{i}")).collect();
    sojourn::compile("sum(1, 200, i -> var[i]^2 - 3.0)", &names)
        .unwrap_or_else(|e| panic!("should parse and bind: {e:#}"));
}

// ------------------------------------------------------------- aggregates
//
// `sum` and `prod` are big-sigma and big-pi over a fixed index set, unrolled at
// compile time. A bound that is not a constant, or is a constant that is not an
// index, or a span wider than the unroll cap, is refused here rather than being
// a loop the evaluator would have to run one sample at a time.

#[test]
fn a_bound_that_depends_on_a_variable_does_not_compile() {
    for (source, bound, span) in [
        ("sum(1, x1, i -> i)", BoundKind::Upper, Span::new(7, 9)),
        (
            "sum(x1 + 0, x1 + 5, i -> var[i])",
            BoundKind::Lower,
            Span::new(4, 10),
        ),
        (
            "sum (\n  0,\n  20/x1,\n  i -> i + 2\n)",
            BoundKind::Upper,
            Span::new(13, 18),
        ),
        (
            "prod(1, ceil(sqrt(target)), i -> i)",
            BoundKind::Upper,
            Span::new(8, 26),
        ),
    ] {
        assert_reports(source, "a non-constant aggregate bound", |p| {
            p.kind == ProblemKind::AggregateBoundNotConstant { bound } && p.span == span
        });
    }
}

#[test]
fn a_constant_bound_that_is_not_an_index_does_not_compile() {
    for (source, bound, value) in [
        ("sum(1, 2.5, i -> i)", BoundKind::Upper, 2.5),
        ("sum(1.0e300, 20, i -> i)", BoundKind::Lower, 1e300),
    ] {
        assert_reports(source, "an illegal aggregate bound", |p| {
            p.kind == ProblemKind::IllegalAggregateBound { bound, value }
        });
    }
}

#[test]
fn an_aggregate_wider_than_the_unroll_cap_does_not_compile() {
    assert_reports("sum(1, 2000, i -> i)", "an aggregate past the cap", |p| {
        p.kind
            == ProblemKind::AggregateTooWide {
                terms: 2000,
                limit: 1024,
            }
    });
}

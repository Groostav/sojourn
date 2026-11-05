//! Ast corpus, ported from `BabelExpressionFixture.kt`.
//!
//! Every case runs through [`run`], which mirrors the Kotlin `runExprTest`
//! helper: compile, check the compile-time metadata, then evaluate twice and
//! require both results to agree.
//!
//! Assertions are exact by default. Only cases that route through libm carry a
//! `tolerance` — blanket tolerance would mask real arithmetic regressions in
//! the ~55 cases that are exact.

mod common;

use std::collections::BTreeSet;

/// Java's `Double.MIN_NORMAL`, which Babel uses as the epsilon nudge that makes
/// strict inequalities representable under the `<= 0 is true` convention.
const EPSILON: f64 = f64::MIN_POSITIVE;

struct Case {
    expr: String,
    expected: f64,
    vars: Vec<(String, f64)>,
    tolerance: Option<f64>,
    dynamic_lookup: bool,
    boolean: bool,
    statics: Option<Vec<String>>,
}

impl Case {
    fn new(expr: &str, expected: f64) -> Self {
        Self {
            expr: expr.to_owned(),
            expected,
            vars: Vec::new(),
            tolerance: None,
            dynamic_lookup: false,
            boolean: false,
            statics: None,
        }
    }

    fn vars<I, S>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (S, f64)>,
        S: Into<String>,
    {
        self.vars = vars.into_iter().map(|(n, v)| (n.into(), v)).collect();
        self
    }

    /// Marks a case as routing through libm, so it is compared within `t`.
    fn tol(mut self, t: f64) -> Self {
        self.tolerance = Some(t);
        self
    }

    fn dynamic(mut self) -> Self {
        self.dynamic_lookup = true;
        self
    }

    fn boolean(mut self) -> Self {
        self.boolean = true;
        self
    }

    /// Overrides the default expectation that every supplied variable is
    /// statically referenced.
    fn statics<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.statics = Some(names.into_iter().map(Into::into).collect());
        self
    }
}

fn run(c: Case) {
    let names: Vec<&str> = c.vars.iter().map(|(n, _)| n.as_str()).collect();
    let expr = sojourn::compile(&c.expr, &names)
        .unwrap_or_else(|e| panic!("compile failed for {:?}: {e:#}", c.expr));

    assert_eq!(
        expr.uses_dynamic_lookup(),
        c.dynamic_lookup,
        "uses_dynamic_lookup for {:?}",
        c.expr
    );
    assert_eq!(
        expr.is_constraint(),
        c.boolean,
        "is_constraint for {:?}",
        c.expr
    );

    let expected_statics: BTreeSet<&str> = match &c.statics {
        Some(names) => names.iter().map(String::as_str).collect(),
        None => c.vars.iter().map(|(n, _)| n.as_str()).collect(),
    };
    assert_eq!(
        expr.references(),
        expected_statics,
        "references for {:?}",
        c.expr
    );

    let inputs: Vec<(&str, f64)> = c.vars.iter().map(|(n, v)| (n.as_str(), *v)).collect();

    let first = common::eval_one(&c.expr, &inputs)
        .unwrap_or_else(|e| panic!("evaluation failed for {:?}: {e}", c.expr));
    // The Kotlin fixture evaluates twice and requires agreement, guarding
    // against compiled state being mutated by evaluation.
    let second = common::eval_one(&c.expr, &inputs)
        .unwrap_or_else(|e| panic!("second evaluation failed for {:?}: {e}", c.expr));
    assert_eq!(first, second, "second evaluation differed for {:?}", c.expr);

    match c.tolerance {
        None => assert_eq!(first, c.expected, "result for {:?}", c.expr),
        Some(t) => assert!(
            (first - c.expected).abs() <= t,
            "result for {:?}: expected {} +/- {t}, got {first}",
            c.expr,
            c.expected
        ),
    }
}

macro_rules! case {
    ($name:ident: $body:expr) => {
        #[test]
        fn $name() {
            run($body);
        }
    };
}

// ---------------------------------------------------------------- arithmetic

case!(add: Case::new("3 + 4", 3.0 + 4.0));
case!(subtract: Case::new("3 - 4", 3.0 - 4.0));
case!(multiply: Case::new("3 * 4", 3.0 * 4.0));
case!(divide: Case::new("3 / 4", 3.0 / 4.0));
case!(raise: Case::new("3 ^ 4", 81.0));
// A real exponent is the one power no backend turns into multiplication: the
// literal form pins the constant folder, the variable form the tape's `powf`.
case!(raise_real: Case::new("2 ^ 1.234", 2.0_f64.powf(1.234)).tol(1e-12));
case!(raise_real_variable: Case::new("x1 ^ 1.234", 2.0_f64.powf(1.234)).vars([("x1", 2.0)]).tol(1e-12));
case!(modulo: Case::new("4 % 3", 4.0 % 3.0));
case!(modulo_negative: Case::new("-4 % 3", -4.0 % 3.0));

// ------------------------------------------------------------- sum and prod

case!(sum_single_term: Case::new("sum(1, 1, i -> i)", 1.0));
case!(prod_doubled: Case::new("prod(1, 5, i -> 2*i)", 2.0 * 4.0 * 6.0 * 8.0 * 10.0));
case!(sum_identity_1_to_5: Case::new("sum(1, 5, i -> i)", 1.0 + 2.0 + 3.0 + 4.0 + 5.0));
case!(prod_identity_1_to_4: Case::new("prod(1, 4, i -> i)", 1.0 * 2.0 * 3.0 * 4.0));
case!(sum_negative_range: Case::new("sum(-5, -2, i -> i)", -14.0));

// A literal subscript — including one an aggregate unrolls into — is the
// variable it names: read, referenced, and not a dynamic lookup.
case!(sum_with_dynamic_offset:
    Case::new("sum(2, 2, i -> var[i-1])", 2.0)
        .vars([("x1", 2.0), ("x2", 3.0), ("x3", 4.0)])
        .statics(["x1"]));

case!(sum_over_extra_variable:
    Case::new("sum(1, 2, i -> x1)", 1.0 + 1.0)
        .vars([("x1", 1.0), ("x2", 5.0)])
        .statics(["x1"]));

// --------------------------------------------------------------------- libm

case!(two_pi: Case::new("2 * pi", 2.0 * std::f64::consts::PI));
case!(negate_eulers_e: Case::new("-e", -std::f64::consts::E));
case!(natural_log: Case::new("ln(20)", 20.0_f64.ln()).tol(1e-12));
case!(sin_cubed: Case::new("sin(21)^3", 21.0_f64.sin().powf(3.0)).tol(1e-12));
case!(absolute_value: Case::new("abs(-4)", 4.0));
case!(ceiling: Case::new("ceil(2.7)", 3.0));
case!(floor: Case::new("floor(2.7)", 2.0));
case!(log_base_2_of_16: Case::new("log(2,16)", 4.0).tol(1e-12));
case!(signum: Case::new("sgn(-1)", -1.0));

// --------------------------------------------------- unary minus ambiguity

case!(minus_spaced: Case::new("-3 - -3", -3.0 - -3.0));
case!(minus_unspaced: Case::new("-3--3", -3.0 - -3.0));

// ---------------------------------------------------------------- variables

case!(two_variables: Case::new("x1 + x2", 2.0 + 3.0).vars([("x1", 2.0), ("x2", 3.0)]));
case!(leading_underscore: Case::new("_name", 4.0).vars([("_name", 4.0)]));
case!(embedded_underscore: Case::new("x_1", 4.0).vars([("x_1", 4.0)]));
case!(greek_pi_as_name:
    Case::new("π", std::f64::consts::PI).vars([("π", std::f64::consts::PI)]));
case!(han_identifier:
    Case::new("大_da_dai_meaning_big", 1e250).vars([("大_da_dai_meaning_big", 1e250)]));
case!(emoji_identifier: Case::new("☕", 42.0).vars([("☕", 42.0)]));
case!(han_identifier_short: Case::new("测试", 42.0).vars([("测试", 42.0)]));

// ------------------------------------------------------------------ indexer

case!(index_first:
    Case::new("var[1]", 0.0)
        .vars([("x1", 0.0)]));

case!(index_second:
    Case::new("var[2]", 2.0)
        .vars([("input-sds", 1.0), ("input-SDA", 2.0), ("input-SDJA", 3.0)])
        .statics(["input-SDA"]));

case!(index_third:
    Case::new("var[3]", 3.0)
        .vars([("input-sds", 1.0), ("input-SDA", 2.0), ("input-SDJA", 3.0)])
        .statics(["input-SDJA"]));

case!(index_mixed_with_names:
    Case::new("x + var[2] + z", 1.0 + 1.1 + 1.01)
        .vars([("x", 1.0), ("y", 1.1), ("z", 1.01)]));

// A computed subscript is what "dynamic lookup" means now: which variable it
// reads depends on the point, so `x2` is read here and referenced nowhere.
case!(index_computed:
    Case::new("var[floor(x1)]", 7.0)
        .vars([("x1", 2.0), ("x2", 7.0)])
        .dynamic()
        .statics(["x1"]));

// ------------------------------------------------------------------ boolean
//
// Positive results are false, results <= 0 are true.

case!(gt_equal_operands: Case::new("6 > 6", EPSILON).boolean());
case!(lt_true: Case::new("4 < 6", 4.0 - 6.0).boolean());

case!(lt_equal_large: Case::new("1.0e200 < 1.0e200", EPSILON).boolean());
case!(gt_equal_large: Case::new("1.0e200 > 1.0e200", EPSILON).boolean());
case!(lt_large_gap: Case::new("1.0e200 < 1.0e199", 9.0e199).boolean());
case!(lteq_equal_large: Case::new("1.0e200 <= 1.0e200", 0.0).boolean());
case!(gteq_equal_large: Case::new("1.0e200 >= 1.0e200", 0.0).boolean());

// ------------------------------------------------------------- implication
//
// `p implies q` is `min(not p, q)`: true where the premise is false or the
// conclusion true. `not p` is the *complemented* comparison, not `-p`, so the
// strict nudge lands on the right side of a tie: `not (x > y)` is `x <= y`,
// `x - y`; `not (x >= y)` is `x < y`, `x - y + EPSILON`.

case!(implies_premise_and_conclusion_true:
    // min(3 - 2, 3 * 2 - 20 + EPSILON)
    Case::new("x > y implies x * y < 20", -14.0)
        .vars([("x", 3.0), ("y", 2.0)])
        .boolean());

case!(implies_premise_true_conclusion_false:
    // min(6 - 4, 6 * 4 - 20 + EPSILON): false, by the premise's margin
    Case::new("x > y implies x * y < 20", 2.0)
        .vars([("x", 6.0), ("y", 4.0)])
        .boolean());

case!(implies_premise_false_is_vacuous:
    // min(1 - 2, 1 - 0 + EPSILON): the conclusion is false and it holds
    Case::new("x > y implies x < 0", -1.0)
        .vars([("x", 1.0), ("y", 2.0)])
        .boolean());

case!(implies_strict_premise_at_a_tie_is_false:
    // x > y is false at x == y, so the implication holds: min(1 - 1, 1 + EPSILON)
    Case::new("x > y implies x < 0", 0.0)
        .vars([("x", 1.0), ("y", 1.0)])
        .boolean());

case!(implies_non_strict_premise_at_a_tie_is_true:
    // x >= y is true at x == y, and the conclusion is false: min(EPSILON, 1 + EPSILON)
    Case::new("x >= y implies x < 0", EPSILON)
        .vars([("x", 1.0), ("y", 1.0)])
        .boolean());

case!(implies_band_premise_inside:
    // not (x == y +/- 0.5) is x < y - 0.5 or x > y + 0.5:
    // min(1 - 0.75 + EPSILON, 1.75 - 1 + EPSILON) = 0.25; the conclusion is 1 + EPSILON
    Case::new("x == y +/- 0.5 implies x < 0", 0.25)
        .vars([("x", 1.0), ("y", 1.25)])
        .boolean());

case!(implies_band_premise_outside:
    // min(1 - 2.5 + EPSILON, 3.5 - 1 + EPSILON) = -1.5
    Case::new("x == y +/- 0.5 implies x < 0", -1.5)
        .vars([("x", 1.0), ("y", 3.0)])
        .boolean());

case!(implies_band_conclusion:
    // min(1 - 0, max(0.75 - 1, 1 - 1.75))
    Case::new("x > 0 implies x == y +/- 0.5", -0.25)
        .vars([("x", 1.0), ("y", 1.25)])
        .boolean());

case!(implies_nested_in_the_conclusion:
    // inner min(2 - 0, 1 - 2 + EPSILON) = -1; outer min(1 - 0, -1)
    Case::new("x > 0 implies (y > 0 implies x < y)", -1.0)
        .vars([("x", 1.0), ("y", 2.0)])
        .boolean());

case!(implies_nested_in_the_premise:
    // not (p implies q) is p and not q: max(0 - 1 + EPSILON, -1 - 0) = -1;
    // the conclusion is 1 - (-1) + EPSILON = 2; min(-1, 2)
    Case::new("(x > 0 implies y > 0) implies x < y", -1.0)
        .vars([("x", 1.0), ("y", -1.0)])
        .boolean());

case!(implies_binds_looser_than_comparison:
    // min((3 + 1) - 2, 3 * 2 - 20 + EPSILON) — the arithmetic stays on its side
    Case::new("x + 1 > y implies x * y < 20", -14.0)
        .vars([("x", 3.0), ("y", 2.0)])
        .boolean());

/// A premise `rewrite::invert_monotone` turns into a conjunction (`ln(x) < 1`
/// is `x < e` with the domain's floor `x >= 0`) is negated by De Morgan,
/// either bound failing being enough. The residual's value is the rewrite's
/// business, so this pins the verdict on either side of each bound rather
/// than a number.
#[test]
fn implies_negates_an_inverted_premise_by_de_morgan() {
    for (x, y, holds) in [
        (1.0, 1.0, false), // premise true, conclusion false
        (1.0, -1.0, true), // premise true, conclusion true
        (9.0, 1.0, true),  // past the bound: premise false, vacuous
        (-1.0, 1.0, true), // below the floor: premise false, and no ln to fault
    ] {
        let residual = common::eval_one("ln(x) < 1 implies y < 0", &[("x", x), ("y", y)]).unwrap();
        assert_eq!(residual <= 0.0, holds, "x = {x}, y = {y}: {residual}");
    }
}

// ------------------------------------------------------------------ nesting

case!(nested_sum: Case::new("sum(3, 6, i -> sum(3, 3, j -> j + i))", 30.0));
case!(nested_prod: Case::new("prod(3, 6, i -> prod(3, 3, j -> j + i))", 3024.0));

// -------------------------------------------------------------- name hiding

case!(lambda_param_shadows_global:
    Case::new("sum(1, 3, x1 -> x1) + x1", 6.0 + 1000.0).vars([("x1", 1000.0)]));

case!(sibling_lambdas_reuse_name:
    Case::new("sum(1, 2, i -> i) + sum(3, 4, i -> i)", 3.0 + 7.0));

case!(identical_sibling_lambdas:
    Case::new("sum(1, 3, x -> x) + sum(1,3, x -> x)", 6.0 + 6.0));

case!(nested_lambda_shadows_outer:
    Case::new("prod(1, 2, i -> i + sum(1000, 1000, i -> i))", 1001.0 * 1002.0));

// -------------------------------------------------------------- integration

case!(rosenbrock_10:
Case::new(
    "sum(2, 10, i -> 100*(var[i]-var[i-1]^2)^2 + (1-var[i-1])^2)",
    271_194.0,
)
.vars([
    ("x1", 2.0), ("x2", 3.0), ("x3", 4.0), ("x4", 5.0), ("x5", 6.0),
    ("x6", 6.0), ("x7", 2.0), ("x8", 3.0), ("x9", 4.0), ("x10", 5.0),
]));

case!(rastrigin_10:
    Case::new("10*10+sum(1, 10, i -> var[i]^2 - 10*cos(2*pi*var[i]))", 180.0)
        .vars([
            ("x1", 2.0), ("x2", 3.0), ("x3", 4.0), ("x4", -5.0), ("x5", 6.0),
            ("x6", -6.0), ("x7", 2.0), ("x8", 3.0), ("x9", 4.0), ("x10", 5.0),
        ])
        .tol(1e-9));

case!(from_the_manual:
    Case::new("l1_height * l2_width >= 7.0E2", 100.0)
        .vars([("l1_height", 30.0), ("l2_width", 20.0)])
        .boolean());

#[test]
fn large_sum_dynamic_access_no_whitespace() {
    run(Case::new(
        "sum(1,50,i->((var[2*i-1]^2-var[2*i])^2+(var[2*i-1]-1)^2))",
        0.0,
    )
    .vars((1..=100).map(|i| (format!("x{i}"), 1.0))));
}

// ------------------------------------------------------- statements & scope

case!(multi_statement:
    Case::new("var x = x1;\n x + x1", 1.0 + 1.0).vars([("x1", 1.0)]));

case!(explicit_return:
    Case::new("var x = x1;\nreturn x + x1;\n", 1.1 + 1.1).vars([("x1", 1.1)]));

case!(equality_with_tolerance:
    // x1 == x2 +/- 0.15 becomes the binding upper bound: x1 - (x2 + 0.15) <= 0
    Case::new("x1 == x2 +/- 0.15", 1.0 - (0.9 + 0.15))
        .vars([("x1", 1.0), ("x2", 0.9)])
        .boolean());

case!(multiline_lambda: Case::new("sum(1, 2, i -> \n  i \n)\n", 1.0 + 2.0));

case!(local_binding_inside_lambda:
    Case::new("sum(1, 3, i -> \n    var x = i + i;\n    x;\n)", 1.0 + 1.0 + 2.0 + 2.0 + 3.0 + 3.0));

case!(multiple_statements_inside_lambda: Case::new(
    "prod(1, 3, i -> \n    var first = i + 2;\n    var second = i - 1;\n    return first - second;\n)",
    ((1.0 + 2.0) - (1.0 - 1.0)) * ((2.0 + 2.0) - (2.0 - 1.0)) * ((3.0 + 2.0) - (3.0 - 1.0)),
));

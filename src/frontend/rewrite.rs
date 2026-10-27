//! AST-to-AST rewrites.
//!
//! Each pass is `fn(Program) -> Program`, taking ownership. Passing trees by
//! value means unchanged subtrees are *moved* rather than cloned or refcounted,
//! which is why the AST needs no persistent collections to be effectively
//! immutable.
//!
//! # The boolean convention
//!
//! Babel has no boolean values at run time. A comparison lowers to arithmetic
//! whose *sign* carries the truth value: `<= 0` is true, `> 0` is false. That is
//! the canonical `g(x) <= 0` constraint form, so a violated constraint reports
//! how badly it was violated rather than merely that it was.

use std::f64::consts::FRAC_PI_2;

use crate::ast::{
    AggregateKind, Assignment, BinaryOp, Block, CompareOp, Expr, GlobalId, Kind, LocalSlot,
    Program, UnaryOp, to_index,
};
use crate::diagnostics::{BoundKind, Fault, ProblemKind, Span};
use crate::{Ast, Schema};
/// The canonical form of a parsed tree: what every consumer reads.
///
/// The passes in the order they must run — [`fold_constants`], so that a
/// statically known value *is* a literal for everything after;
/// [`invert_monotone`], while comparisons exist to be matched on;
/// [`unroll_aggregates`], once every bound is a literal; then
/// [`collect_powers`], once every repeated factor an aggregate hid is spelled
/// out. Each is meaning-preserving; together they are the one place the tree
/// is normalised, so that two spellings of a thing reach the evaluator and the
/// narrower as one tree. What a single consumer wants of the tree beyond this
/// — the evaluator's [`unroll_powers`] — is that consumer's to apply.
///
/// # Errors
/// [`fold_constants`]'s and [`unroll_aggregates`]'s.
pub(crate) fn canonicalize(program: Program) -> Result<Program, Vec<Fault>> {
    let program = fold_constants(program)?;
    check_subscripts(&program)?;
    let program = invert_monotone(program);
    let program = unroll_aggregates(program)?;
    let program = collect_powers(program);
    Ok(program)
}

/// Replaces every subexpression made only of literals with the value it works
/// out to.
///
/// Runs first, and the reason it runs first is that it makes "is this constant?"
/// stop being a question anywhere else. After this pass a statically known value
/// **is** a [`Kind::Literal`], so [`unroll_aggregates`] can ask about its bounds
/// with a pattern match and [`invert_monotone`] can ask about a comparison's
/// other side the same way. Neither needs an evaluator of its own, and there
/// used to be one — `ast::const_eval`, which this pass replaced.
///
/// Bottom-up, so the work is linear: children are folded before their parent is
/// looked at, which means a parent only ever has to combine literals rather than
/// walk a subtree. Values come from [`crate::ast::UnaryOp::apply`] and
/// [`crate::ast::BinaryOp::apply`], the same functions the evaluator calls, so
/// folding cannot change a result — `corpus.rs` pins several constant
/// expressions by value and is the guard on that.
///
/// # Errors
/// A constant subexpression that works out to NaN or an infinity. See
/// [`ProblemKind::NonFiniteConstant`] for why that is refused rather than
/// folded.
pub(crate) fn fold_constants(program: Program) -> Result<Program, Vec<Fault>> {
    let Program { body, frame_size } = program;
    Ok(Program {
        body: fold_block(body)?,
        frame_size,
    })
}

fn fold_block(block: Block) -> Result<Block, Vec<Fault>> {
    let Block {
        assignments,
        result,
    } = block;
    Ok(Block {
        assignments: assignments
            .into_iter()
            .map(|Assignment { slot, value, span }| {
                Ok(Assignment {
                    slot,
                    value: fold_expr(value)?,
                    span,
                })
            })
            .collect::<Result<_, Vec<Fault>>>()?,
        result: fold_expr(result)?,
    })
}

fn fold_expr(expr: Expr) -> Result<Expr, Vec<Fault>> {
    let Expr { kind, span } = expr;

    let kind = match kind {
        Kind::Unary { op, arg } => {
            let arg = fold_descend(*arg)?;
            match arg.kind {
                Kind::Literal(value) => Kind::Literal(op.apply(value)),
                _ => Kind::Unary { op, arg },
            }
        }
        Kind::Binary { op, lhs, rhs } => {
            let (lhs, rhs) = (fold_descend(*lhs)?, fold_descend(*rhs)?);
            match (&lhs.kind, &rhs.kind) {
                (Kind::Literal(left), Kind::Literal(right)) => {
                    Kind::Literal(op.apply(*left, *right))
                }
                _ => Kind::Binary { op, lhs, rhs },
            }
        }

        Kind::Compare { op, lhs, rhs } => Kind::Compare {
            op,
            lhs: fold_descend(*lhs)?,
            rhs: fold_descend(*rhs)?,
        },
        Kind::NearEq {
            lhs,
            rhs,
            tolerance,
        } => Kind::NearEq {
            lhs: fold_descend(*lhs)?,
            rhs: fold_descend(*rhs)?,
            tolerance,
        },
        Kind::And { terms } => Kind::And {
            terms: terms
                .into_iter()
                .map(fold_expr)
                .collect::<Result<_, Vec<Fault>>>()?,
        },
        // A literal subscript that no schema could satisfy — zero, negative,
        // or not a whole number — is refused here, where the aggregate bounds
        // are, rather than on the first row that reaches it. Which parameters
        // *exist* needs a schema and stays with `resolve_subscripts`.
        Kind::DynamicIndex(index) => {
            let index = fold_descend(*index)?;
            if let Kind::Literal(value) = index.kind {
                let kind = match to_index(value) {
                    None => Some(ProblemKind::DynamicIndexNotAnInteger { value }),
                    Some(0) => Some(ProblemKind::ZeroIndex),
                    Some(requested_1index) if requested_1index < 0 => {
                        Some(ProblemKind::NegativeDynamicIndex { requested_1index })
                    }
                    Some(_) => None,
                };
                if let Some(kind) = kind {
                    return Err(vec![Fault {
                        kind,
                        span: index.span,
                    }]);
                }
            }
            Kind::DynamicIndex(index)
        }
        Kind::Aggregate {
            kind,
            lower,
            upper,
            param,
            body,
        } => Kind::Aggregate {
            kind,
            lower: fold_descend(*lower)?,
            upper: fold_descend(*upper)?,
            param,
            body: Box::new(fold_block(*body)?),
        },
        Kind::Block(block) => Kind::Block(Box::new(fold_block(*block)?)),

        // Unrolling runs last, so nothing has produced one of these yet. Folding
        // an unrolled aggregate's terms would be worth real time on the
        // evaluator and is deliberately left to the tape work — moving this pass
        // after unrolling would put it after the boolean rewrite too, where it
        // would collapse a constant comparison onto the strictness epsilon.
        Kind::Fold { kind, terms } => Kind::Fold {
            kind,
            terms: terms
                .into_iter()
                .map(fold_expr)
                .collect::<Result<_, Vec<Fault>>>()?,
        },

        // Literals are checked as well as folded results: babel's `FLOAT` token
        // admits `1.0e400`, which parses straight to an infinity, and refusing
        // `1.0e400 * 1.0` while accepting `1.0e400` would be a rule with a hole
        // in it.
        leaf @ (Kind::Literal(_) | Kind::Global(_) | Kind::Local(_)) => leaf,
    };

    if let Kind::Literal(value) = kind
        && !value.is_finite()
    {
        return Err(vec![Fault {
            kind: ProblemKind::NonFiniteConstant { value },
            span,
        }]);
    }

    Ok(Expr { kind, span })
}

/// Mirrors `descend` below: takes the child by value so the caller's box is
/// released rather than passed through.
fn fold_descend(expr: Expr) -> Result<Box<Expr>, Vec<Fault>> {
    Ok(Box::new(fold_expr(expr)?))
}

// ------------------------------------------------------------------- subscripts

/// Refuses every subscript that is not a whole number by construction.
///
/// After [`fold_constants`], so a literal subscript has been folded and
/// value-checked already, and before [`unroll_aggregates`], so an aggregate's
/// parameter is still a local the judgement can see and the span is the one
/// the user wrote. The judgement itself is [`ast::is_integral`]; this walk
/// only supplies its `env` — a parameter is integral inside its aggregate, a
/// `var` when its value was — and collects one fault per offending
/// subscript, so a round trip fixes all of them. Descends into a subscript
/// too: `var[floor(var[x1])]` has an inner one to judge.
fn check_subscripts(program: &Program) -> Result<(), Vec<Fault>> {
    let mut faults = Vec::new();
    let mut env = vec![false; program.frame_size as usize];
    check_block(&program.body, &mut env, &mut faults);
    if faults.is_empty() {
        Ok(())
    } else {
        Err(faults)
    }
}

fn check_block(block: &Block, env: &mut Vec<bool>, faults: &mut Vec<Fault>) {
    for assignment in &block.assignments {
        check_expr(&assignment.value, env, faults);
        env[assignment.slot.index()] = crate::ast::is_integral(&assignment.value, env);
    }
    check_expr(&block.result, env, faults);
}

fn check_expr(expr: &Expr, env: &mut Vec<bool>, faults: &mut Vec<Fault>) {
    match &expr.kind {
        Kind::DynamicIndex(subscript) => {
            if !crate::ast::is_integral(subscript, env) {
                faults.push(Fault {
                    kind: ProblemKind::SubscriptNotIntegral,
                    span: subscript.span,
                });
            }
            check_expr(subscript, env, faults);
        }
        Kind::Literal(_) | Kind::Global(_) | Kind::Local(_) => {}
        Kind::Unary { arg, .. } => check_expr(arg, env, faults),
        Kind::Binary { lhs, rhs, .. }
        | Kind::Compare { lhs, rhs, .. }
        | Kind::NearEq { lhs, rhs, .. } => {
            check_expr(lhs, env, faults);
            check_expr(rhs, env, faults);
        }
        Kind::And { terms } | Kind::Fold { terms, .. } => {
            for term in terms {
                check_expr(term, env, faults);
            }
        }
        Kind::Block(block) => check_block(block, env, faults),
        Kind::Aggregate {
            lower,
            upper,
            param,
            body,
            ..
        } => {
            check_expr(lower, env, faults);
            check_expr(upper, env, faults);
            env[param.index()] = true;
            check_block(body, env, faults);
        }
    }
}

/// Turns `var[1]` into an ordinary reference to the schema's first variable.
///
/// # Why this one takes a schema when no other rewrite does
///
/// Because it is the only one that needs a *name*. [`Kind::Global`] holds an
/// index into the expression's own symbol list - the AST is built before any
/// schema exists, which [`Ast`] documents - while `var[i]` holds a one-based
/// index into the **schema**, in declaration order. Nothing inside `parse` can
/// bridge those: it has no idea what variable 1 is.
///
/// So this runs later, at the first moment a schema exists: in
/// [`eval::bind`](crate::eval::bind), and before it in
/// [`ConstraintSystem::new`](crate::ConstraintSystem::new), which keeps the
/// resolved tree for the search.
///
/// # What it buys
///
/// A subscript is an indirection that serves nobody downstream. `cvg::classify`
/// gave up on the whole constraint rather than reason about one, so
/// `1.5 == var[1] + var[2]` could never be driven, and `cvg::interval` can
/// narrow nothing through one. Resolving here means neither has to care, and
/// `Ast::contains_dynamic_lookup` stops meaning "has a subscript" and starts
/// meaning "has one nothing could resolve" - which is what a caller actually
/// needs before pruning columns it believes unreferenced.
///
/// A *computed* subscript - `var[n]` - is left exactly as it was and keeps the
/// flag true. Which variable it reads depends on the point, so there is nothing
/// static to resolve, and interval evaluation answers `ENTIRE` to it.
///
/// # Errors
/// A [`Fault`] at the subscript — `DynamicIndexOutOfBounds` — when a literal
/// falls outside the schema: settled the moment the schema exists, where it
/// used to surface once per evaluation.
pub(crate) fn resolve_subscripts(ast: Ast, schema: &Schema) -> Result<Ast, Fault> {
    if !ast.contains_dynamic_lookup {
        return Ok(ast);
    }

    let Ast {
        source,
        program: Program { body, frame_size },
        mut symbols,
        is_constraint,
        ..
    } = ast;

    let body = resolve_block(body, schema, &mut symbols)?;
    // Recomputed rather than cleared: a computed subscript may have survived,
    // and saying otherwise would be the lie the flag exists to prevent.
    let contains_dynamic_lookup = holds_subscript(&body);

    Ok(Ast {
        source,
        program: Program { body, frame_size },
        symbols,
        contains_dynamic_lookup,
        is_constraint,
    })
}

fn resolve_block(block: Block, schema: &Schema, symbols: &mut Vec<String>) -> Result<Block, Fault> {
    let Block {
        assignments,
        result,
    } = block;
    Ok(Block {
        assignments: assignments
            .into_iter()
            .map(|Assignment { slot, value, span }| {
                Ok(Assignment {
                    slot,
                    value: resolve_expr(value, schema, symbols)?,
                    span,
                })
            })
            .collect::<Result<_, _>>()?,
        result: resolve_expr(result, schema, symbols)?,
    })
}

fn resolve_expr(expr: Expr, schema: &Schema, symbols: &mut Vec<String>) -> Result<Expr, Fault> {
    let Expr { kind, span } = expr;

    let kind = match kind {
        Kind::DynamicIndex(subscript) => {
            let Kind::Literal(value) = subscript.kind else {
                // Computed, and staying that way.
                let inner = resolve_expr(*subscript, schema, symbols)?;
                return Ok(Expr {
                    kind: Kind::DynamicIndex(Box::new(inner)),
                    span,
                });
            };
            let out_of_range = |requested_1index| Fault {
                kind: ProblemKind::DynamicIndexOutOfBounds {
                    requested_1index,
                    available: schema.layout().inputs,
                },
                span: subscript.span,
            };
            // Folding refused anything that is not a whole number before this
            // pass runs; the arm is here so the match is total, not reachable.
            let requested = to_index(value).ok_or(Fault {
                kind: ProblemKind::DynamicIndexNotAnInteger { value },
                span: subscript.span,
            })?;
            // Inputs only: a row may carry intermediates after them, and a
            // subscript is not a way to reach one.
            let name = usize::try_from(requested - 1)
                .ok()
                .filter(|position| *position < schema.layout().inputs)
                .and_then(|position| schema.names().get(position))
                .ok_or_else(|| out_of_range(requested))?
                .clone();

            // The constraint may never have named this variable, in which case
            // it gains a symbol. That makes `statically_referenced_symbols`
            // report what the expression *reads* rather than what it spells.
            let index = symbols
                .iter()
                .position(|held| *held == name)
                .unwrap_or_else(|| {
                    symbols.push(name);
                    symbols.len() - 1
                });
            let index = u32::try_from(index).expect("fewer than four billion symbols");
            Kind::Global(GlobalId::from_index(index))
        }

        Kind::Unary { op, arg } => Kind::Unary {
            op,
            arg: Box::new(resolve_expr(*arg, schema, symbols)?),
        },
        Kind::Binary { op, lhs, rhs } => Kind::Binary {
            op,
            lhs: Box::new(resolve_expr(*lhs, schema, symbols)?),
            rhs: Box::new(resolve_expr(*rhs, schema, symbols)?),
        },
        Kind::Compare { op, lhs, rhs } => Kind::Compare {
            op,
            lhs: Box::new(resolve_expr(*lhs, schema, symbols)?),
            rhs: Box::new(resolve_expr(*rhs, schema, symbols)?),
        },
        Kind::NearEq {
            lhs,
            rhs,
            tolerance,
        } => Kind::NearEq {
            lhs: Box::new(resolve_expr(*lhs, schema, symbols)?),
            rhs: Box::new(resolve_expr(*rhs, schema, symbols)?),
            tolerance,
        },
        Kind::And { terms } => Kind::And {
            terms: terms
                .into_iter()
                .map(|term| resolve_expr(term, schema, symbols))
                .collect::<Result<_, _>>()?,
        },
        Kind::Fold { kind, terms } => Kind::Fold {
            kind,
            terms: terms
                .into_iter()
                .map(|term| resolve_expr(term, schema, symbols))
                .collect::<Result<_, _>>()?,
        },
        Kind::Block(block) => Kind::Block(Box::new(resolve_block(*block, schema, symbols)?)),
        Kind::Aggregate {
            kind,
            lower,
            upper,
            param,
            body,
        } => Kind::Aggregate {
            kind,
            lower: Box::new(resolve_expr(*lower, schema, symbols)?),
            upper: Box::new(resolve_expr(*upper, schema, symbols)?),
            param,
            body: Box::new(resolve_block(*body, schema, symbols)?),
        },

        leaf @ (Kind::Literal(_) | Kind::Global(_) | Kind::Local(_)) => leaf,
    };

    Ok(Expr { kind, span })
}

/// Whether any subscript survives in `block`.
pub(crate) fn holds_subscript(block: &Block) -> bool {
    block
        .iter_preorder()
        .any(|node| matches!(node.kind, Kind::DynamicIndex(_)))
}

/// Rewrites `f(u) op c` into a comparison on `u`, for the strictly monotone `f`
/// that no solver will reason about.
///
/// `2 < ln(x1)` becomes `x1 > e^2`; `20 > 2^x5` becomes `x5 < log2(20)`. The
/// bound is computed here, in `f64`, so what reaches a backend is linear in
/// `u` and wants no logarithm inverted at all. Both are real constraints in
/// the CVG corpus and are the reason this was written — the SMT solver of the
/// day had no logarithm under any spelling, and inverting through `^` did not
/// work either, since a variable exponent answered `unknown` even with the
/// other side pinned.
///
/// It outlives its own motivation, too: restricting `a ^ b` to an integer `b`
/// would make `2^x5` a compile error, and this rewrites it away first.
///
/// Infallible. Anything it cannot invert it leaves exactly as it found it, to be
/// reported through `Document::untranslated` as before.
pub(crate) fn invert_monotone(program: Program) -> Program {
    let Program { body, frame_size } = program;
    Program {
        body: invert_block(body),
        frame_size,
    }
}

fn invert_block(block: Block) -> Block {
    let Block {
        assignments,
        result,
    } = block;
    Block {
        assignments: assignments
            .into_iter()
            .map(|Assignment { slot, value, span }| Assignment {
                slot,
                value: invert_expr(value),
                span,
            })
            .collect(),
        result: invert_expr(result),
    }
}

fn invert_expr(node: Expr) -> Expr {
    let Expr { kind, span } = node;

    let kind = match kind {
        Kind::Compare { op, lhs, rhs } => {
            return invert_comparison(op, invert_expr(*lhs), invert_expr(*rhs), span);
        }

        Kind::Unary { op, arg } => Kind::Unary {
            op,
            arg: Box::new(invert_expr(*arg)),
        },
        Kind::Binary { op, lhs, rhs } => Kind::Binary {
            op,
            lhs: Box::new(invert_expr(*lhs)),
            rhs: Box::new(invert_expr(*rhs)),
        },
        Kind::NearEq {
            lhs,
            rhs,
            tolerance,
        } => Kind::NearEq {
            lhs: Box::new(invert_expr(*lhs)),
            rhs: Box::new(invert_expr(*rhs)),
            tolerance,
        },
        Kind::And { terms } => Kind::And {
            terms: terms.into_iter().map(invert_expr).collect(),
        },
        Kind::DynamicIndex(index) => Kind::DynamicIndex(Box::new(invert_expr(*index))),
        Kind::Aggregate {
            kind,
            lower,
            upper,
            param,
            body,
        } => Kind::Aggregate {
            kind,
            lower: Box::new(invert_expr(*lower)),
            upper: Box::new(invert_expr(*upper)),
            param,
            body: Box::new(invert_block(*body)),
        },
        Kind::Block(block) => Kind::Block(Box::new(invert_block(*block))),
        Kind::Fold { kind, terms } => Kind::Fold {
            kind,
            terms: terms.into_iter().map(invert_expr).collect(),
        },

        leaf @ (Kind::Literal(_) | Kind::Global(_) | Kind::Local(_)) => leaf,
    };

    Expr { kind, span }
}

/// A strictly monotone function, and what it takes to invert a comparison
/// against one.
struct Monotone {
    /// `f`-inverse, to be applied to the constant side. A closure rather than a
    /// `fn` pointer because `a ^ u` has to carry its base.
    inverse: Box<dyn Fn(f64) -> f64>,
    /// Whether `f` preserves the comparison. A decreasing one reverses it.
    increasing: bool,
    /// The closure of the values `f` can produce.
    ///
    /// Checked before inverting, and not an optimisation: `atan(x) > 2` is
    /// unsatisfiable, and `x > tan(2)` — which is `x > -2.18` — is very much not
    /// the same claim.
    range: (f64, f64),
    /// The values `f` accepts, when it does not accept all of them. Every
    /// entry in the table is either total or bounded below, so a floor and
    /// whether it is attainable says the whole of it.
    domain_floor: Option<(f64, CompareOp)>,
}

/// The invertible application at `expr`, if it is one, paired with its argument.
///
/// The argument is a whole subtree, not a variable: `ln(x1 + x2) > 2` inverts to
/// `x1 + x2 > e^2` as readily as the one-variable case.
///
/// `abs`, `sqr`, `sgn` and the trigonometric functions are absent because they
/// are not monotone. `cos` on `[0, pi]` is, but knowing that needs a bound on
/// the argument, which is causalization's problem rather than this pass's.
fn monotone(expr: &Expr) -> Option<(Monotone, &Expr)> {
    const ALL: (f64, f64) = (f64::NEG_INFINITY, f64::INFINITY);

    fn total(inverse: impl Fn(f64) -> f64 + 'static, range: (f64, f64)) -> Monotone {
        Monotone {
            inverse: Box::new(inverse),
            increasing: true,
            range,
            domain_floor: None,
        }
    }
    /// `ln` and `log10`, whose floor is the textbook `u > 0` — but only because
    /// the evaluator refuses a non-finite value. `ln(0)` is negative infinity,
    /// so while that was allowed to travel, zero satisfied *any* upper bound and
    /// this floor had to be `>= 0` to match. Now `ln(0)` is a
    /// `ProblemKind::NonFiniteValue` and the point is discarded either way, so
    /// the mathematics and the evaluator finally agree.
    ///
    /// `runtime_errors::a_logarithm_of_zero_is_refused` is what holds that up.
    /// If it ever goes green-by-relaxation, this floor has to go back to `Gte`.
    fn logarithmic(inverse: impl Fn(f64) -> f64 + 'static) -> Monotone {
        Monotone {
            inverse: Box::new(inverse),
            increasing: true,
            range: ALL,
            domain_floor: Some((0.0, CompareOp::Gt)),
        }
    }

    match &expr.kind {
        Kind::Unary { op, arg } => {
            let inversion = match op {
                UnaryOp::Ln => logarithmic(f64::exp),
                UnaryOp::Log10 => logarithmic(|c| 10.0_f64.powf(c)),
                // `sqrt` keeps the inclusive floor, and the asymmetry with
                // `ln` above is principled rather than incidental: `sqrt(0)` is
                // `0.0`, a perfectly good answer, where `ln(0)` is not an
                // answer at all.
                UnaryOp::Sqrt => Monotone {
                    inverse: Box::new(|c| c * c),
                    increasing: true,
                    range: (0.0, f64::INFINITY),
                    domain_floor: Some((0.0, CompareOp::Gte)),
                },
                UnaryOp::Cbrt => total(|c| c * c * c, ALL),
                UnaryOp::Cube => total(f64::cbrt, ALL),
                UnaryOp::Sinh => total(f64::asinh, ALL),
                UnaryOp::Tanh => total(f64::atanh, (-1.0, 1.0)),
                UnaryOp::Atan => total(f64::tan, (-FRAC_PI_2, FRAC_PI_2)),
                _ => return None,
            };
            Some((inversion, arg.as_ref()))
        }

        // `a ^ u` and `log(a, u)`, the two halves of the same relationship:
        // each is the other's inverse, so inverting one means applying the
        // other. Both are monotone in `u` for a positive constant base, and
        // both reverse below a base of one — the only rows in the table that
        // reverse a comparison.
        Kind::Binary {
            op: op @ (BinaryOp::Pow | BinaryOp::LogB),
            lhs,
            rhs,
        } => {
            let Kind::Literal(base) = lhs.kind else {
                return None;
            };
            // No NaN check needed: `fold_constants` runs first and refuses a
            // non-finite literal, so every literal reaching this pass is finite.
            // `1 ^ u` is excluded because it is constant rather than monotone,
            // and exact equality is the right test — the base is a literal the
            // author wrote, not the result of a computation.
            if base <= 0.0 || base == 1.0 {
                return None;
            }
            // `a^u` maps the reals onto the positives, and `log(a, u)` maps
            // the positives onto the reals, so the domain and range swap along
            // with the inverse.
            let inversion = if *op == BinaryOp::Pow {
                Monotone {
                    inverse: Box::new(move |c| c.ln() / base.ln()),
                    increasing: base > 1.0,
                    range: (0.0, f64::INFINITY),
                    domain_floor: None,
                }
            } else {
                Monotone {
                    inverse: Box::new(move |c| base.powf(c)),
                    increasing: base > 1.0,
                    range: ALL,
                    domain_floor: Some((0.0, CompareOp::Gt)),
                }
            };
            Some((inversion, rhs.as_ref()))
        }

        _ => None,
    }
}

/// `a op b` with the sides exchanged: what `b ? a` has to be to mean the same.
const fn mirrored(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::Lt => CompareOp::Gt,
        CompareOp::Lte => CompareOp::Gte,
        CompareOp::Gt => CompareOp::Lt,
        CompareOp::Gte => CompareOp::Lte,
    }
}

fn invert_comparison(op: CompareOp, lhs: Expr, rhs: Expr, span: Span) -> Expr {
    // Orient to `f(u) op c`, whichever side the function turned up on.
    let (op, function, constant) = match (&lhs.kind, &rhs.kind) {
        (_, Kind::Literal(constant)) if monotone(&lhs).is_some() => (op, &lhs, *constant),
        (Kind::Literal(constant), _) if monotone(&rhs).is_some() => (mirrored(op), &rhs, *constant),
        _ => {
            return Expr::new(
                Kind::Compare {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }
    };

    let unchanged = || {
        Expr::new(
            Kind::Compare {
                op,
                lhs: Box::new(lhs.clone()),
                rhs: Box::new(rhs.clone()),
            },
            span,
        )
    };

    let Some((inversion, argument)) = monotone(function) else {
        unreachable!("the orientation above already established one");
    };

    // Outside what `f` can produce there is nothing to invert — the comparison
    // is constantly true or constantly false, and saying which would need a
    // representation for a constant boolean that does not exist yet.
    if constant < inversion.range.0 || constant > inversion.range.1 {
        return unchanged();
    }

    let op = if inversion.increasing {
        op
    } else {
        mirrored(op)
    };
    let bound = (inversion.inverse)(constant);

    // Catches the open ends of a range — `atanh(1.0)` is infinite — and an
    // inverse that simply overflows, as `exp` does past about 709.
    if !bound.is_finite() {
        return unchanged();
    }

    // One ulp outward, so the region asserted is never *narrower* than the one
    // the constraint describes. `fl(e^2)` is not `e^2`, and on the narrow side
    // an `unsat` would stop implying that the original is unsatisfiable. The
    // sliver this admits is filtered by the pool's own `evaluate`, which is the
    // existing contract for everything a solver proposes.
    let bounds_above = matches!(op, CompareOp::Lt | CompareOp::Lte);
    let bound = if bounds_above {
        bound.next_up()
    } else {
        bound.next_down()
    };

    let inverted = Expr::new(
        Kind::Compare {
            op,
            lhs: Box::new(argument.clone()),
            rhs: Box::new(Expr::new(Kind::Literal(bound), span)),
        },
        span,
    );

    // A lower bound on `u` normally carries the domain with it, because
    // `f`-inverse lands in the domain by definition — `e^c` is positive for
    // every `c`. Bounding `u` from above does not, and neither does a widened
    // bound that has slipped below the floor, which `sqrt(x) > 0` manages.
    let Some((floor, floor_op)) = inversion.domain_floor else {
        return inverted;
    };
    if !bounds_above && bound >= floor {
        return inverted;
    }

    // Both at once. This used to build `max(residual_a, residual_b) <= 0` by
    // hand, which meant this pass — a *front end* pass — knew the evaluator's
    // `<= 0` convention. `Kind::And` says what is meant and lets each backend
    // decide what that costs.
    let comparison = |op, against| {
        Expr::new(
            Kind::Compare {
                op,
                lhs: Box::new(argument.clone()),
                rhs: Box::new(Expr::new(Kind::Literal(against), span)),
            },
            span,
        )
    };
    Expr::new(
        Kind::And {
            terms: vec![comparison(op, bound), comparison(floor_op, floor)],
        },
        span,
    )
}

/// The most terms an aggregate will unroll into.
///
/// A policy knob, not a correctness one: unrolling a million terms is a memory
/// blowup, and the JVM implementation had no cap at all. Generous against the
/// ~200 of the performance fixture and the 9 of rosenbrock; past it the
/// aggregate is refused, because an aggregate that large is its own disaster
/// for a solver anyway and a run-time loop would be a performance landmine in
/// the batched evaluator.
const UNROLL_LIMIT: i64 = 1024;

/// Replaces every aggregate with the terms it expands to.
///
/// `sum` and `prod` are big-sigma and big-pi over a fixed index set, and that
/// is all they are: after this pass no [`Kind::Aggregate`] exists, and no
/// backend has to know one ever did. This is what keeps them out of the
/// batched evaluator, where a loop whose trip count differs per sample cannot
/// run a tile at a time, and out of interval narrowing, which walks a fold
/// term by term.
///
/// # Errors
/// A bound that is not a constant expression, one that is a constant but not a
/// usable index — NaN, infinite, or fractional — and an aggregate wider than
/// [`UNROLL_LIMIT`] are all compile-time failures.
pub(crate) fn unroll_aggregates(program: Program) -> Result<Program, Vec<Fault>> {
    let Program { body, frame_size } = program;
    Ok(Program {
        body: unroll_block(body)?,
        frame_size,
    })
}

fn unroll_block(block: Block) -> Result<Block, Vec<Fault>> {
    let Block {
        assignments,
        result,
    } = block;

    let assignments = assignments
        .into_iter()
        .map(|Assignment { slot, value, span }| {
            let value_expr = unroll_expr(value)?;
            Ok(Assignment {
                slot,
                value: value_expr,
                span,
            })
        })
        .collect::<Result<_, Vec<Fault>>>()?;

    let result_expr = unroll_expr(result)?;

    Ok(Block {
        assignments,
        result: result_expr,
    })
}

fn unroll_expr(expr: Expr) -> Result<Expr, Vec<Fault>> {
    let Expr { kind, span } = expr;

    let kind = match kind {
        Kind::Aggregate {
            kind,
            lower,
            upper,
            param,
            body,
        } => {
            // Descend first: an inner aggregate only becomes unrollable once the
            // enclosing parameter has been substituted away, and substitution
            // happens below. Recursing here handles nesting without a fixpoint.
            let lower = Box::new(unroll_expr(*lower)?);
            let upper = Box::new(unroll_expr(*upper)?);
            let body = Box::new(unroll_block(*body)?);

            Kind::Fold {
                kind,
                terms: static_range(&lower, &upper, span)?
                    .map(|index| {
                        let term = Expr::new(Kind::Block(body.clone()), span);
                        substitute(term, param, index)
                    })
                    .collect::<Result<_, _>>()?,
            }
        }

        Kind::Unary { op, arg } => Kind::Unary {
            op,
            arg: Box::new(unroll_expr(*arg)?),
        },
        Kind::Binary { op, lhs, rhs } => Kind::Binary {
            op,
            lhs: Box::new(unroll_expr(*lhs)?),
            rhs: Box::new(unroll_expr(*rhs)?),
        },
        Kind::DynamicIndex(index) => Kind::DynamicIndex(Box::new(unroll_expr(*index)?)),
        Kind::Block(block) => Kind::Block(Box::new(unroll_block(*block)?)),
        Kind::Fold { kind, terms } => Kind::Fold {
            kind,
            terms: terms
                .into_iter()
                .map(unroll_expr)
                .collect::<Result<_, Vec<Fault>>>()?,
        },

        leaf @ (Kind::Literal(_) | Kind::Global(_) | Kind::Local(_)) => leaf,

        // A boolean can only be the root of a constraint, so an aggregate never
        // sits above one. Descending anyway costs three arms and means this pass
        // does not have to know that.
        Kind::Compare { op, lhs, rhs } => Kind::Compare {
            op,
            lhs: Box::new(unroll_expr(*lhs)?),
            rhs: Box::new(unroll_expr(*rhs)?),
        },
        Kind::NearEq {
            lhs,
            rhs,
            tolerance,
        } => Kind::NearEq {
            lhs: Box::new(unroll_expr(*lhs)?),
            rhs: Box::new(unroll_expr(*rhs)?),
            tolerance,
        },
        Kind::And { terms } => Kind::And {
            terms: terms
                .into_iter()
                .map(unroll_expr)
                .collect::<Result<_, Vec<Fault>>>()?,
        },
    };

    Ok(Expr { kind, span })
}

/// The range an aggregate covers, when both bounds are statically known.
///
/// `Ok(None)` means at least one bound depends on a variable, so the aggregate
/// has to stay a loop. `Err` means a bound is known *and* unusable.
///
/// "Statically known" is a pattern match rather than an analysis, because
/// [`fold_constants`] has already run: `sum(1, 2+3, …)` arrives here with a
/// literal `5` in it. This function used to evaluate the bound expressions
/// itself, through an `ast::const_eval` that existed only to serve it.
fn static_range(
    lower: &Expr,
    upper: &Expr,
    span: Span,
) -> Result<std::ops::RangeInclusive<i64>, Vec<Fault>> {
    let constant = |bound: &Expr, which: BoundKind| match bound.kind {
        Kind::Literal(value) => Ok(value),
        _ => Err(vec![Fault {
            kind: ProblemKind::AggregateBoundNotConstant { bound: which },
            span: bound.span,
        }]),
    };
    let lower_value = constant(lower, BoundKind::Lower)?;
    let upper_value = constant(upper, BoundKind::Upper)?;

    let bound = |value: f64, which: BoundKind, at: &Expr| {
        to_index(value).ok_or_else(|| {
            vec![Fault {
                kind: ProblemKind::IllegalAggregateBound {
                    bound: which,
                    value,
                },
                span: at.span,
            }]
        })
    };

    let first = bound(lower_value, BoundKind::Lower, lower)?;
    let last = bound(upper_value, BoundKind::Upper, upper)?;

    let terms = last.saturating_sub(first).saturating_add(1);
    if terms > UNROLL_LIMIT {
        return Err(vec![Fault {
            kind: ProblemKind::AggregateTooWide {
                terms,
                limit: UNROLL_LIMIT,
            },
            span,
        }]);
    }

    Ok(first..=last)
}

/// Replaces every reference to `param` with `index`.
///
/// By slot rather than by name, so a body that shadows the parameter —
/// `sum(1, 3, i -> var i = 5; i)` — binds a different slot and is left alone.
#[allow(clippy::cast_precision_loss)]
fn substitute(expr: Expr, param: LocalSlot, index: i64) -> Result<Expr, Vec<Fault>> {
    let Expr { kind, span } = expr;

    let kind = match kind {
        Kind::Local(slot) if slot == param => Kind::Literal(index as f64),

        Kind::Unary { op, arg } => Kind::Unary {
            op,
            arg: Box::new(substitute(*arg, param, index)?),
        },
        Kind::Binary { op, lhs, rhs } => Kind::Binary {
            op,
            lhs: Box::new(substitute(*lhs, param, index)?),
            rhs: Box::new(substitute(*rhs, param, index)?),
        },
        Kind::DynamicIndex(subscript) => {
            // Folded here, and not left to the pass that folds: `fold_constants`
            // runs *before* this one, so it has already been and gone by the
            // time substitution puts a literal where the parameter was.
            // `var[i-1]` would otherwise unroll to `var[2 - 1]` and stay an
            // expression — indistinguishable downstream from `var[n]`, which
            // nothing can resolve, so `interval` narrows nothing through it
            // and `classify` refuses the whole constraint.
            //
            // The fold judges the literal it makes — `sum(0, 2, i -> var[i])`
            // puts a `var[0]` here — and that verdict is this pass's too,
            // which is why the whole subscript node is folded, not its index.
            let subscript = substitute(*subscript, param, index)?;
            fold_expr(Expr {
                kind: Kind::DynamicIndex(Box::new(subscript)),
                span,
            })?
            .kind
        }
        Kind::Aggregate {
            kind,
            lower,
            upper,
            param: inner,
            body,
        } => Kind::Aggregate {
            kind,
            lower: Box::new(substitute(*lower, param, index)?),
            upper: Box::new(substitute(*upper, param, index)?),
            param: inner,
            body: Box::new(substitute_block(*body, param, index)?),
        },
        Kind::Block(block) => Kind::Block(Box::new(substitute_block(*block, param, index)?)),
        Kind::Fold { kind, terms } => Kind::Fold {
            kind,
            terms: terms
                .into_iter()
                .map(|term| substitute(term, param, index))
                .collect::<Result<_, _>>()?,
        },

        leaf @ (Kind::Literal(_) | Kind::Global(_) | Kind::Local(_)) => leaf,

        // Unreachable: a lambda body is a `scalarBlock`, so no boolean sits
        // under a parameter. Substituted anyway rather than left alone, so the
        // walk is total and a grammar change cannot leave a parameter behind.
        Kind::Compare { op, lhs, rhs } => Kind::Compare {
            op,
            lhs: Box::new(substitute(*lhs, param, index)?),
            rhs: Box::new(substitute(*rhs, param, index)?),
        },
        Kind::NearEq {
            lhs,
            rhs,
            tolerance,
        } => Kind::NearEq {
            lhs: Box::new(substitute(*lhs, param, index)?),
            rhs: Box::new(substitute(*rhs, param, index)?),
            tolerance,
        },
        Kind::And { terms } => Kind::And {
            terms: terms
                .into_iter()
                .map(|term| substitute(term, param, index))
                .collect::<Result<_, _>>()?,
        },
    };

    Ok(Expr { kind, span })
}

fn substitute_block(block: Block, param: LocalSlot, index: i64) -> Result<Block, Vec<Fault>> {
    let Block {
        assignments,
        result,
    } = block;
    Ok(Block {
        assignments: assignments
            .into_iter()
            .map(|Assignment { slot, value, span }| {
                Ok(Assignment {
                    slot,
                    value: substitute(value, param, index)?,
                    span,
                })
            })
            .collect::<Result<_, Vec<Fault>>>()?,
        result: substitute(result, param, index)?,
    })
}

// --------------------------------------------------------------------- powers

/// Rewrites a term multiplied by itself into the power it is: `f * f` to
/// `f^2`, `f * f * f` to `f^3`, `prod(1, n, i -> f)` to `f^n`.
///
/// So that a power has one form whatever the author wrote. It matters
/// because the consumers treat the two spellings differently and would
/// otherwise let the difference show: narrowing inverts `Mul` by dividing by
/// the other factor's whole range, which for `h * h` concludes nothing, and
/// inverts a power through its root, which for `h^2 < 9` concludes `h <= 3`;
/// and the evaluator lowers a repeated factor as many times as it is written
/// where it lowers a power's base once.
///
/// Any association is collected: `f * (f * f)` and `f^2 * f^3` are powers as
/// much as `(f * f) * f` is. The evaluator then unrolls `f^n` as `(t·t)·t…`,
/// left to right, which can differ from the spelling's own order in the last
/// bit — the one pass here that is not bit-exact, on purpose: one power, one
/// tape, one bound, and nobody choosing a spelling for its rounding.
/// Exponents stay within [`crate::ast::POWER_LIMIT`].
pub(crate) fn collect_powers(program: Program) -> Program {
    let Program { body, frame_size } = program;
    Program {
        body: collect_block(body),
        frame_size,
    }
}

fn collect_block(block: Block) -> Block {
    let Block {
        assignments,
        result,
    } = block;
    Block {
        assignments: assignments
            .into_iter()
            .map(|Assignment { slot, value, span }| Assignment {
                slot,
                value: collect_expr(value),
                span,
            })
            .collect(),
        result: collect_expr(result),
    }
}

fn collect_expr(expr: Expr) -> Expr {
    let Expr { kind, span } = expr;
    let descend = |expr: Box<Expr>| Box::new(collect_expr(*expr));

    let kind = match kind {
        // Children first, so `(f * f) * f` sees `f^2 * f`.
        Kind::Binary {
            op: BinaryOp::Mul,
            lhs,
            rhs,
        } => {
            let (lhs, rhs) = (descend(lhs), descend(rhs));
            match collected(*lhs, *rhs, span) {
                Ok(power) => return power,
                Err((lhs, rhs)) => Kind::Binary {
                    op: BinaryOp::Mul,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            }
        }
        Kind::Fold {
            kind: AggregateKind::Prod,
            terms,
        } => {
            let terms: Vec<Expr> = terms.into_iter().map(collect_expr).collect();
            let repeated = terms.len() >= 2
                && i64::try_from(terms.len()).is_ok_and(|n| n <= crate::ast::POWER_LIMIT)
                && terms[1..].iter().all(|term| same_shape(term, &terms[0]));
            if repeated {
                let n = i64::try_from(terms.len()).expect("checked above");
                let mut terms = terms;
                return power(terms.swap_remove(0), n, span);
            }
            Kind::Fold {
                kind: AggregateKind::Prod,
                terms,
            }
        }

        Kind::Unary { op, arg } => Kind::Unary {
            op,
            arg: descend(arg),
        },
        Kind::Binary { op, lhs, rhs } => Kind::Binary {
            op,
            lhs: descend(lhs),
            rhs: descend(rhs),
        },
        Kind::DynamicIndex(index) => Kind::DynamicIndex(descend(index)),
        Kind::Block(block) => Kind::Block(Box::new(collect_block(*block))),
        Kind::Fold { kind, terms } => Kind::Fold {
            kind,
            terms: terms.into_iter().map(collect_expr).collect(),
        },
        Kind::Compare { op, lhs, rhs } => Kind::Compare {
            op,
            lhs: descend(lhs),
            rhs: descend(rhs),
        },
        Kind::NearEq {
            lhs,
            rhs,
            tolerance,
        } => Kind::NearEq {
            lhs: descend(lhs),
            rhs: descend(rhs),
            tolerance,
        },
        Kind::And { terms } => Kind::And {
            terms: terms.into_iter().map(collect_expr).collect(),
        },
        Kind::Aggregate {
            kind,
            lower,
            upper,
            param,
            body,
        } => Kind::Aggregate {
            kind,
            lower: descend(lower),
            upper: descend(upper),
            param,
            body: Box::new(collect_block(*body)),
        },

        leaf @ (Kind::Literal(_) | Kind::Global(_) | Kind::Local(_)) => leaf,
    };

    Expr { kind, span }
}

/// `lhs * rhs` as a power, where it is one: `f * f` is `f^2`, `f^n * f` and
/// `f * f^n` are `f^(n+1)`, `f^n * f^m` is `f^(n+m)`. Otherwise the operands,
/// handed back. A factor that is not a power counts as the first.
fn collected(lhs: Expr, rhs: Expr, span: Span) -> Result<Expr, (Expr, Expr)> {
    let (left_base, n) = base_and_exponent(&lhs);
    let (right_base, m) = base_and_exponent(&rhs);
    if !same_shape(left_base, right_base) || n + m > crate::ast::POWER_LIMIT {
        return Err((lhs, rhs));
    }
    let base = match lhs.kind {
        Kind::Binary {
            op: BinaryOp::Pow,
            lhs: base,
            ..
        } if n > 1 => *base,
        _ => lhs,
    };
    Ok(power(base, n + m, span))
}

/// `(f, n)` for `f^n` with a whole positive `n` within the cap; `(expr, 1)`
/// for anything else.
fn base_and_exponent(expr: &Expr) -> (&Expr, i64) {
    if let Kind::Binary {
        op: BinaryOp::Pow,
        lhs: base,
        rhs: exponent,
    } = &expr.kind
        && let Some(n) = exponent.whole_exponent()
        && (2..=crate::ast::POWER_LIMIT).contains(&n)
    {
        return (base, n);
    }
    (expr, 1)
}

fn power(base: Expr, n: i64, span: Span) -> Expr {
    #[expect(
        clippy::cast_precision_loss,
        reason = "an exponent is within POWER_LIMIT"
    )]
    let exponent = n as f64;
    // An unrolled aggregate's term is its body, a block binding nothing; the
    // power's base is what the block computes.
    let base = match base.kind {
        Kind::Block(block) if block.assignments.is_empty() => block.result,
        _ => base,
    };
    Expr::new(
        Kind::Binary {
            op: BinaryOp::Pow,
            lhs: Box::new(base),
            rhs: Box::new(Expr::new(Kind::Literal(exponent), span)),
        },
        span,
    )
}

/// Structural equality, spans aside: two subtrees that compute the same thing
/// from the same variables. `Expr`'s own equality compares spans, which two
/// spellings of one factor never share.
fn same_shape(a: &Expr, b: &Expr) -> bool {
    match (&a.kind, &b.kind) {
        (Kind::Literal(x), Kind::Literal(y)) => x.to_bits() == y.to_bits(),
        (Kind::Global(x), Kind::Global(y)) => x == y,
        (Kind::Local(x), Kind::Local(y)) => x == y,
        (Kind::Unary { op: p, arg: x }, Kind::Unary { op: q, arg: y }) => {
            p == q && same_shape(x, y)
        }
        (
            Kind::Binary {
                op: p,
                lhs: a1,
                rhs: a2,
            },
            Kind::Binary {
                op: q,
                lhs: b1,
                rhs: b2,
            },
        ) => p == q && same_shape(a1, b1) && same_shape(a2, b2),
        (Kind::DynamicIndex(x), Kind::DynamicIndex(y)) => same_shape(x, y),
        (Kind::Fold { kind: p, terms: x }, Kind::Fold { kind: q, terms: y }) => {
            p == q && x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_shape(x, y))
        }
        // A block binding nothing is its result: what an unrolled aggregate's
        // terms are. One that binds slots, and a comparison, are not factors
        // worth collecting, and saying "different" is always safe.
        (Kind::Block(x), Kind::Block(y))
            if x.assignments.is_empty() && y.assignments.is_empty() =>
        {
            same_shape(&x.result, &y.result)
        }
        (Kind::Block(x), _) if x.assignments.is_empty() => same_shape(&x.result, b),
        (_, Kind::Block(y)) if y.assignments.is_empty() => same_shape(a, &y.result),
        _ => false,
    }
}

/// Rewrites `x ^ n`, for a whole literal `n`, into the multiplications it is:
/// what the evaluator lowers, and no other consumer.
///
/// `powf` is a per-lane libm call with no vector form and the shader's `pow`
/// is NaN for a negative base; a multiply is one instruction on both and
/// sign-safe. Interval narrowing wants the opposite — `x·x·x` over intervals
/// is wider than `x³`, and a chain of multiplications inverts by dividing
/// where a power inverts through its root — so this is not part of
/// [`canonicalize`]: `eval::bind` applies it to its own copy of the tree, and
/// the narrower lowers the tree as parsed. The lowering itself decides nothing
/// about powers either way.
///
/// A compound base is bound to a fresh `let` and the multiplications read the
/// local, so the base is evaluated once — the reason a tree-level unroll was
/// once removed from `parse`, where it could only duplicate the subtree. A
/// leaf base is repeated as it stands. `x^0` is the constant one for every
/// base, including one that would fault, which is what `powf` answers; a
/// negative exponent is one over the positive power; a real or oversized
/// exponent stays `Pow` and goes through `powf`.
pub(crate) fn unroll_powers(program: Program) -> Program {
    let Program {
        body,
        mut frame_size,
    } = program;
    let body = powers_block(body, &mut frame_size);
    Program { body, frame_size }
}

fn powers_block(block: Block, frame_size: &mut u32) -> Block {
    let Block {
        assignments,
        result,
    } = block;
    Block {
        assignments: assignments
            .into_iter()
            .map(|Assignment { slot, value, span }| Assignment {
                slot,
                value: powers_expr(value, frame_size),
                span,
            })
            .collect(),
        result: powers_expr(result, frame_size),
    }
}

fn powers_expr(expr: Expr, frame_size: &mut u32) -> Expr {
    let Expr { kind, span } = expr;
    let descend = |expr: Box<Expr>, frame_size: &mut u32| Box::new(powers_expr(*expr, frame_size));

    let kind = match kind {
        Kind::Binary {
            op: BinaryOp::Pow,
            lhs,
            rhs,
        } if rhs.whole_exponent().is_some() => {
            let n = rhs.whole_exponent().expect("matched above");
            let base = powers_expr(*lhs, frame_size);
            return unrolled(base, n, span, frame_size);
        }

        Kind::Unary { op, arg } => Kind::Unary {
            op,
            arg: descend(arg, frame_size),
        },
        Kind::Binary { op, lhs, rhs } => Kind::Binary {
            op,
            lhs: descend(lhs, frame_size),
            rhs: descend(rhs, frame_size),
        },
        Kind::DynamicIndex(index) => Kind::DynamicIndex(descend(index, frame_size)),
        Kind::Block(block) => Kind::Block(Box::new(powers_block(*block, frame_size))),
        Kind::Fold { kind, terms } => Kind::Fold {
            kind,
            terms: terms
                .into_iter()
                .map(|term| powers_expr(term, frame_size))
                .collect(),
        },
        Kind::Compare { op, lhs, rhs } => Kind::Compare {
            op,
            lhs: descend(lhs, frame_size),
            rhs: descend(rhs, frame_size),
        },
        Kind::NearEq {
            lhs,
            rhs,
            tolerance,
        } => Kind::NearEq {
            lhs: descend(lhs, frame_size),
            rhs: descend(rhs, frame_size),
            tolerance,
        },
        Kind::And { terms } => Kind::And {
            terms: terms
                .into_iter()
                .map(|term| powers_expr(term, frame_size))
                .collect(),
        },
        Kind::Aggregate {
            kind,
            lower,
            upper,
            param,
            body,
        } => Kind::Aggregate {
            kind,
            lower: descend(lower, frame_size),
            upper: descend(upper, frame_size),
            param,
            body: Box::new(powers_block(*body, frame_size)),
        },

        leaf @ (Kind::Literal(_) | Kind::Global(_) | Kind::Local(_)) => leaf,
    };

    Expr { kind, span }
}

/// `base ^ n` as multiplications: `|n| - 1` of them, left-associated, over
/// reads of the base — bound to a fresh local first unless it is a leaf. The
/// same `(t·t)·t` a product fold from the identity computes, less the
/// multiplication by one.
fn unrolled(base: Expr, n: i64, span: Span, frame_size: &mut u32) -> Expr {
    if n == 0 {
        return Expr::new(Kind::Literal(1.0), span);
    }
    let count = usize::try_from(n.unsigned_abs()).expect("bounded by POWER_LIMIT");
    let product = |factor: Expr| {
        (1..count).fold(factor.clone(), |acc, _| {
            Expr::new(
                Kind::Binary {
                    op: BinaryOp::Mul,
                    lhs: Box::new(acc),
                    rhs: Box::new(factor.clone()),
                },
                span,
            )
        })
    };
    let positive = match base.kind {
        Kind::Literal(_) | Kind::Global(_) | Kind::Local(_) => product(base),
        _ => {
            let slot = LocalSlot::from_index(*frame_size);
            *frame_size += 1;
            Expr::new(
                Kind::Block(Box::new(Block {
                    assignments: vec![Assignment {
                        slot,
                        value: base,
                        span,
                    }],
                    result: product(Expr::new(Kind::Local(slot), span)),
                })),
                span,
            )
        }
    };
    if n > 0 {
        positive
    } else {
        Expr::new(
            Kind::Binary {
                op: BinaryOp::Div,
                lhs: Box::new(Expr::new(Kind::Literal(1.0), span)),
                rhs: Box::new(positive),
            },
            span,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval;
    use crate::frontend::syntax;

    /// `Pow(_, n)` at the root of a parsed expression, or `None`.
    fn power_at_root(source: &str) -> Option<(Kind, i64)> {
        let ast = crate::parse(source).expect("should compile");
        match ast.program.body.result.kind {
            Kind::Binary {
                op: BinaryOp::Pow,
                lhs,
                rhs,
            } => Some((lhs.kind, rhs.whole_exponent().expect("a whole exponent"))),
            _ => None,
        }
    }

    /// A term multiplied by itself is a power, however it is spelled, so that
    /// every consumer sees one form: the evaluator unrolls it, the narrower
    /// inverts it through its root, and neither depends on the author's
    /// choice between `f * f` and `f^2`.
    #[test]
    fn a_term_multiplied_by_itself_is_a_power() {
        let (base, n) = power_at_root("(sin(x) + 1) * (sin(x) + 1)").expect("a square");
        assert_eq!(n, 2);
        assert!(
            matches!(
                base,
                Kind::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            ),
            "{base:?}"
        );

        let (base, n) = power_at_root("y * y * y").expect("a cube");
        assert_eq!((base, n), (Kind::Global(GlobalId::from_index(0)), 3));

        let (base, n) = power_at_root("prod(1, 4, i -> x)").expect("a fourth power");
        assert_eq!((base, n), (Kind::Global(GlobalId::from_index(0)), 4));
    }

    /// However the factors are grouped, and whichever of them are already
    /// powers, the product is one power.
    #[test]
    fn any_association_of_a_repeated_factor_is_a_power() {
        for (source, n) in [
            ("y * (y * y)", 3),
            ("(y * y) * (y * y)", 4),
            ("y^2 * y^3", 5),
            ("y * y^4", 5),
            ("(y * y) * (y * (y * y))", 5),
        ] {
            let (base, exponent) = power_at_root(source).unwrap_or_else(|| panic!("{source}"));
            assert_eq!(
                (base, exponent),
                (Kind::Global(GlobalId::from_index(0)), n),
                "{source}"
            );
        }
        // Past the cap the product stays a product.
        assert!(power_at_root("y^40 * y^40").is_none());
    }

    /// Collecting powers, and the evaluator's unrolling after it, keep every
    /// value: to the bit where the multiplications land in the order they
    /// were written, and within a few ulps where collecting reassociated
    /// them. What lets the canonical form grow.
    #[test]
    fn collecting_powers_keeps_the_value() {
        use rand::rngs::Xoshiro256PlusPlus;
        use rand::{RngExt, SeedableRng};

        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x5A_5A_C0_DE);
        for (source, ulps) in [
            ("x * x", 0),
            ("x * x * x", 0),
            ("(a / b) * (a / b) * (a / b)", 0),
            ("x^3", 0),
            ("x^-2", 0),
            ("x^0", 0),
            ("(a + b)^4", 0),
            ("(a + b) * (a + b)", 0),
            ("sin(x) * sin(x)", 0),
            ("prod(1, 3, i -> x)", 0),
            ("a * b * a", 0),
            // Reassociated: the spelling's order is not the unroll's.
            ("x * (x * x)", 4),
            ("(x * x) * (x * (x * x))", 4),
            ("(a + b)^2 * (a + b)^3", 4),
        ] {
            let raw = syntax::raw_ast(source).expect("should translate");
            let program = fold_constants(raw.program).expect("folds");
            let program = unroll_aggregates(invert_monotone(program)).expect("unrolls");
            let raw = Ast {
                source: source.to_owned(),
                contains_dynamic_lookup: holds_subscript(&program.body),
                program,
                symbols: raw.symbols,
                is_constraint: raw.is_constraint,
            };
            let rewritten = crate::parse(source).expect("should compile");
            for _ in 0..64 {
                let inputs: Vec<(&str, f64)> = ["x", "a", "b"]
                    .into_iter()
                    .map(|name| (name, rng.random_range(-3.0..3.0)))
                    .collect();
                let before = eval::eval_parsed(&raw, &inputs);
                let after = eval::eval_parsed(&rewritten, &inputs);
                match (before, after) {
                    (Ok(before), Ok(after)) => {
                        let apart = before.to_bits().abs_diff(after.to_bits());
                        assert!(
                            apart <= ulps,
                            "{source} at {inputs:?}: {before} became {after}, {apart} ulps"
                        );
                    }
                    (Err(_), Err(_)) => {}
                    (before, after) => {
                        panic!("{source} at {inputs:?}: {before:?} became {after:?}")
                    }
                }
            }
        }
    }

    /// The evaluator's unrolling binds a compound base once and multiplies
    /// the local; a leaf base is multiplied as it stands, and the frame grows
    /// only for the former.
    #[test]
    fn unrolling_binds_a_compound_base_once() {
        let compound = crate::parse("(a + b)^3").expect("should compile");
        let before = compound.program.frame_size;
        let unrolled = unroll_powers(compound.program);
        assert_eq!(unrolled.frame_size, before + 1);
        let Kind::Block(block) = &unrolled.body.result.kind else {
            panic!("a compound base is bound: {:?}", unrolled.body.result);
        };
        assert_eq!(block.assignments.len(), 1);
        assert!(
            matches!(
                block.assignments[0].value.kind,
                Kind::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            ),
            "{:?}",
            block.assignments[0].value
        );
        // `(t * t) * t`, reading the local.
        let Kind::Binary {
            op: BinaryOp::Mul,
            rhs,
            ..
        } = &block.result.kind
        else {
            panic!("{:?}", block.result);
        };
        assert_eq!(rhs.kind, Kind::Local(block.assignments[0].slot));

        let leaf = crate::parse("x^3").expect("should compile");
        let before = leaf.program.frame_size;
        let unrolled = unroll_powers(leaf.program);
        assert_eq!(unrolled.frame_size, before);
        assert!(
            matches!(
                unrolled.body.result.kind,
                Kind::Binary {
                    op: BinaryOp::Mul,
                    ..
                }
            ),
            "{:?}",
            unrolled.body.result
        );
    }

    /// Substitution puts a literal where the loop parameter was, and the
    /// subscript has to end up a *literal* rather than an expression that
    /// happens to be constant.
    ///
    /// `fold_constants` runs before unrolling, so nothing else will do it, and
    /// the difference is not cosmetic: `resolve_subscripts` resolves a literal
    /// subscript against the schema and leaves anything else computed, so an
    /// unfolded `var[2 - 1]` costs every reader the whole constraint. Every
    /// aggregate subscript in the corpus is arithmetic — Rosenbrock's
    /// `var[i-1]`, `var[2*i-1]` — so this is most of them.
    #[test]
    fn an_unrolled_subscript_is_a_literal() {
        let expression = crate::parse("sum(2, 2, i -> var[i-1])").expect("should compile");

        // One term, since the aggregate runs 2..=2.
        let Kind::Fold { ref terms, .. } = expression.program.body.result.kind else {
            panic!(
                "expected a fold, got {:?}",
                expression.program.body.result.kind
            );
        };
        // The lambda body is a block, even when it holds one expression.
        let Kind::Block(ref body) = terms[0].kind else {
            panic!("expected the lambda's block, got {:?}", terms[0].kind);
        };
        let Kind::DynamicIndex(ref subscript) = body.result.kind else {
            panic!("expected a subscript, got {:?}", body.result.kind);
        };
        match subscript.kind {
            Kind::Literal(value) => assert_eq!(value, 1.0),
            ref other => panic!("`var[i-1]` at i = 2 should be `var[1]`, got {other:?}"),
        }
    }

    // ------------------------------------------------------- resolve_subscripts

    fn resolved(source: &str, names: &[&str]) -> Result<Ast, Fault> {
        let schema = Schema::for_names(names);
        resolve_subscripts(crate::parse(source).expect("should compile"), &schema)
    }

    /// The whole point: `var[2]` stops being an indirection and becomes the
    /// variable it always meant.
    #[test]
    fn a_literal_subscript_becomes_a_variable() {
        let ast = resolved("var[2] + 1", &["x1", "x2"]).expect("x2 exists");

        let Kind::Binary { ref lhs, .. } = ast.program.body.result.kind else {
            panic!(
                "expected an addition, got {:?}",
                ast.program.body.result.kind
            );
        };
        let Kind::Global(id) = lhs.kind else {
            panic!("expected a variable, got {:?}", lhs.kind);
        };
        assert_eq!(ast.symbols[id.index()], "x2");
        assert!(
            !ast.contains_dynamic_lookup,
            "nothing is left that could not be resolved"
        );
    }

    /// Resolution has to *extend* the symbol list, which is the reason this
    /// rewrite takes an `Ast` where every other one takes a `Program`.
    ///
    /// `var[2] + 1` names nothing statically, so before this there is no symbol
    /// for `Kind::Global` to point at. Afterwards
    /// `statically_referenced_symbols` reports what the expression *reads*
    /// rather than what it spells, which is the more useful answer and the one
    /// a caller pruning unreferenced columns needs.
    #[test]
    fn resolving_extends_the_symbol_list() {
        let before = crate::parse("var[2] + 1").expect("should compile");
        assert!(before.symbols.is_empty(), "{:?}", before.symbols);

        let after = resolved("var[2] + 1", &["x1", "x2"]).expect("x2 exists");
        assert_eq!(after.symbols, vec!["x2".to_owned()]);
    }

    /// Evaluation must not move. The rewrite is meaning-preserving or it is a
    /// bug, and `var[2]` and `x2` are the same load by different routes.
    #[test]
    fn resolving_does_not_change_what_it_evaluates_to() {
        let bindings = [("x1", 3.0), ("x2", 7.0)];
        let before = crate::parse("var[2] + var[1]").expect("should compile");
        let after = resolved("var[2] + var[1]", &["x1", "x2"]).expect("both exist");

        assert_eq!(
            eval::eval_parsed(&before, &bindings).expect("evaluates"),
            eval::eval_parsed(&after, &bindings).expect("evaluates")
        );
    }

    /// Settled the moment a schema exists, so it is answered then rather than
    /// once per evaluation — and at the subscript, so a caret can land on it.
    #[test]
    fn a_subscript_past_the_schema_is_reported() {
        assert_eq!(
            resolved("var[3] + 1", &["x1", "x2"]),
            Err(Fault {
                kind: ProblemKind::DynamicIndexOutOfBounds {
                    requested_1index: 3,
                    available: 2,
                },
                span: Span::new(4, 5),
            })
        );
        // Zero is out of range at the other end, and refused before this
        // pass runs — `compile_errors::a_subscript_below_one_is_caught_at_compile_time`.
    }

    /// A computed subscript survives untouched, and keeps the flag true.
    ///
    /// Which variable `var[n]` reads depends on the point, so there is nothing
    /// static to resolve. The flag then means what it says - "a subscript
    /// nothing could resolve" - rather than "a subscript".
    #[test]
    fn a_computed_subscript_survives() {
        let ast =
            resolved("var[floor(n)] + 1", &["n", "x2"]).expect("n is a variable, not an index");
        assert!(
            ast.contains_dynamic_lookup,
            "a computed subscript is still dynamic"
        );
    }

    // ---------------------------------------------------------- fold_constants

    /// The whole point of the pass, in one assertion.
    #[test]
    fn a_constant_call_becomes_a_literal() {
        let expression = crate::parse("sin(2.3)").expect("should compile");
        match expression.program.body.result.kind {
            Kind::Literal(value) => assert_eq!(value, 2.3_f64.sin()),
            ref other => panic!("expected a literal, got {other:?}"),
        }
    }

    /// Folding must reach as far as it can and stop exactly where a variable
    /// starts. Half of this tree is knowable and half is not.
    #[test]
    fn folding_stops_at_the_first_variable() {
        let expression = crate::parse("x1 * (2 + 3) + sqrt(16)").expect("should compile");

        // `2 + 3` and `sqrt(16)` both collapse, so what is left is
        // `x1 * 5 + 4` — three leaves and two operators.
        assert_eq!(literals(&expression.program.body.result), vec![5.0, 4.0]);
    }

    fn literals(node: &Expr) -> Vec<f64> {
        node.iter_preorder()
            .filter_map(|node| match node.kind {
                Kind::Literal(value) => Some(value),
                // A query for one kind.
                _ => None,
            })
            .collect()
    }

    /// Folding may not change a value, ever. A folded chain and the same chain
    /// held apart by a variable have to agree to the bit — which they only do if
    /// folding uses the evaluator's own `apply` in the evaluator's own order.
    #[test]
    fn folding_agrees_with_the_evaluator_bit_for_bit() {
        for (folded, held_apart, at) in [
            ("0.1 + 0.2 + 0.3", "x1 + 0.2 + 0.3", 0.1),
            ("1.0 / 3.0 * 3.0", "x1 / 3.0 * 3.0", 1.0),
            ("sin(2.3) * cos(1.1)", "sin(x1) * cos(1.1)", 2.3),
        ] {
            let folded = crate::parse(folded).expect("should compile");
            let held_apart = crate::parse(held_apart).expect("should compile");
            assert_eq!(
                eval::eval_parsed(&folded, &[]).expect("should evaluate"),
                eval::eval_parsed(&held_apart, &[("x1", at)]).expect("should evaluate"),
                "folding changed a value"
            );
        }
    }

    /// A constant that can only ever be NaN or infinite is a mistake, and one
    /// worth reporting where it was written. The last of these is a *literal*
    /// rather than a folded result: babel's `FLOAT` token admits `1.0e400`,
    /// which parses straight to an infinity, so leaves are checked too.
    #[test]
    fn a_non_finite_constant_is_refused() {
        for source in [
            "ln(-1)",
            "sqrt(-1)",
            "1/0",
            "0/0",
            "1.0e400",
            "1.0e400 * 1.0",
        ] {
            let failure = crate::parse(source).expect_err(&format!("{source} should not compile"));
            assert!(
                failure
                    .problems
                    .iter()
                    .any(|p| matches!(p.kind, ProblemKind::NonFiniteConstant { .. })),
                "{source:?} reported {:?}",
                failure.problems
            );
        }
    }

    /// Folding runs before unrolling, which is what lets `static_range` be a
    /// pattern match rather than an evaluator. If this goes red, the
    /// simplification in `static_range` lost something.
    #[test]
    fn an_aggregate_bound_that_needed_folding_still_unrolls() {
        let expression = crate::parse("sum(1, 2+3, i -> i)").expect("should compile");

        match &expression.program.body.result.kind {
            Kind::Fold { terms, .. } => assert_eq!(terms.len(), 5),
            other => panic!("expected a flat fold, got {other:?}"),
        }
    }

    // ------------------------------------------------- powers after unrolling

    /// A loop index as an exponent is a literal once `substitute` has put it
    /// there, which is what lets every backend lower `x1^2` as a whole power.
    #[test]
    fn a_loop_index_as_an_exponent_evaluates() {
        let expression = crate::parse("sum(1, 3, i -> x1^i)").expect("should compile");
        assert_eq!(
            eval::eval_parsed(&expression, &[("x1", 2.0)]).expect("should evaluate"),
            2.0 + 4.0 + 8.0
        );
    }

    // --------------------------------------------------------- invert_monotone

    /// Structure: the function is gone and the variable faces a literal.
    #[test]
    fn a_monotone_function_leaves_the_comparison() {
        // `2 < ln(x1)` is `ln(x1) > 2` is `x1 > e^2`.
        let expression = crate::parse("2 < ln(x1)").expect("should compile");
        assert!(
            !mentions_unary(&expression.program.body.result, UnaryOp::Ln),
            "the logarithm survived: {:?}",
            expression.program.body.result
        );

        // Residual form is `bound - x1 + eps`, so evaluating just past the
        // bound is negative and just short of it is positive.
        let bound = std::f64::consts::E.powi(2);
        assert!(eval::eval_parsed(&expression, &[("x1", bound * 1.001)]).unwrap() < 0.0);
        assert!(eval::eval_parsed(&expression, &[("x1", bound * 0.999)]).unwrap() > 0.0);
    }

    /// `sin` is monotone on `[0, 1]`, but this pass cannot know the box, so it
    /// must leave `sin(x1) > c` alone. `tests/brute_squad.rs` depends on that:
    /// its transcendental family exists to exercise a case no seeder settles,
    /// and an inversion here would quietly turn it back into arithmetic.
    #[test]
    fn a_trig_function_is_not_inverted() {
        let expression = crate::parse("sin(x1) > 0.5").expect("should compile");
        assert!(
            mentions_unary(&expression.program.body.result, UnaryOp::Sin),
            "sin was inverted away: {:?}",
            expression.program.body.result
        );
    }

    fn mentions_unary(node: &Expr, wanted: UnaryOp) -> bool {
        node.iter_preorder()
            .any(|node| matches!(node.kind, Kind::Unary { op, .. } if op == wanted))
    }

    /// The test that matters. A wrong inverse or a flipped direction survives
    /// every structural assertion and dies here: for each row of the table, the
    /// rewritten constraint has to agree with the function it replaced, at every
    /// sampled point.
    #[test]
    fn every_inversion_agrees_with_the_function_it_replaced() {
        // `source` inverts; `equivalent` says the same thing in a form the pass
        // cannot touch, by putting the constant behind a variable.
        let cases = [
            ("ln(x1) > 2", "ln(x1) > 2 * x2", 1.0),
            ("ln(x1) < 2", "ln(x1) < 2 * x2", 1.0),
            ("log(x1) >= 0.5", "log(x1) >= 0.5 * x2", 1.0),
            ("sqrt(x1) > 3", "sqrt(x1) > 3 * x2", 1.0),
            ("sqrt(x1) <= 3", "sqrt(x1) <= 3 * x2", 1.0),
            ("cbrt(x1) < 2", "cbrt(x1) < 2 * x2", 1.0),
            ("cube(x1) >= 8", "cube(x1) >= 8 * x2", 1.0),
            ("sinh(x1) > 1.5", "sinh(x1) > 1.5 * x2", 1.0),
            ("tanh(x1) < 0.5", "tanh(x1) < 0.5 * x2", 1.0),
            ("atan(x1) > 0.7", "atan(x1) > 0.7 * x2", 1.0),
            ("2 ^ x1 < 20", "2 ^ x1 < 20 * x2", 1.0),
            ("log(2, x1) > 3", "log(2, x1) > 3 * x2", 1.0),
            ("log(2, x1) < 3", "log(2, x1) < 3 * x2", 1.0),
            ("log(10, x1) >= 0.5", "log(10, x1) >= 0.5 * x2", 1.0),
            // A base below one: decreasing, so the comparison reverses.
            ("log(0.5, x1) > -3", "log(0.5, x1) > -3 * x2", 1.0),
            // The decreasing row, and the only one that reverses the comparison.
            ("0.5 ^ x1 > 8", "0.5 ^ x1 > 8 * x2", 1.0),
        ];

        for (source, equivalent, x2) in cases {
            let inverted = crate::parse(source).expect("should compile");
            let original = crate::parse(equivalent).expect("should compile");

            // The two may differ, but only in one direction. The inverted
            // bound is nudged a ulp outward on purpose, so it can accept a
            // point the original rejects — and must never reject one the
            // original accepts, because that is the direction in which a
            // solver's `unsat` would stop meaning anything.
            let mut widened = 0;
            for step in -200..=200 {
                let x1 = f64::from(step) * 0.1;
                let (a, b) = (
                    eval::eval_parsed(&inverted, &[("x1", x1), ("x2", x2)]),
                    eval::eval_parsed(&original, &[("x1", x1), ("x2", x2)]),
                );

                // The un-inverted form refuses points outside the domain now
                // that a non-finite value is an error — `ln(0)` among them.
                // Skipping those would gut this test, because they are exactly
                // the points the domain guard exists to exclude. Assert instead
                // that the inverted form rejects them, which is the agreement
                // being claimed.
                let (Ok(a), Ok(b)) = (a.as_ref().copied(), b.as_ref().copied()) else {
                    if let Ok(a) = a {
                        assert!(
                            a > 0.0,
                            "{source:?} accepted x1 = {x1}, where {equivalent:?} \
                                 will not evaluate at all"
                        );
                    }
                    continue;
                };
                assert!(
                    !(a > 0.0 && b <= 0.0),
                    "{source:?} rejected x1 = {x1}, which {equivalent:?} accepts — \
                         the inversion is narrower than the constraint it replaced"
                );
                if a <= 0.0 && b > 0.0 {
                    widened += 1;
                }
            }
            // A grid step landing within a ulp of the boundary picks up the
            // nudge; several would mean the bound itself is in the wrong place.
            assert!(
                widened <= 2,
                "{source:?} accepted {widened} points {equivalent:?} rejects, \
                     which is more than boundary rounding explains"
            );
        }
    }

    /// `ln(x) < 2` bounds `x` from above, which does not carry `x > 0` with it.
    /// The pass has to say both, and "and" is `max`.
    #[test]
    fn an_upper_bound_keeps_the_domain() {
        let expression = crate::parse("ln(x1) < 2").expect("should compile");

        // Inside the domain and under the bound: satisfied.
        assert!(eval::eval_parsed(&expression, &[("x1", 1.0)]).unwrap() <= 0.0);
        // Over the bound.
        assert!(eval::eval_parsed(&expression, &[("x1", 100.0)]).unwrap() > 0.0);
        // Outside the domain. Without the guard this would read as satisfied,
        // and a solver could then report an `unsat` that is not true.
        assert!(
            eval::eval_parsed(&expression, &[("x1", -5.0)]).unwrap() > 0.0,
            "a negative argument passed a logarithm constraint"
        );

        // Zero, which is the case the floor was widened to `>= 0` for while
        // `ln(0)` was allowed to evaluate to negative infinity. The evaluator
        // refuses that now, so the floor is the textbook `> 0` and zero has to
        // be rejected here — `runtime_errors::a_logarithm_of_zero_is_refused`
        // is the other half of this pair.
        assert!(
            eval::eval_parsed(&expression, &[("x1", 0.0)]).unwrap() > 0.0,
            "zero passed a logarithm constraint whose floor is now exclusive"
        );
    }

    /// A constant outside what the function can produce is not invertible.
    /// `atan(x) > 2` is unsatisfiable; `x > tan(2)` is `x > -2.18`, which is
    /// almost always true. Refusing is the only safe answer available.
    #[test]
    fn a_constant_outside_the_range_is_left_alone() {
        for source in [
            "atan(x1) > 2",
            "atan(x1) < -2",
            "tanh(x1) > 1.5",
            "sqrt(x1) < -1",
        ] {
            let expression = crate::parse(source).expect("should compile");
            assert!(
                holds_comparison(&expression.program.body.result)
                    || mentions_any_unary(&expression.program.body.result),
                "{source:?} was inverted when it should not have been"
            );
        }

        // And the meaning is unchanged, which is the part that matters.
        let unsatisfiable = crate::parse("atan(x1) > 2").expect("should compile");
        for step in -100..=100 {
            let x1 = f64::from(step);
            assert!(
                eval::eval_parsed(&unsatisfiable, &[("x1", x1)]).unwrap() > 0.0,
                "atan({x1}) > 2 should never hold"
            );
        }
    }

    fn mentions_any_unary(node: &Expr) -> bool {
        node.iter_preorder()
            .any(|node| matches!(node.kind, Kind::Unary { .. }))
    }

    /// Nothing to invert against. `ln(x1) > x2` has no constant side, so the
    /// pass must leave it for the emitter to report as untranslatable.
    #[test]
    fn a_variable_bound_is_left_alone() {
        let expression = crate::parse("ln(x1) > x2").expect("should compile");
        assert!(
            mentions_unary(&expression.program.body.result, UnaryOp::Ln),
            "a logarithm against a variable was inverted"
        );
    }

    fn holds_comparison(node: &Expr) -> bool {
        node.iter_preorder().any(|node| {
            matches!(
                node.kind,
                Kind::Compare { .. } | Kind::NearEq { .. } | Kind::And { .. }
            )
        })
    }

    /// Unrolling accumulates left to right from the identity, in the order the
    /// terms are written. `0.1 * i` is chosen because `f64` addition is not
    /// associative, so a rebalanced tree would disagree here even though it
    /// agrees on the small integers the corpus uses.
    #[test]
    fn unrolling_accumulates_left_to_right_bit_for_bit() {
        let unrolled = crate::parse("sum(1, 3, i -> 0.1*i)").expect("should compile");
        let by_hand: f64 = ((0.0 + 0.1 * 1.0) + 0.1 * 2.0) + 0.1 * 3.0;
        assert_eq!(
            eval::eval_parsed(&unrolled, &[])
                .expect("should evaluate")
                .to_bits(),
            by_hand.to_bits()
        );
    }

    /// A bound that depends on a variable has no meaning: `sum` is big-sigma
    /// over a fixed index set, not a loop.
    #[test]
    fn a_bound_that_is_not_constant_is_a_compile_error() {
        let failure = crate::parse("sum(x1+0, x1+2, i -> 0.1*i)").expect_err("should not compile");
        let problem = &failure.problems[0];
        assert_eq!(
            problem.kind,
            ProblemKind::AggregateBoundNotConstant {
                bound: BoundKind::Lower
            }
        );
        assert_eq!(problem.span, Span::new(4, 8), "the lower bound expression");
    }

    /// A statically bounded aggregate becomes one n-ary node, not a chain.
    /// A chain is what would put a thousand-term unroll back on the stack.
    #[test]
    fn a_static_aggregate_becomes_one_flat_fold() {
        let expression = crate::parse("sum(1, 5, i -> i)").expect("should compile");

        match &expression.program.body.result.kind {
            Kind::Fold { terms, .. } => assert_eq!(terms.len(), 5),
            other => panic!("expected a flat fold, got {other:?}"),
        }
    }

    /// Past the cap an aggregate is refused, with the count in the message.
    /// `sum(1, 2000, ...)` is 2000 terms against a 1024 limit.
    #[test]
    fn an_aggregate_past_the_cap_is_a_compile_error() {
        let failure = crate::parse("sum(1, 2000, i -> i)").expect_err("should not compile");
        assert_eq!(
            failure.problems[0].kind,
            ProblemKind::AggregateTooWide {
                terms: 2000,
                limit: 1024
            }
        );
    }

    /// Comparisons reach both backends intact.
    ///
    /// The inverse of what this used to assert. `rewrite_booleans` flattened
    /// every one into a residual here, which is the evaluator's convention
    /// applied on `cvg`'s behalf — and it destroyed the structure `cvg` reads.
    /// Each backend lowers them itself now, so their surviving *is* the
    /// invariant.
    #[test]
    fn every_comparison_survives_compilation() {
        for source in [
            "4 < 6",
            "6 > 6",
            "1.0e200 <= 1.0e200",
            "1.0e200 >= 1.0e200",
            "x1 == x2 +/- 0.15",
            "(4 < 6)",
        ] {
            let expression = crate::parse(source)
                .unwrap_or_else(|e| panic!("compile failed for {source:?}: {e}"));
            assert!(
                expression.program.body.expressions().any(holds_comparison),
                "the comparison in {source:?} was flattened during compilation"
            );
        }
    }
}

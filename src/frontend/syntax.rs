//! Lexes and parses text into [`crate::ast`], as a [`RawAst`].
//!
//! **This is the only module allowed to name `antlr4_runtime`.** ANTLR exists
//! purely as a compile step; once [`raw_ast`] returns, everything downstream deals
//! in babel's own owned types. The borrow checker helps enforce this — the parse
//! tree is arena-owned and every typed context borrows from it, so an AST
//! holding ANTLR types could not outlive the parse.
//!
//! The grammar has no labelled alternatives, so `scalarExpr` yields a single
//! `ScalarExprContext` and semantics has to work out which alternative matched.
//! The generated code emits a typed accessor per possible child, so that is a
//! question of which accessor is `Some` — see [`SemanticTranslator::translate_scalar_expr`].

use std::sync::{Arc, Mutex};

use antlr4_runtime::errors::{ErrorListener, SyntaxErrorEvent};
use antlr4_runtime::token::Token;
use antlr4_runtime::{
    AsRuleNode, CommonTokenStream, FromRuleNode, InputStream, ParsedFile, Recognizer,
};

use crate::ast::{
    AggregateKind, Assignment, BinaryOp, Block, CompareOp, Expr, GlobalId, Kind, LocalSlot,
    Program, UnaryOp,
};
use crate::diagnostics::{Problem, ProblemKind, Span};
use crate::frontend::generated::lexer::BabelLexer;
use crate::frontend::generated::parser::{
    self, BabelParser, BooleanExprContext, LiteralContext, ScalarBlockContext,
    ScalarEvaluableContext, ScalarExprContext, StatementBlockContext, StatementContext,
};

/// Everything compilation learns from walking the parse tree: the tree as
/// written, names resolved, before [`rewrite::canonicalize`](super::rewrite::canonicalize)
/// makes it an [`Ast`](super::Ast).
pub(super) struct RawAst {
    pub program: Program,
    /// Distinct statically-referenced names in first-reference order.
    pub symbols: Vec<String>,
    pub is_constraint: bool,
}

/// Collects syntax errors from both the lexer and the parser.
///
/// Every diagnostic is forwarded as reported — no filtering, no coalescing, no
/// rewording. A parser recovering from one mistake may emit several; choosing
/// between them is the caller's business.
#[derive(Clone)]
struct ErrorSink {
    source: Arc<str>,
    problems: Arc<Mutex<Vec<Problem>>>,
}

impl ErrorSink {
    fn new(source: &str) -> Self {
        Self {
            source: Arc::from(source),
            problems: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn take(&self) -> Vec<Problem> {
        std::mem::take(&mut *self.problems.lock().expect("error sink poisoned"))
    }
}

impl<R: Recognizer + ?Sized> ErrorListener<R> for ErrorSink {
    fn syntax_error(&mut self, _recognizer: &R, event: &SyntaxErrorEvent<'_>) {
        let span = event.span.clone().map_or(Span::new(0, 0), |bytes| {
            Span::from_utf8_range(&self.source, bytes)
        });

        // The lexer reports no offending token; the parser always does. That is
        // the only classification available without reading the message, since
        // the runtime formats `MismatchedInput { expected, found }` into prose
        // before listeners see it.
        let kind = ProblemKind::Syntax {
            message: event.message.to_owned(),
            from_lexer: event.offending.is_none(),
        };

        self.problems
            .lock()
            .expect("error sink poisoned")
            .push(Problem::new(kind, &self.source, span));
    }
}

/// The source range a parse-tree context covers.
///
/// `Token::start`/`stop` are already measured in Unicode scalar values, so this
/// needs no byte conversion and no access to the source text. `stop` is
/// inclusive where [`Span`] is half-open, hence the `+ 1`.
///
/// Empty when the rule matched no tokens, which the grammar's optional
/// productions can produce.
fn span_of<'a>(ctx: &impl AsRuleNode<'a>) -> Span {
    let node = ctx.as_rule_node();
    match (node.start(), node.stop()) {
        (Some(first), Some(last)) => Span::new(
            u32::try_from(first.start()).unwrap_or(u32::MAX),
            u32::try_from(last.stop().saturating_add(1)).unwrap_or(u32::MAX),
        ),
        _ => Span::new(0, 0),
    }
}

fn aggregate_kind(ctx: &ScalarExprContext<'_>) -> Option<AggregateKind> {
    if ctx.sum().is_some() {
        Some(AggregateKind::Sum)
    } else if ctx.prod().is_some() {
        Some(AggregateKind::Prod)
    } else {
        None
    }
}

/// Parses `source` from the `scalar_evaluable` entry rule and translates the result to an AST.
///
/// # Errors
/// Returns every syntax error found, or a single problem for an equality
/// tolerance that is non-finite or not positive.
pub(super) fn raw_ast(source: &str) -> Result<RawAst, Vec<Problem>> {
    let sink = ErrorSink::new(source);

    let parsed: ParsedFile = parser::parse_with_parser_constructor(
        source,
        |input| make_lexer(input, sink.clone()),
        |tokens| make_parser(tokens, sink.clone()),
        BabelParser::scalar_evaluable,
    )
    .map_err(|e| {
        vec![Problem::new(
            ProblemKind::Syntax {
                message: e.to_string(),
                from_lexer: false,
            },
            source,
            Span::new(0, 0),
        )]
    })?
    .into_parsed_file();

    let errors = sink.take();
    if !errors.is_empty() {
        return Err(errors);
    }

    let root = parsed
        .tree()
        .as_rule()
        .and_then(ScalarEvaluableContext::from_rule_node)
        .expect("a well-formed parse guarantees this expression");

    SemanticTranslator { source }.translate_scalar_evaluable(&root)
}

/// Whether `name` parses cleanly as a lone variable.
pub(crate) fn parses_as_variable(name: &str) -> bool {
    let sink = ErrorSink::new(name);

    let parsed = parser::parse_with_parser_constructor(
        name,
        |input| make_lexer(input, sink.clone()),
        |tokens| make_parser(tokens, sink.clone()),
        BabelParser::variable_only,
    );

    parsed.is_ok() && sink.take().is_empty()
}

fn make_parser(
    tokens: CommonTokenStream<BabelLexer<InputStream>>,
    err_sink: ErrorSink,
) -> BabelParser<BabelLexer<InputStream>> {
    let mut parser = BabelParser::new(tokens);
    parser.remove_error_listeners();
    parser.add_error_listener(err_sink);
    parser
}

fn make_lexer(input: InputStream, err_sink: ErrorSink) -> BabelLexer<InputStream> {
    let mut lexer = BabelLexer::new(input);
    lexer.remove_error_listeners();
    lexer.add_error_listener(err_sink);
    lexer
}

/// Translates a parse tree into an AST.
///
/// Stateless: the accumulators live in [`TranslationState`], threaded
/// explicitly. Every malformed-tree guard is a panic rather than a reported
/// problem; the source is carried for the one thing the grammar admits and
/// the language refuses, a degenerate equality tolerance.
struct SemanticTranslator<'a> {
    source: &'a str,
}

/// Everything the translation accumulates, threaded explicitly rather than held
/// on the translator so it stays visible in every signature that touches it.
#[derive(Default)]
struct TranslationState {
    /// Distinct global names in first-reference order. A name only lands here
    /// if it did not resolve to a local first.
    globals: Vec<String>,
    /// A stack of lexical scopes, innermost on top. One entry for the root
    /// block today; lambda bodies will push their own.
    scope_stack: Vec<Scope>,
    /// Monotonic — slots are never reused, so a single flat frame serves the
    /// whole tree and a nested block cannot alias an enclosing binding.
    next_slot: u32,
}

/// The bindings introduced by one lexical scope.
///
/// The representation is deliberately behind the struct. A scope holds a lambda
/// parameter and perhaps a couple of `var` bindings, and at that size a linear
/// scan beats hashing and allocates nothing extra — but nothing outside here
/// depends on that, so it can become a map if scopes ever grow.
#[derive(Default)]
struct Scope {
    slot_by_name: Vec<(String, LocalSlot)>,
}

impl TranslationState {
    fn push_scope(&mut self) {
        self.scope_stack.push(Scope::default());
    }

    fn pop_scope(&mut self) {
        self.scope_stack.pop();
    }

    /// Binds `name` in the innermost scope and hands back its slot.
    ///
    /// Re-declaring a name shadows the earlier binding rather than erroring —
    /// well-defined, and the same rule Rust's `let` uses. The JVM
    /// implementation also allowed it, but only because its runtime map
    /// overwrote silently; that is not the reason it is allowed here. Making it
    /// an error is a defensible alternative and would need its own diagnostic.
    fn declare(&mut self, name: &str) -> LocalSlot {
        let slot = LocalSlot::from_index(self.next_slot);
        self.next_slot += 1;
        self.scope_stack
            .last_mut()
            .expect("a scope is pushed before anything is declared")
            .slot_by_name
            .push((name.to_owned(), slot));
        slot
    }

    /// Walks the stack from the top down, and each scope from its newest
    /// binding back. That ordering is the whole of shadowing.
    fn resolve_local(&self, name: &str) -> Option<LocalSlot> {
        self.scope_stack
            .iter()
            .rev()
            .find_map(|scope| {
                scope
                    .slot_by_name
                    .iter()
                    .rev()
                    .find(|(bound, _)| bound == name)
            })
            .map(|(_, slot)| *slot)
    }

    /// Resolves a global, registering it on first reference so ordering is
    /// stable.
    fn get_or_make_id(&mut self, name: &str) -> GlobalId {
        let position = self.globals.iter().position(|known| known == name);
        let position = match position {
            Some(position) => position,
            None => {
                self.globals.push(name.to_owned());
                self.globals.len() - 1
            }
        };
        GlobalId::from_index(u32::try_from(position).unwrap_or(u32::MAX))
    }
}

impl SemanticTranslator<'_> {
    /// Walks a parsed `scalar_evaluable` tree and builds the AST.
    fn translate_scalar_evaluable(
        &self,
        ctx: &ScalarEvaluableContext<'_>,
    ) -> Result<RawAst, Vec<Problem>> {
        let mut state = TranslationState::default();
        let block = ctx
            .statement_block()
            .expect("the grammar requires this block to have a result");

        // The root is the only place a boolean can be, because `lambdaExpr`
        // takes a `scalarBlock`. It did not used to be: the JVM implementation
        // set this flag for a boolean *anywhere* and so called
        // `sum(1, 3, i -> i > 2)` a boolean expression, and the port narrowed
        // the flag to the root while leaving the construct legal. The grammar
        // decides it now, so this is a straight reading rather than a policy.
        let is_constraint = block
            .return_statement()
            .is_ok_and(|ret| ret.boolean_expr().is_some());

        let body = self.translate_block(&block, &mut state)?;

        Ok(RawAst {
            program: Program {
                body,
                frame_size: state.next_slot,
            },
            symbols: state.globals,
            is_constraint,
        })
    }

    /// Translates `statementBlock`, which is both the root of an expression and
    /// the body of every lambda.
    ///
    /// Owns the scope it introduces: bindings made here are invisible once it
    /// returns.
    fn translate_block(
        &self,
        ctx: &StatementBlockContext<'_>,
        state: &mut TranslationState,
    ) -> Result<Block, Vec<Problem>> {
        state.push_scope();

        // `(statement ';')* returnStatement ';'?`
        let assignments = self.translate_assignments(ctx.statement_children(), state)?;

        let ret = ctx
            .return_statement()
            .expect("the grammar requires this block to have a result");

        let result = if let Some(boolean) = ret.boolean_expr() {
            self.translate_boolean_expr(&boolean, state)?
        } else {
            let scalar = ret
                .scalar_expr()
                .expect("the grammar requires this block to have a result");
            self.translate_scalar_expr(&scalar, state)?
        };

        state.pop_scope();

        Ok(Block {
            assignments,
            result,
        })
    }

    /// Translates `scalarBlock`, the body of every lambda.
    ///
    /// A separate rule from `statementBlock` and therefore a separate function,
    /// which is the point: **a lambda body has no route to `booleanExpr`.**
    /// `sum(1, 3, i -> i > 2)` used to parse and quietly sum constraint
    /// residuals — epsilon included, so `prod(1, 3, i -> var a = i; a < 2)`
    /// evaluated to `-2.2e-308`. The grammar refuses it now, so there is no
    /// boolean arm here to write.
    fn translate_scalar_block(
        &self,
        ctx: &ScalarBlockContext<'_>,
        state: &mut TranslationState,
    ) -> Result<Block, Vec<Problem>> {
        state.push_scope();

        // `(statement ';')* scalarReturnStatement ';'?`
        let assignments = self.translate_assignments(ctx.statement_children(), state)?;

        let scalar = ctx
            .scalar_return_statement()
            .expect("the grammar requires this block to have a result")
            .scalar_expr()
            .expect("scalarReturnStatement requires a scalar expression");
        let result = self.translate_scalar_expr(&scalar, state)?;

        state.pop_scope();

        Ok(Block {
            assignments,
            result,
        })
    }

    /// Translates `(statement ';')*`, binding each name after its own value has
    /// been translated.
    ///
    /// That order is the whole of sequential scoping: in `var x = x`, the
    /// right-hand `x` resolves against the enclosing scope because `x` is not
    /// bound until the assignment completes.
    /// Takes the statements rather than the block, because the two block rules
    /// are different generated types that happen to share this part exactly.
    fn translate_assignments<'i>(
        &self,
        statements: impl IntoIterator<Item = StatementContext<'i>>,
        state: &mut TranslationState,
    ) -> Result<Vec<Assignment>, Vec<Problem>> {
        let mut assignments = Vec::new();

        for statement in statements {
            let span = span_of(&statement);
            let assignment = statement
                .assignment()
                .expect("statement requires an assignment");

            let name = assignment
                .name()
                .expect("assignment requires a name and a value")
                .variable_token()
                .map_or(String::new(), |token| {
                    token.symbol().text_or_empty().to_owned()
                });

            let value_ctx = assignment
                .scalar_expr()
                .expect("assignment requires a name and a value");
            let value = self.translate_scalar_expr(&value_ctx, state)?;

            assignments.push(Assignment {
                slot: state.declare(&name),
                value,
                span,
            });
        }

        Ok(assignments)
    }

    // These survive compilation as `Kind::Compare` and `Kind::NearEq`. Each
    // backend lowers them itself: `eval` computes a residual whose sign carries
    // the truth value, `cvg::interval` reads the comparison as a target
    // interval. Neither convention belongs to the front end.
    fn translate_boolean_expr(
        &self,
        ctx: &BooleanExprContext<'_>,
        state: &mut TranslationState,
    ) -> Result<Expr, Vec<Problem>> {
        let span = span_of(ctx);

        // `'(' booleanExpr ')'` — grouping adds no node.
        if let Some(inner) = ctx.boolean_expr() {
            return self.translate_boolean_expr(&inner, state);
        }

        let children: Vec<_> = ctx.scalar_expr_children().collect();
        let lhs = self.operand(&children, 0, state)?;
        let rhs = self.operand(&children, 1, state)?;

        // `scalarExpr eq scalarExpr plusMinus literal` — the grammar requires a
        // literal tolerance, so it is always statically known.
        if ctx.eq().is_some() {
            let literal = ctx
                .literal()
                .expect("the eq alternative requires a literal tolerance");
            let tolerance = translate_literal(&literal);
            // Judged on the value: the tolerance is not an `Expr`, so the
            // folding pass that refuses `1.0e400` elsewhere never sees it, and
            // `0`, `0.0`, `-0.0` and `0.0e1` are one mistake, not four.
            if !tolerance.is_finite() {
                return Err(vec![Problem::new(
                    ProblemKind::NonFiniteConstant { value: tolerance },
                    self.source,
                    span_of(&literal),
                )]);
            }
            if tolerance <= 0.0 {
                return Err(vec![Problem::new(
                    ProblemKind::DegenerateTolerance { tolerance },
                    self.source,
                    span_of(&literal),
                )]);
            }
            return Ok(Expr::new(
                Kind::NearEq {
                    lhs,
                    rhs,
                    tolerance,
                },
                span,
            ));
        }

        let op = if ctx.lteq().is_some() {
            CompareOp::Lte
        } else if ctx.gteq().is_some() {
            CompareOp::Gte
        } else if ctx.lt().is_some() {
            CompareOp::Lt
        } else if ctx.gt().is_some() {
            CompareOp::Gt
        } else {
            unreachable!("booleanExpr has no other alternative");
        };

        Ok(Expr::new(Kind::Compare { op, lhs, rhs }, span))
    }

    /// Lowers the `index`-th sub-expression of a node.
    fn operand(
        &self,
        children: &[ScalarExprContext<'_>],
        index: usize,
        state: &mut TranslationState,
    ) -> Result<Box<Expr>, Vec<Problem>> {
        let child = children
            .get(index)
            .expect("a well-formed parse guarantees this expression");
        Ok(Box::new(self.translate_scalar_expr(child, state)?))
    }

    fn translate_scalar_expr(
        &self,
        ctx: &ScalarExprContext<'_>,
        state: &mut TranslationState,
    ) -> Result<Expr, Vec<Problem>> {
        let span = span_of(ctx);
        let children: Vec<_> = ctx.scalar_expr_children().collect();

        // Ordering matters: `open_paren_token` is present for grouping *and* for
        // every function call and aggregate, so grouping is tested last.

        if let Some(literal) = ctx.literal() {
            return Ok(Expr::new(Kind::Literal(translate_literal(&literal)), span));
        }

        if let Some(variable) = ctx.variable() {
            let name = variable
                .variable_token()
                .map_or(String::new(), |t| t.symbol().text_or_empty().to_owned());
            let kind = state
                .resolve_local(&name)
                .map_or_else(|| Kind::Global(state.get_or_make_id(&name)), Kind::Local);
            return Ok(Expr::new(kind, span));
        }

        // `var '[' scalarExpr ']'` — a one-based index into the whole row, in
        // schema declaration order.
        //
        // No parent check is needed. The JVM implementation's `exitVar` had a
        // `when (ctx.parent)` to tell this from the `var x = …` of an
        // assignment, because a listener sees every `VarContext`; here
        // `assignment` is its own rule and owns its own `var` child.
        if ctx.var().is_some() {
            let subscript = self.operand(&children, 0, state)?;
            return Ok(Expr::new(Kind::DynamicIndex(subscript), span));
        }
        // `(sum | prod) '(' scalarExpr ',' scalarExpr ',' lambdaExpr ')'`
        if let Some(kind) = aggregate_kind(ctx) {
            let lambda = ctx
                .lambda_expr()
                .expect("the aggregate alternative requires a lambda");

            // Bounds translate in the enclosing scope: they cannot see the
            // parameter, which is not bound until the body.
            let lower = self.operand(&children, 0, state)?;
            let upper = self.operand(&children, 1, state)?;

            let name = lambda
                .name()
                .expect("lambdaExpr requires a name and a scalarBlock")
                .variable_token()
                .map_or(String::new(), |token| {
                    token.symbol().text_or_empty().to_owned()
                });
            let body_ctx = lambda
                .scalar_block()
                .expect("lambdaExpr requires a name and a scalarBlock");

            // One scope for the parameter, and `translate_block` pushes another
            // for the body — so a `var i = …` inside shadows the parameter
            // rather than colliding with it.
            state.push_scope();
            let param = state.declare(&name);
            let body = self.translate_scalar_block(&body_ctx, state);
            state.pop_scope();

            return Ok(Expr::new(
                Kind::Aggregate {
                    kind,
                    lower,
                    upper,
                    param,
                    body: Box::new(body?),
                },
                span,
            ));
        }

        // A bare lambda outside an aggregate is unreachable from the grammar.
        if ctx.lambda_expr().is_some() {
            unreachable!("lambdaExpr only appears inside an aggregate");
        }

        if let Some(function) = ctx.binary_function() {
            let op = BinaryOp::from_function_keyword(function.text().as_ref())
                .expect("unknown binaryFunction keyword");
            let lhs = self.operand(&children, 0, state)?;
            let rhs = self.operand(&children, 1, state)?;
            return Ok(Expr::new(Kind::Binary { op, lhs, rhs }, span));
        }

        if let Some(function) = ctx.unary_function() {
            let op = UnaryOp::from_keyword(function.text().as_ref())
                .expect("unknown unaryFunction keyword");
            let arg = self.operand(&children, 0, state)?;
            return Ok(Expr::new(Kind::Unary { op, arg }, span));
        }

        // `negate : '-'` and `minus : '-'` are distinct rules, so unary minus
        // and binary subtraction never collide.
        if ctx.negate().is_some() {
            let arg = self.operand(&children, 0, state)?;
            return Ok(Expr::new(
                Kind::Unary {
                    op: UnaryOp::Negate,
                    arg,
                },
                span,
            ));
        }

        let binary = if ctx.raise().is_some() {
            Some(BinaryOp::Pow)
        } else if ctx.mult().is_some() {
            Some(BinaryOp::Mul)
        } else if ctx.div().is_some() {
            Some(BinaryOp::Div)
        } else if ctx.r#mod().is_some() {
            Some(BinaryOp::Rem)
        } else if ctx.plus().is_some() {
            Some(BinaryOp::Add)
        } else if ctx.minus().is_some() {
            Some(BinaryOp::Sub)
        } else {
            None
        };

        if let Some(op) = binary {
            let lhs = self.operand(&children, 0, state)?;
            let rhs = self.operand(&children, 1, state)?;
            return Ok(Expr::new(Kind::Binary { op, lhs, rhs }, span));
        }

        // `'(' scalarExpr ')'` — grouping adds no node, it just returns the
        // child. Precedence is already encoded in the tree's shape.
        if children.len() == 1 {
            return self.translate_scalar_expr(&children[0], state);
        }

        unreachable!("a well-formed parse guarantees this expression")
    }
}

/// `literal : '-'? (INTEGER | FLOAT | PI | EULERS_E)`
///
/// Note `-3` can arrive here as a negative literal *or* as `negate` applied to
/// `3`, depending on which alternative ANTLR picks. Both are valid and produce
/// the same value; `-3 - -3` and `-3--3` in the corpus pin that.
fn translate_literal(ctx: &LiteralContext<'_>) -> f64 {
    let magnitude: f64 = if let Some(token) = ctx.integer_token().or_else(|| ctx.float_token()) {
        token
            .symbol()
            .text_or_empty()
            .parse::<f64>()
            .expect("the lexer only emits parseable INTEGER and FLOAT")
    } else if ctx.pi_token().is_some() {
        std::f64::consts::PI
    } else if ctx.eulers_e_token().is_some() {
        std::f64::consts::E
    } else {
        unreachable!("literal has no other alternative");
    };

    if ctx.minus_token().is_some() {
        -magnitude
    } else {
        magnitude
    }
}

#[cfg(test)]
mod tests {
    use crate::eval;
    /// A name bound by an assignment must not be reported as a global. The
    /// corpus covers this only indirectly, through `statically_referenced_symbols`.
    #[test]
    fn a_local_is_not_a_global() {
        let expression = crate::parse(
            "var x = x1;
 x + x1",
        )
        .expect("should compile");
        let globals: Vec<&str> = expression.symbols().iter().map(String::as_str).collect();
        assert_eq!(globals, vec!["x1"], "`x` is local and must not be a global");
    }

    /// Innermost-outward resolution: where a local and a global share a name,
    /// the local wins. Nothing in the corpus exercises this.
    #[test]
    fn a_local_shadows_a_global_of_the_same_name() {
        let expression = crate::parse(
            "var x1 = 7;
 x1",
        )
        .expect("should compile");
        assert!(
            expression.symbols().is_empty(),
            "the trailing x1 resolves to the local, so nothing is referenced globally"
        );
        assert_eq!(
            eval::eval_one(expression.source(), &[]).expect("should evaluate"),
            7.0
        );
    }

    /// Sequential scoping: a name is bound only *after* its own value is
    /// translated, so the right-hand side sees the enclosing scope.
    #[test]
    fn an_assignment_does_not_bind_its_own_right_hand_side() {
        let expression = crate::parse(
            "var x1 = x1 + 1;
 x1",
        )
        .expect("should compile");
        let globals: Vec<&str> = expression.symbols().iter().map(String::as_str).collect();
        assert_eq!(globals, vec!["x1"], "the right-hand x1 is the global");
        assert_eq!(
            eval::eval_one(expression.source(), &[("x1", 10.0)]).expect("should evaluate"),
            11.0
        );
    }
}

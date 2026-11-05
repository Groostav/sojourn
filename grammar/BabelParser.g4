parser grammar BabelParser;

options {
    tokenVocab=BabelLexer;
}

scalar_evaluable
    : statementBlock EOF
    ;

// The root of an expression, and the only place a boolean may appear. A lambda
// body is a `scalarBlock` instead: `sum(1, 3, i -> i > 2)` used to parse and
// then quietly sum constraint *residuals*, epsilon and all, so the grammar now
// refuses it rather than the semantics apologising for it afterwards.
statementBlock
    : (statement ';')* returnStatement ';'?
    ;

// A lambda body. Same shape as `statementBlock` minus any route to booleanExpr.
scalarBlock
    : (statement ';')* scalarReturnStatement ';'?
    ;

//used in validation of text fields supplied by the user
variable_only
    : variable EOF
    ;

statement
    : assignment
    ;

returnStatement
    : 'return'? booleanExpr
    | 'return'? scalarExpr
    ;

scalarReturnStatement
    : 'return'? scalarExpr
    ;

assignment
    : var name '=' scalarExpr
    ;

// `p implies q`, binding looser than any comparison, so neither side needs
// brackets. Right recursion rather than a left-recursive rule with an
// `<assoc>`: the grammar admits `a implies b implies c` as `a implies (b
// implies c)`, and `syntax::translate_boolean_expr` refuses that bare chain
// with a caret (`ProblemKind::ChainedImplication`), where a bracketed one
// arrives through `booleanPrimary` and passes.
booleanExpr
    : booleanPrimary (IMPLIES booleanExpr)?
    ;

booleanPrimary
//    locals [ @Nonnull Availability availability = Availability.Runtime ]
// TODO: eagerly evaluated booleans probably arent too useful without universal/existential operators
    : scalarExpr (lt | lteq | gt | gteq) scalarExpr
    | scalarExpr eq scalarExpr plusMinus literal
    | '(' booleanExpr ')'
    ;

scalarExpr
    : literal
    | variable
    | var '[' scalarExpr ']'
    | '(' scalarExpr ')'
    | (sum | prod) '(' scalarExpr ',' scalarExpr ',' lambdaExpr ')'
    | binaryFunction '(' scalarExpr ',' scalarExpr ')'
    | unaryFunction '(' scalarExpr ')'
    | negate scalarExpr
    | scalarExpr raise scalarExpr
    | scalarExpr (mult | div | mod) scalarExpr
    | scalarExpr (plus | minus) scalarExpr
    ;

lambdaExpr
    : name '->' scalarBlock
    ;

plus : '+';
minus : '-';
plusMinus : '+/-';
negate : '-'; //note it is legal to have to productions consuming the same token
mult : '*';
div : '/';
mod : '%';
raise : '^';
sum : 'sum';
prod : 'prod';
lt : '<';
lteq : '<=';
gt : '>';
gteq : '>=';
eq : '=' | '==' ;

var
    : 'var'
    ;

binaryFunction
    : 'max'
    | 'min'
    | 'log'
    ;

unaryFunction
    : 'cos' | 'sin' | 'tan'
    | 'atan' | 'acos' | 'asin'
    | 'sinh' | 'cosh' | 'tanh'
    | 'cot'
    //override Javas default log & log10 with ln & log respectively
    | 'ln' | 'log'
    | 'abs'
    | 'sqrt' | 'cbrt'
    | 'sqr' | 'cube'
    | 'ceil' | 'floor'
    | 'sgn'
    ;

name
    : VARIABLE
    ;

variable : VARIABLE;

literal
    :( INTEGER
    | '-' INTEGER
    | FLOAT
    | '-' FLOAT
    | PI
    | '-' PI
    | EULERS_E
    | '-' EULERS_E
    )
    ;

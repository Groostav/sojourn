lexer grammar BabelLexer;

INTEGER : DIGIT+ ;
FLOAT   : DIGIT* '.' DIGIT+ ( ('e'|'E') '-'? DIGIT+)? ;
fragment DIGIT : '0'..'9';

COS      :    'cos';
SIN      :    'sin';
TAN      :    'tan';
ATAN     :    'atan';
ACOS     :    'acos';
ASIN     :    'asin';
SINH     :    'sinh';
COSH     :    'cosh';
TANH     :    'tanh';
COT      :    'cot';
LN       :    'ln';
LOG      :    'log';
ABS      :    'abs';
SQRT     :    'sqrt';
CBRT     :    'cbrt';
SQR      :    'sqr';
CUBE     :    'cube';
CIEL     :    'ceil';
FLOOR    :    'floor';
MAX      :    'max';
MIN      :    'min';
SGN      :    'sgn';

PI       :    'pi';
EULERS_E :    'e';

TRUE     : 'true';
FALSE    : 'false';

SUM           : 'sum';
PROD          : 'prod';
DYN_VAR       : 'var' ;

LAMBDA        : '->' ;

// Declared before VARIABLE, so `implies` lexes as the keyword on the tie and
// `impliesX` still as a name, by longest match.
IMPLIES       : 'implies' ;

LT            : '<' ;
LTEQ          : '<=' ;
GT            : '>' ;
GTEQ          : '>=' ;
EQ            : '==' ;

MULT          : '*' ;
DIV           : '/' ;
MOD           : '%' ;

PLUS          : '+' ;
MINUS         : '-' ;

EXPONENT      : '^' ;

OPEN_PAREN    : '(' ;
CLOSE_PAREN   : ')' ;
OPEN_BRACKET  : '[' ;
CLOSE_BRACKET : ']' ;

COMMA         : ',' ;

ASSIGN        : '=' ;
RETURN        : 'return' ;

PLUS_MINUS    : '+/-' ;

EOL           : ';' ;

VARIABLE      : VARIABLE_START VARIABLE_PART*;

fragment
VARIABLE_START
    : [a-zA-Z_]
    | ~[\u0000-\u00FF] // U+0100..U+10FFFF; Rust char cannot hold a surrogate
    ;

fragment
VARIABLE_PART
    : [0-9]
    | VARIABLE_START
    ;

LINEBREAKS    : ('\r' | '\n' | ' ') -> skip ;
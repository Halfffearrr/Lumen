# Lumen grammar

Notation: EBNF. `{ X }` means zero or more, `[ X ]` optional, `|` alternation,
`"x"` literal text. Lexical tokens are in `UPPER_CASE`. Statement boundaries are
found from the grammar; semicolons are optional separators.

## Lexical

```ebnf
INT     = DIGIT { DIGIT } ;
FLOAT   = DIGIT { DIGIT } "." DIGIT { DIGIT } ;
IDENT   = (ALPHA | "_") { ALPHA | DIGIT | "_" } ;        (* UTF-8 letters allowed *)
STRING  = '"' { CHAR | ESCAPE | INTERP } '"' ;
INTERP  = "{" expr "}" ;                                  (* string interpolation *)
ESCAPE  = "\" ( "n" | "t" | '"' | "\" | "{" ) ;
COMMENT = "//" { any-char-until-newline } ;               (* skipped *)

keywords: let mut fn if else while for in loop break return true false nil
```

`..` and `..=` are distinct range operators and are not confused with the `.` of
a float literal.

## Program & statements

```ebnf
program   = { statement } ;

statement = let_stmt
          | fn_decl
          | while_stmt
          | for_stmt
          | loop_stmt
          | "break"
          | return_stmt
          | expr_stmt ;

let_stmt    = "let" [ "mut" ] IDENT "=" expr ;
fn_decl     = "fn" IDENT "(" [ params ] ")" block ;
while_stmt  = "while" expr block ;
for_stmt    = "for" IDENT "in" expr block ;
loop_stmt   = "loop" block ;
return_stmt = "return" [ expr ] ;
expr_stmt   = expr ;

params      = IDENT { "," IDENT } ;
block       = "{" { statement } [ expr ] "}" ;   (* trailing expr is the block's value *)
```

A `block`'s value (when used as an expression) is its final expression, or `nil`
if there is none.

## Expressions (lowest to highest precedence)

Expressions are parsed with Pratt (precedence-climbing) parsing.

```ebnf
expr        = assignment ;
assignment  = logic_or [ "=" assignment ] ;            (* target: IDENT or index *)
logic_or    = logic_and { "||" logic_and } ;
logic_and   = equality  { "&&" equality } ;
equality    = comparison { ( "==" | "!=" ) comparison } ;
comparison  = range      { ( "<" | "<=" | ">" | ">=" ) range } ;
range       = additive [ ( ".." | "..=" ) additive ] ;
additive    = multiplicative { ( "+" | "-" ) multiplicative } ;
multiplicative = unary { ( "*" | "/" | "%" ) unary } ;
unary       = ( "!" | "-" ) unary | postfix ;
postfix     = primary { call | index } ;
call        = "(" [ args ] ")" ;
index       = "[" expr "]" ;
args        = expr { "," expr } ;

primary = INT | FLOAT | STRING | "true" | "false" | "nil"
        | IDENT
        | "(" expr ")"
        | list
        | dict
        | if_expr
        | lambda ;

list    = "[" [ expr { "," expr } ] "]" ;
dict    = "{" [ entry { "," entry } ] "}" ;
entry   = expr ":" expr ;
if_expr = "if" expr block [ "else" ( if_expr | block ) ] ;
lambda  = "fn" "(" [ params ] ")" block ;
```

### Notes

- **`if` is an expression.** `let x = if c { a } else { b }` is ordinary; in
  statement position its value is discarded.
- **Blocks vs dicts.** A `{ … }` in *expression position* is always a **dict**
  literal; blocks appear only as the body of `if`/`fn`/`while`/`for`/`loop`.
- **Assignment** is right-associative and only valid with an `IDENT` or an index
  expression on the left.
- **Immutability.** `let` is immutable, `let mut` is mutable; the resolver rejects
  assignment to an immutable binding at compile time.

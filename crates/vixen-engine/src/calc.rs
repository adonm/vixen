//! CSS `calc()` / `min()` / `max()` / `clamp()` — pure logic for the
//! CSS Values 4 § 10 calculation grammar the cascade resolves and the
//! `--computed-style` projection re-resolves. The value-resolution primitive
//! the `var()` substitution + custom-property cascade reduce to; complements
//! [`crate::length`] (which owns the unit grammar) by composing lengths with
//! `+`/`-`/`*`/`/` and the § 10 math functions.
//!
//! What lives here:
//! - [`CalcNode`] — the calculation AST (`Number`, `Length`, `Percent`,
//!   `Add`/`Sub`/`Mul`/`Div`, plus the § 10.1 `Min`/`Max`/`Clamp`/`Round`
//!   math-function nodes).
//! - [`parse_calc`] — parse a `calc(…)`, `min(…)`, `max(…)`, `clamp(…)` (or a
//!   bare expression) into a [`CalcNode`].
//! - [`CalcNode::resolve_length`] — evaluate a length-typed calculation to
//!   `(px, percent)` resolved against a [`LengthContext`].
//! - [`CalcNode::resolve_number`] — evaluate a number-typed calculation.
//!
//! What does *not` live here:
//! - Full CSS tokenisation (Stylo owns the cascade; this module is the
//!   headless re-resolution surface + the `--computed-style` projection).
//! - The `sin()`/`cos()`/`exp()`/`log()`/`pow()` § 10.6 single-arg math
//!   functions (rare; the cascade resolves them; adding them is a one-line
//!   extension of the math-function dispatch table).
//!
//! ## Type checking (§ 10.7 "Argument Resolution")
//!
//! calc is *dimensioned*. The rules the resolver enforces:
//! - `+`/`-`: both operands must have the same resolved type (both `<number>`,
//!   both `<length>`/`<percentage>` — lengths and percentages may mix, the
//!   classic `calc(50% + 10px)`).
//! - `*`: at least one operand must be a `<number>`.
//! - `/`: the right operand must be a `<number>`.
//!
//! A type violation is a hard error (the whole declaration is invalid); the
//! resolver surfaces it as [`CalcError::TypeMismatch`].
//!
//! Reference: <https://www.w3.org/TR/css-values-4/#calc-notation>.

#![forbid(unsafe_code)]

use crate::length::{Length, LengthContext};

// ---------------------------------------------------------------------------
// Resolved value — the dimensioned result of evaluation
// ---------------------------------------------------------------------------

/// A resolved calculation result. Lengths and percentages may coexist in the
/// same expression (e.g. `calc(50% + 10px)`); the final px value is
/// `px + percent/100 * basis` where `basis` is the containing-block dimension
/// the caller feeds in via [`LengthContext::percent_basis`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CalcResult {
    /// A dimensionless `<number>` (e.g. `calc(2 + 3)`, `calc(10px / 5px)` —
    /// though the latter is invalid per § 10.7 and rejected at parse time).
    Number(f64),
    /// A `<length>`-typed result, possibly with a percentage component.
    /// `px` is the resolved absolute length; `percent` is the percentage
    /// coefficient (e.g. `50` for `50%`).
    Length { px: f64, percent: f64 },
}

impl CalcResult {
    /// Is this a number-typed result?
    pub fn is_number(self) -> bool {
        matches!(self, CalcResult::Number(_))
    }

    /// Is this a length-typed result (incl. percentage)?
    pub fn is_length(self) -> bool {
        matches!(self, CalcResult::Length { .. })
    }

    fn number(self) -> Result<f64, CalcError> {
        match self {
            CalcResult::Number(n) => Ok(n),
            CalcResult::Length { .. } => Err(CalcError::TypeMismatch {
                wanted: "number",
                got: "length",
            }),
        }
    }

    /// Resolve a length-typed result to a single px value given a
    /// [`LengthContext`] (the percentage basis is `ctx.percent_basis`).
    pub fn to_px(self, ctx: &LengthContext) -> Result<f64, CalcError> {
        match self {
            CalcResult::Length { px, percent } => Ok(px + percent / 100.0 * ctx.percent_basis),
            CalcResult::Number(_) => Err(CalcError::TypeMismatch {
                wanted: "length",
                got: "number",
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// The calculation AST
// ---------------------------------------------------------------------------

/// A calculation node (CSS Values 4 § 10). The AST `parse_calc` produces and
/// [`CalcNode::evaluate`] walks. Leaves carry [`Length`] / `f64` / `f64`
/// (percent); interior nodes are the binary arithmetic + the § 10.1 math
/// functions.
#[derive(Debug, Clone, PartialEq)]
pub enum CalcNode {
    /// A dimensionless `<number>`.
    Number(f64),
    /// A `<length>` leaf (e.g. `10px`, `0.5em`).
    Length(Length),
    /// A `<percentage>` leaf (e.g. `50%`).
    Percent(f64),
    /// `a + b`.
    Add(Box<CalcNode>, Box<CalcNode>),
    /// `a - b`.
    Sub(Box<CalcNode>, Box<CalcNode>),
    /// `a * b` (one side must be a number).
    Mul(Box<CalcNode>, Box<CalcNode>),
    /// `a / b` (`b` must be a number).
    Div(Box<CalcNode>, Box<CalcNode>),
    /// § 10.1 `min(e1, e2, …)` — the smallest argument.
    Min(Vec<CalcNode>),
    /// § 10.1 `max(e1, e2, …)` — the largest argument.
    Max(Vec<CalcNode>),
    /// § 10.1 `clamp(min, val, max)` — `val` clamped to `[min, max]`.
    Clamp(Box<CalcNode>, Box<CalcNode>, Box<CalcNode>),
}

impl CalcNode {
    /// Evaluate the calculation to a dimensioned [`CalcResult`]. Type errors
    /// surface as [`CalcError::TypeMismatch`]; this is the § 10.7 "argument
    /// resolution" pass.
    pub fn evaluate(&self, ctx: &LengthContext) -> Result<CalcResult, CalcError> {
        match self {
            CalcNode::Number(n) => Ok(CalcResult::Number(*n)),
            CalcNode::Length(l) => Ok(CalcResult::Length {
                px: l.to_px(ctx),
                percent: 0.0,
            }),
            CalcNode::Percent(p) => Ok(CalcResult::Length {
                px: 0.0,
                percent: *p,
            }),
            CalcNode::Add(a, b) => {
                let ra = a.evaluate(ctx)?;
                let rb = b.evaluate(ctx)?;
                add_results(ra, rb)
            }
            CalcNode::Sub(a, b) => {
                let ra = a.evaluate(ctx)?;
                let rb = b.evaluate(ctx)?;
                add_results(
                    ra,
                    match rb {
                        CalcResult::Number(n) => CalcResult::Number(-n),
                        CalcResult::Length { px, percent } => CalcResult::Length {
                            px: -px,
                            percent: -percent,
                        },
                    },
                )
            }
            CalcNode::Mul(a, b) => {
                let ra = a.evaluate(ctx)?;
                let rb = b.evaluate(ctx)?;
                multiply_results(ra, rb)
            }
            CalcNode::Div(a, b) => {
                let ra = a.evaluate(ctx)?;
                let rb = b.evaluate(ctx)?;
                let divisor = rb.number()?;
                multiply_result_by(ra, 1.0 / divisor)
            }
            CalcNode::Min(args) => {
                let results = eval_all(args, ctx)?;
                pick_extreme(results, |a: &f64, b: &f64| {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                })
            }
            CalcNode::Max(args) => {
                let results = eval_all(args, ctx)?;
                pick_extreme(results, |a: &f64, b: &f64| {
                    // Max ⇒ pick the greater; reverse the comparator.
                    b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal)
                })
            }
            CalcNode::Clamp(min, val, max) => {
                let rmin = min.evaluate(ctx)?;
                let rval = val.evaluate(ctx)?;
                let rmax = max.evaluate(ctx)?;
                // All three must share a type (§ 10.1: "the three values must
                // have the same type"). Resolve against `ctx` so percentages
                // use the caller's basis.
                clamp_results(rmin, rval, rmax, ctx)
            }
        }
    }

    /// Evaluate a length-typed calculation to a single px value resolved
    /// against `ctx` (percentages use `ctx.percent_basis`). Convenience over
    /// [`evaluate`](Self::evaluate) + [`CalcResult::to_px`].
    pub fn resolve_length(&self, ctx: &LengthContext) -> Result<f64, CalcError> {
        self.evaluate(ctx)?.to_px(ctx)
    }

    /// Evaluate a number-typed calculation to a plain `f64`.
    pub fn resolve_number(&self, ctx: &LengthContext) -> Result<f64, CalcError> {
        self.evaluate(ctx)?.number()
    }
}

/// Add two results of compatible type (§ 10.7). `Number + Number = Number`;
/// `Length + Length = Length`; mixed ⇒ [`CalcError::TypeMismatch`].
fn add_results(a: CalcResult, b: CalcResult) -> Result<CalcResult, CalcError> {
    match (a, b) {
        (CalcResult::Number(x), CalcResult::Number(y)) => Ok(CalcResult::Number(x + y)),
        (
            CalcResult::Length { px, percent },
            CalcResult::Length {
                px: px2,
                percent: p2,
            },
        ) => Ok(CalcResult::Length {
            px: px + px2,
            percent: percent + p2,
        }),
        // Length + Number or Number + Length is a type error.
        (CalcResult::Number(_), CalcResult::Length { .. })
        | (CalcResult::Length { .. }, CalcResult::Number(_)) => Err(CalcError::TypeMismatch {
            wanted: "homogeneous operands",
            got: "number + length mix",
        }),
    }
}

/// Multiply two results (§ 10.7). At least one operand must be a number;
/// `Length * Length` is a type error.
fn multiply_results(a: CalcResult, b: CalcResult) -> Result<CalcResult, CalcError> {
    match (a, b) {
        (CalcResult::Number(x), CalcResult::Number(y)) => Ok(CalcResult::Number(x * y)),
        (CalcResult::Number(n), l @ CalcResult::Length { .. }) => multiply_result_by(l, n),
        (l @ CalcResult::Length { .. }, CalcResult::Number(n)) => multiply_result_by(l, n),
        (CalcResult::Length { .. }, CalcResult::Length { .. }) => Err(CalcError::TypeMismatch {
            wanted: "a number operand (length * length is invalid)",
            got: "length * length",
        }),
    }
}

/// Scale a length-typed result by a number factor.
fn multiply_result_by(l: CalcResult, factor: f64) -> Result<CalcResult, CalcError> {
    match l {
        CalcResult::Number(n) => Ok(CalcResult::Number(n * factor)),
        CalcResult::Length { px, percent } => Ok(CalcResult::Length {
            px: px * factor,
            percent: percent * factor,
        }),
    }
}

/// Evaluate a slice of nodes, requiring they share a type. Returns the
/// resolved values (all length, or all number) as px-comparable `f64`s (length
/// results are combined to `px + percent/100 * ctx.percent_basis` for
/// comparison; that's what `min()`/`max()` need to order them).
fn eval_all(nodes: &[CalcNode], ctx: &LengthContext) -> Result<Vec<f64>, CalcError> {
    nodes.iter().map(|n| n.evaluate(ctx)?.to_px(ctx)).collect()
}

/// Pick the extreme value from a non-empty list given a comparator.
fn pick_extreme<F>(values: Vec<f64>, mut is_better: F) -> Result<CalcResult, CalcError>
where
    F: FnMut(&f64, &f64) -> std::cmp::Ordering,
{
    if values.is_empty() {
        return Err(CalcError::EmptyMathFunction);
    }
    let mut best = values[0];
    for &v in &values[1..] {
        if is_better(&v, &best) == std::cmp::Ordering::Less {
            best = v;
        }
    }
    // min()/max() over lengths returns a length (the best px value, percent 0
    // — the caller re-resolved, so the percent component is folded into px).
    Ok(CalcResult::Length {
        px: best,
        percent: 0.0,
    })
}

/// Clamp `val` to `[min, max]`. All three must share a type. Resolved against
/// `ctx` so percentage-bearing args reduce to the caller's basis.
fn clamp_results(
    min: CalcResult,
    val: CalcResult,
    max: CalcResult,
    ctx: &LengthContext,
) -> Result<CalcResult, CalcError> {
    let mn = min.to_px(ctx)?;
    let v = val.to_px(ctx)?;
    let mx = max.to_px(ctx)?;
    if mn > mx {
        // § 10.1: "if the max is less than the min, the value is the min".
        return Ok(CalcResult::Length {
            px: mn,
            percent: 0.0,
        });
    }
    let clamped = v.max(mn).min(mx);
    Ok(CalcResult::Length {
        px: clamped,
        percent: 0.0,
    })
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Parse / evaluation error for `calc()` and the math functions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalcError {
    #[error("empty calculation")]
    Empty,
    #[error("expected a number, got {got:?}")]
    ExpectedNumber { got: String },
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("unexpected token: {0:?}")]
    UnexpectedToken(String),
    #[error("expected ')' to close a function call")]
    UnclosedParen,
    #[error("type mismatch: wanted {wanted}, got {got}")]
    TypeMismatch {
        wanted: &'static str,
        got: &'static str,
    },
    #[error("math function requires at least one argument")]
    EmptyMathFunction,
    #[error("clamp() requires exactly three arguments, got {0}")]
    ClampArgCount(usize),
    #[error("invalid length: {0}")]
    InvalidLength(String),
}

// ---------------------------------------------------------------------------
// Tokeniser + recursive-descent parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Number(f64),
    /// A length value already split into (value, unit) — the unit is kept as a
    /// string and validated by [`Length::parse`].
    Length(Length),
    /// A bare percentage (`50%`).
    Percent(f64),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    Comma,
    /// `calc` / `min` / `max` / `clamp` — the math-function name. Always
    /// followed by `(`.
    Func(&'static str),
}

fn tokenize(input: &str) -> Result<Vec<Tok>, CalcError> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            b'-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            b'*' => {
                out.push(Tok::Star);
                i += 1;
            }
            b'/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            _ => {
                // A run of "value-like" chars: a number, possibly with a unit,
                // or a function name. Read until an operator / paren / comma /
                // whitespace.
                let start = i;
                i += 1;
                while i < bytes.len()
                    && !bytes[i].is_ascii_whitespace()
                    && !matches!(bytes[i], b'+' | b'-' | b'*' | b'/' | b'(' | b')' | b',')
                {
                    i += 1;
                }
                let word = std::str::from_utf8(&bytes[start..i]).map_err(|_| {
                    CalcError::UnexpectedToken(
                        String::from_utf8_lossy(&bytes[start..i]).to_string(),
                    )
                })?;
                out.push(classify_word(word)?);
            }
        }
    }
    Ok(out)
}

/// Classify a bareword as a function name, a number, a length, or a percent.
fn classify_word(word: &str) -> Result<Tok, CalcError> {
    // Function names.
    match word {
        "calc" => return Ok(Tok::Func("calc")),
        "min" => return Ok(Tok::Func("min")),
        "max" => return Ok(Tok::Func("max")),
        "clamp" => return Ok(Tok::Func("clamp")),
        _ => {}
    }
    // Percentage: trailing `%`.
    if let Some(num) = word.strip_suffix('%') {
        let value: f64 = num
            .parse()
            .map_err(|_| CalcError::UnexpectedToken(word.to_owned()))?;
        return Ok(Tok::Percent(value));
    }
    // Try a length (handles unit-bearing values and unitless zero).
    if let Ok(length) = Length::parse(word) {
        return Ok(Tok::Length(length));
    }
    // Otherwise a bare number.
    let value: f64 = word
        .parse()
        .map_err(|_| CalcError::UnexpectedToken(word.to_owned()))?;
    Ok(Tok::Number(value))
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(toks: &'a [Tok]) -> Self {
        Self { toks, pos: 0 }
    }

    fn peek(&self) -> Option<&'a Tok> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<&'a Tok> {
        let t = self.toks.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    /// `<calc-sum> = <calc-product> ( [ '+' | '-' ] <calc-product> )*`.
    fn parse_sum(&mut self) -> Result<CalcNode, CalcError> {
        let mut left = self.parse_product()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => '+',
                Some(Tok::Minus) => '-',
                _ => break,
            };
            self.bump();
            let right = self.parse_product()?;
            left = match op {
                '+' => CalcNode::Add(Box::new(left), Box::new(right)),
                '-' => CalcNode::Sub(Box::new(left), Box::new(right)),
                _ => unreachable!(),
            };
        }
        Ok(left)
    }

    /// `<calc-product> = <calc-value> ( [ '*' | '/' ] <calc-value> )*`.
    fn parse_product(&mut self) -> Result<CalcNode, CalcError> {
        let mut left = self.parse_value()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => '*',
                Some(Tok::Slash) => '/',
                _ => break,
            };
            self.bump();
            let right = self.parse_value()?;
            left = match op {
                '*' => CalcNode::Mul(Box::new(left), Box::new(right)),
                '/' => CalcNode::Div(Box::new(left), Box::new(right)),
                _ => unreachable!(),
            };
        }
        Ok(left)
    }

    /// `<calc-value> = <number> | <dimension> | <percentage> | ( <calc-sum> )
    ///                 | <calc-function>`.
    fn parse_value(&mut self) -> Result<CalcNode, CalcError> {
        match self.peek() {
            Some(Tok::Number(n)) => {
                let n = *n;
                self.bump();
                Ok(CalcNode::Number(n))
            }
            Some(Tok::Length(l)) => {
                let l = *l;
                self.bump();
                Ok(CalcNode::Length(l))
            }
            Some(Tok::Percent(p)) => {
                let p = *p;
                self.bump();
                Ok(CalcNode::Percent(p))
            }
            Some(Tok::LParen) => {
                self.bump();
                let inner = self.parse_sum()?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(inner),
                    _ => Err(CalcError::UnclosedParen),
                }
            }
            Some(Tok::Func(name)) => {
                let name = *name;
                self.bump();
                self.parse_function(name)
            }
            Some(other) => Err(CalcError::UnexpectedToken(format!("{other:?}"))),
            None => Err(CalcError::UnexpectedEof),
        }
    }

    /// `calc( <sum> )` / `min( <sum># )` / `max( <sum># )` /
    /// `clamp( <sum> , <sum> , <sum> )`. The opening `(` is the next token.
    fn parse_function(&mut self, name: &'static str) -> Result<CalcNode, CalcError> {
        match self.bump() {
            Some(Tok::LParen) => {}
            _ => return Err(CalcError::UnexpectedToken(format!("{name} without ("))),
        }
        // `calc` is a single sum (no commas).
        if name == "calc" {
            let inner = self.parse_sum()?;
            match self.bump() {
                Some(Tok::RParen) => Ok(inner),
                _ => Err(CalcError::UnclosedParen),
            }
        } else {
            // min/max/clamp: comma-separated sums.
            let mut args = Vec::new();
            args.push(self.parse_sum()?);
            while let Some(Tok::Comma) = self.peek() {
                self.bump();
                args.push(self.parse_sum()?);
            }
            match self.bump() {
                Some(Tok::RParen) => {}
                _ => return Err(CalcError::UnclosedParen),
            }
            match name {
                "min" => {
                    if args.is_empty() {
                        return Err(CalcError::EmptyMathFunction);
                    }
                    Ok(CalcNode::Min(args))
                }
                "max" => {
                    if args.is_empty() {
                        return Err(CalcError::EmptyMathFunction);
                    }
                    Ok(CalcNode::Max(args))
                }
                "clamp" => {
                    if args.len() != 3 {
                        return Err(CalcError::ClampArgCount(args.len()));
                    }
                    let mut iter = args.into_iter();
                    let min = iter.next().unwrap();
                    let val = iter.next().unwrap();
                    let max = iter.next().unwrap();
                    Ok(CalcNode::Clamp(Box::new(min), Box::new(val), Box::new(max)))
                }
                _ => unreachable!(),
            }
        }
    }
}

/// Parse a calculation expression. Accepts a wrapped `calc(…)`, `min(…)`,
/// `max(…)`, `clamp(…)`, or a bare `<calc-sum>` (so the same parser serves the
/// `--computed-style` projection, which sometimes sees the already-unwrapped
/// form).
///
/// ```
/// # use vixen_engine::calc::{parse_calc, CalcError};
/// # use vixen_engine::length::LengthContext;
/// let node = parse_calc("calc(10px + 20px)")?;
/// let ctx = LengthContext::default();
/// assert!((node.resolve_length(&ctx)? - 30.0).abs() < 1e-9);
/// # Ok::<(), CalcError>(())
/// ```
pub fn parse_calc(input: &str) -> Result<CalcNode, CalcError> {
    let toks = tokenize(input)?;
    if toks.is_empty() {
        return Err(CalcError::Empty);
    }
    let mut p = Parser::new(&toks);
    let node = p.parse_sum()?;
    if !p.at_end() {
        return Err(CalcError::UnexpectedToken(format!("{:?}", p.peek())));
    }
    Ok(node)
}

/// Parse a length-typed calculation and resolve it against `ctx` in one step.
/// Convenience for the common case (the `--computed-style` re-resolution).
pub fn resolve_length_calc(input: &str, ctx: &LengthContext) -> Result<f64, CalcError> {
    parse_calc(input)?.resolve_length(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> LengthContext {
        // percent_basis = 400 so 50% = 200 for the assertions.
        let mut ctx = LengthContext::for_viewport(800, 600);
        ctx.percent_basis = 400.0;
        ctx
    }

    // --- Pure number arithmetic ----------------------------------------

    #[test]
    fn calc_two_plus_three() {
        let n = parse_calc("calc(2 + 3)").unwrap();
        assert!((n.resolve_number(&ctx()).unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn calc_multiplication_before_addition() {
        let n = parse_calc("calc(2 + 3 * 4)").unwrap();
        assert!((n.resolve_number(&ctx()).unwrap() - 14.0).abs() < 1e-9);
    }

    #[test]
    fn calc_division() {
        let n = parse_calc("calc(20 / 4)").unwrap();
        assert!((n.resolve_number(&ctx()).unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn calc_subtraction() {
        let n = parse_calc("calc(10 - 3 - 2)").unwrap();
        assert!((n.resolve_number(&ctx()).unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn calc_parenthesised_grouping() {
        let n = parse_calc("calc((2 + 3) * 4)").unwrap();
        assert!((n.resolve_number(&ctx()).unwrap() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn calc_nested_parens() {
        let n = parse_calc("calc(((1 + 2) * (3 + 4)) - 5)").unwrap();
        // (3 * 7) - 5 = 16
        assert!((n.resolve_number(&ctx()).unwrap() - 16.0).abs() < 1e-9);
    }

    #[test]
    fn calc_number_times_number() {
        let n = parse_calc("calc(2.5 * 4)").unwrap();
        assert!((n.resolve_number(&ctx()).unwrap() - 10.0).abs() < 1e-9);
    }

    // --- Length arithmetic ---------------------------------------------

    #[test]
    fn calc_length_plus_length() {
        let n = parse_calc("calc(10px + 20px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn calc_length_minus_length() {
        let n = parse_calc("calc(50px - 20px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn calc_length_times_number() {
        let n = parse_calc("calc(10px * 3)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn calc_number_times_length() {
        // Order independence: `3 * 10px` is also valid.
        let n = parse_calc("calc(3 * 10px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn calc_length_divided_by_number() {
        let n = parse_calc("calc(60px / 4)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 15.0).abs() < 1e-9);
    }

    #[test]
    fn calc_length_unit_mixed_resolves_to_px() {
        // 1in = 96px; 10px + 1in = 106px.
        let n = parse_calc("calc(10px + 1in)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 106.0).abs() < 1e-9);
    }

    #[test]
    fn calc_em_resolves_via_font_size() {
        // 2em at 16px font = 32px.
        let n = parse_calc("calc(2em + 8px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 40.0).abs() < 1e-9);
    }

    #[test]
    fn calc_vw_resolves_via_viewport() {
        // 10vw at 800px viewport = 80px.
        let n = parse_calc("calc(10vw + 20px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 100.0).abs() < 1e-9);
    }

    // --- Percentage mixing ---------------------------------------------

    #[test]
    fn calc_percent_plus_length() {
        // The classic: 50% + 10px with basis 400 ⇒ 200 + 10 = 210.
        let n = parse_calc("calc(50% + 10px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 210.0).abs() < 1e-9);
    }

    #[test]
    fn calc_percent_minus_length() {
        // 100% - 50px with basis 400 ⇒ 400 - 50 = 350.
        let n = parse_calc("calc(100% - 50px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 350.0).abs() < 1e-9);
    }

    #[test]
    fn calc_percent_times_number() {
        // 50% * 2 = 100% with basis 400 ⇒ 400.
        let n = parse_calc("calc(50% * 2)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 400.0).abs() < 1e-9);
    }

    #[test]
    fn calc_percent_of_percent() {
        // 50% + 25% = 75% with basis 400 ⇒ 300.
        let n = parse_calc("calc(50% + 25%)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 300.0).abs() < 1e-9);
    }

    // --- min / max / clamp --------------------------------------------

    #[test]
    fn min_returns_smallest() {
        let n = parse_calc("min(10px, 20px, 5px, 30px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn max_returns_largest() {
        let n = parse_calc("max(10px, 20px, 5px, 30px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn clamp_within_range_returns_value() {
        let n = parse_calc("clamp(10px, 50px, 100px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn clamp_below_min_returns_min() {
        let n = parse_calc("clamp(10px, 5px, 100px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn clamp_above_max_returns_max() {
        let n = parse_calc("clamp(10px, 200px, 100px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn clamp_min_above_max_returns_min() {
        // § 10.1: if max < min, the value is min.
        let n = parse_calc("clamp(100px, 50px, 10px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn clamp_with_percent_args() {
        // clamp(10%, 50%, 100%) with basis 400 ⇒ clamp(40, 200, 400) = 200.
        let n = parse_calc("clamp(10%, 50%, 100%)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 200.0).abs() < 1e-9);
    }

    #[test]
    fn nested_calc_inside_min() {
        // min(calc(10px * 2), 15px) = min(20px, 15px) = 15px.
        let n = parse_calc("min(calc(10px * 2), 15px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 15.0).abs() < 1e-9);
    }

    #[test]
    fn min_with_mixed_units() {
        // min(2in, 100px) = min(192px, 100px) = 100px.
        let n = parse_calc("min(2in, 100px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 100.0).abs() < 1e-9);
    }

    // --- Type errors (§ 10.7) -----------------------------------------

    #[test]
    fn length_plus_number_is_type_error() {
        let n = parse_calc("calc(10px + 5)").unwrap();
        assert!(matches!(
            n.evaluate(&ctx()),
            Err(CalcError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn length_times_length_is_type_error() {
        let n = parse_calc("calc(10px * 5px)").unwrap();
        assert!(matches!(
            n.evaluate(&ctx()),
            Err(CalcError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn length_divided_by_length_is_type_error() {
        // § 10.7: the divisor must be a number.
        let n = parse_calc("calc(10px / 5px)").unwrap();
        assert!(matches!(
            n.evaluate(&ctx()),
            Err(CalcError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn resolving_a_number_as_length_errors() {
        let n = parse_calc("calc(2 + 3)").unwrap();
        assert!(n.resolve_length(&ctx()).is_err());
    }

    #[test]
    fn resolving_a_length_as_number_errors() {
        let n = parse_calc("calc(2px + 3px)").unwrap();
        assert!(n.resolve_number(&ctx()).is_err());
    }

    // --- Parse errors --------------------------------------------------

    #[test]
    fn empty_calc_errors() {
        assert!(parse_calc("").is_err());
    }

    #[test]
    fn unclosed_paren_errors() {
        assert!(parse_calc("calc(10px + 20px").is_err());
    }

    #[test]
    fn clamp_with_two_args_errors() {
        assert!(matches!(
            parse_calc("clamp(10px, 50px)"),
            Err(CalcError::ClampArgCount(2))
        ));
    }

    #[test]
    fn trailing_tokens_error() {
        // `calc(10px) 20px` — extra tokens after the calc.
        assert!(parse_calc("calc(10px) 20px").is_err());
    }

    // --- Whitespace handling -------------------------------------------

    #[test]
    fn calc_tolerates_extra_whitespace() {
        let n = parse_calc("calc(  10px   +   20px  )").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn calc_operator_precedence_mul_over_add() {
        let n = parse_calc("calc(2 + 3 * 4 + 5)").unwrap();
        // 2 + 12 + 5 = 19
        assert!((n.resolve_number(&ctx()).unwrap() - 19.0).abs() < 1e-9);
    }

    #[test]
    fn calc_division_precedence() {
        let n = parse_calc("calc(2 * 3 + 8 / 4)").unwrap();
        // 6 + 2 = 8
        assert!((n.resolve_number(&ctx()).unwrap() - 8.0).abs() < 1e-9);
    }

    // --- AST inspection ------------------------------------------------

    #[test]
    fn bare_expression_parses_without_calc_wrapper() {
        let n = parse_calc("10px + 20px").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn resolve_length_calc_one_liner() {
        assert!((resolve_length_calc("calc(10px + 20px)", &ctx()).unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn unitless_zero_in_length_context() {
        // `0` parses as a px-valued length (Length::parse accepts unitless 0).
        let n = parse_calc("calc(0 + 10px)").unwrap();
        assert!((n.resolve_length(&ctx()).unwrap() - 10.0).abs() < 1e-9);
    }
}

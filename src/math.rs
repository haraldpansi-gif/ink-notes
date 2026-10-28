//! Exact evaluation of handwritten math (Issue #46, stage 1).
//!
//! Takes the text of a recognised expression such as `2^4`, `2,5 · 3` or `(x + 3)²`
//! (optionally ending with `=`) and returns its result as text: `16`, `7,5` and
//! `x² + 6x + 9`. Numbers are exact fractions, so `1 : 3 · 3` gives `1`, and expressions
//! with variables are expanded into polynomials.
//!
//! The input is lenient towards how math is written by hand: decimal comma or point,
//! `·`, `×`, `*` and `:`, `÷`, `/`, superscript exponents, Unicode minus signs and implicit
//! multiplication (`2x`, `3(x + 1)`, `(a + b)(a − b)`).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Largest exponent for a polynomial that is not a plain number
const MAX_POLYNOMIAL_EXPONENT: u32 = 32;
/// Largest exponent for a plain number
const MAX_NUMBER_EXPONENT: u32 = 1000;
/// Results with more terms are not shown (they would not fit next to the `=` anyway)
const MAX_TERMS: usize = 64;
/// Terminating decimals with more digits after the separator are shown as fractions
const MAX_DECIMALS: u32 = 12;

/// Computes the result of `expression`, written for display after its `=` sign.
///
/// `decimal_comma` selects the decimal separator of the result. Returns `None` if the
/// expression cannot be read or computed (e.g. division by zero, a variable in a
/// denominator or overflow), and also if there is nothing to compute, i.e. the result would
/// just repeat the input (`5`, `x`, `2x`).
pub fn solve(expression: &str, decimal_comma: bool) -> Option<String> {
    let tokens = tokenize(expression)?;
    let ast = Parser::new(&tokens).parse()?;
    if matches!(ast, Expr::Number(_) | Expr::Variable(_)) {
        return None;
    }
    let result = ast.eval()?;
    if result.terms.len() > MAX_TERMS {
        return None;
    }
    let text = result.format(decimal_comma);
    let compact = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| if c == '−' { '-' } else { c })
            .collect()
    };
    if compact(&text) == compact(expression.trim().trim_end_matches('=')) {
        return None;
    }
    Some(text)
}

/// `expression` written neatly for showing it, e.g. `2⁴` for `2^(4)`, `(x + 1)/2` for
/// `((x+1)/(2))` or `2,5 · 3` for `2,5*3`. `None` if it cannot be read.
pub fn display(expression: &str, decimal_comma: bool) -> Option<String> {
    let tokens = tokenize(expression)?;
    let ast = Parser::new(&tokens).parse()?;
    Some(ast.display(decimal_comma))
}

// --- Rational numbers ---------------------------------------------------------------------

/// Exact fraction `num / den` with `den > 0`, always reduced
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rational {
    num: i128,
    den: i128,
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Rational {
    const ZERO: Rational = Rational { num: 0, den: 1 };
    const ONE: Rational = Rational { num: 1, den: 1 };

    fn new(num: i128, den: i128) -> Option<Rational> {
        // i128::MIN has no positive counterpart, which `gcd` and `neg` need
        if den == 0 || num == i128::MIN || den == i128::MIN {
            return None;
        }
        let g = gcd(num, den).max(1);
        let (mut num, mut den) = (num / g, den / g);
        if den < 0 {
            num = num.checked_neg()?;
            den = den.checked_neg()?;
        }
        Some(Rational { num, den })
    }

    fn integer(n: i128) -> Rational {
        Rational { num: n, den: 1 }
    }

    fn is_zero(self) -> bool {
        self.num == 0
    }

    fn add(self, other: Rational) -> Option<Rational> {
        let g = gcd(self.den, other.den);
        let den = (self.den / g).checked_mul(other.den)?;
        let num = self
            .num
            .checked_mul(other.den / g)?
            .checked_add(other.num.checked_mul(self.den / g)?)?;
        Rational::new(num, den)
    }

    fn neg(self) -> Option<Rational> {
        Some(Rational {
            num: self.num.checked_neg()?,
            den: self.den,
        })
    }

    fn mul(self, other: Rational) -> Option<Rational> {
        // Cross-reducing first keeps the intermediate products small
        let g1 = gcd(self.num, other.den).max(1);
        let g2 = gcd(other.num, self.den).max(1);
        let num = (self.num / g1).checked_mul(other.num / g2)?;
        let den = (self.den / g2).checked_mul(other.den / g1)?;
        Rational::new(num, den)
    }

    fn recip(self) -> Option<Rational> {
        Rational::new(self.den, self.num)
    }

    fn pow(self, exponent: i128) -> Option<Rational> {
        if exponent.unsigned_abs() > MAX_NUMBER_EXPONENT as u128 {
            return None;
        }
        let base = if exponent < 0 { self.recip()? } else { self };
        let mut result = Rational::ONE;
        for _ in 0..exponent.unsigned_abs() {
            result = result.mul(base)?;
        }
        Some(result)
    }

    fn as_integer(self) -> Option<i128> {
        (self.den == 1).then_some(self.num)
    }

    /// The number without its sign: as a terminating decimal if it has one with at most
    /// [`MAX_DECIMALS`] digits, otherwise as a fraction `p/q`.
    fn format_abs(self, decimal_comma: bool) -> String {
        let num = self.num.unsigned_abs();
        let den = self.den.unsigned_abs();
        let whole = num / den;
        let mut rest = num % den;
        if rest == 0 {
            return whole.to_string();
        }
        let mut reduced = den;
        let mut decimals = 0;
        for factor in [2, 5] {
            while reduced.is_multiple_of(factor) {
                reduced /= factor;
            }
        }
        if reduced == 1 {
            let mut digits = String::new();
            while rest != 0 && decimals <= MAX_DECIMALS {
                let Some(shifted) = rest.checked_mul(10) else {
                    break;
                };
                rest = shifted;
                digits.push(char::from(b'0' + (rest / den) as u8));
                rest %= den;
                decimals += 1;
            }
            if rest == 0 {
                let separator = if decimal_comma { ',' } else { '.' };
                return format!("{whole}{separator}{digits}");
            }
        }
        format!("{num}/{den}")
    }
}

// --- Polynomials --------------------------------------------------------------------------

/// Product of variables with their exponents, e.g. `a²b` = `{a: 2, b: 1}`
type Monomial = BTreeMap<char, u32>;

/// Sum of monomials with exact coefficients; a plain number is the empty monomial
#[derive(Debug, Clone, PartialEq, Eq)]
struct Polynomial {
    terms: BTreeMap<Monomial, Rational>,
}

impl Polynomial {
    fn constant(value: Rational) -> Polynomial {
        let mut terms = BTreeMap::new();
        if !value.is_zero() {
            terms.insert(Monomial::new(), value);
        }
        Polynomial { terms }
    }

    fn variable(name: char) -> Polynomial {
        let mut terms = BTreeMap::new();
        terms.insert(Monomial::from([(name, 1)]), Rational::ONE);
        Polynomial { terms }
    }

    /// The value if this is a plain number
    fn as_constant(&self) -> Option<Rational> {
        match self.terms.len() {
            0 => Some(Rational::ZERO),
            1 => self.terms.get(&Monomial::new()).copied(),
            _ => None,
        }
    }

    fn add_term(&mut self, monomial: Monomial, coefficient: Rational) -> Option<()> {
        let sum = match self.terms.get(&monomial) {
            Some(existing) => existing.add(coefficient)?,
            None => coefficient,
        };
        if sum.is_zero() {
            self.terms.remove(&monomial);
        } else {
            self.terms.insert(monomial, sum);
        }
        Some(())
    }

    fn add(mut self, other: &Polynomial) -> Option<Polynomial> {
        for (monomial, coefficient) in &other.terms {
            self.add_term(monomial.clone(), *coefficient)?;
        }
        Some(self)
    }

    fn neg(&self) -> Option<Polynomial> {
        let mut terms = BTreeMap::new();
        for (monomial, coefficient) in &self.terms {
            terms.insert(monomial.clone(), coefficient.neg()?);
        }
        Some(Polynomial { terms })
    }

    fn mul(&self, other: &Polynomial) -> Option<Polynomial> {
        let mut product = Polynomial::constant(Rational::ZERO);
        for (m1, c1) in &self.terms {
            for (m2, c2) in &other.terms {
                let mut monomial = m1.clone();
                for (&name, &exponent) in m2 {
                    let e = monomial.entry(name).or_insert(0);
                    *e = e.checked_add(exponent)?;
                }
                product.add_term(monomial, c1.mul(*c2)?)?;
            }
            if product.terms.len() > MAX_TERMS * 4 {
                return None;
            }
        }
        Some(product)
    }

    fn div(&self, divisor: &Polynomial) -> Option<Polynomial> {
        let factor = divisor.as_constant()?.recip()?;
        self.mul(&Polynomial::constant(factor))
    }

    fn pow(&self, exponent: &Polynomial) -> Option<Polynomial> {
        let exponent = exponent.as_constant()?.as_integer()?;
        if let Some(base) = self.as_constant() {
            return Some(Polynomial::constant(base.pow(exponent)?));
        }
        if !(0..=MAX_POLYNOMIAL_EXPONENT as i128).contains(&exponent) {
            return None;
        }
        let mut result = Polynomial::constant(Rational::ONE);
        for _ in 0..exponent {
            result = result.mul(self)?;
        }
        Some(result)
    }

    /// E.g. `x² + 6x + 9`, `a² − b²`, `7,5`: highest degree first, then in alphabetical
    /// order of the variables (`a² + 2ab + b²`).
    fn format(&self, decimal_comma: bool) -> String {
        if self.terms.is_empty() {
            return "0".to_owned();
        }
        let degree = |m: &Monomial| m.values().sum::<u32>();
        let mut terms: Vec<_> = self.terms.iter().collect();
        terms.sort_by(|(a, _), (b, _)| degree(b).cmp(&degree(a)).then_with(|| graded_order(a, b)));
        let mut out = String::new();
        for (index, (monomial, coefficient)) in terms.into_iter().enumerate() {
            let negative = coefficient.num < 0;
            match (index, negative) {
                (0, true) => out.push('−'),
                (0, false) => {}
                (_, true) => out.push_str(" − "),
                (_, false) => out.push_str(" + "),
            }
            let magnitude = coefficient.format_abs(decimal_comma);
            if monomial.is_empty() {
                out.push_str(&magnitude);
                continue;
            }
            if magnitude.contains('/') {
                let _ = write!(out, "({magnitude})");
            } else if magnitude != "1" {
                out.push_str(&magnitude);
            }
            for (&name, &exponent) in monomial {
                out.push(name);
                if exponent > 1 {
                    out.push_str(&superscript(exponent));
                }
            }
        }
        out
    }
}

/// Order within the same degree: the monomial with the higher power of the alphabetically
/// first variable comes first (`a²`, `ab`, `b²`).
fn graded_order(a: &Monomial, b: &Monomial) -> Ordering {
    let names: std::collections::BTreeSet<char> = a.keys().chain(b.keys()).copied().collect();
    for name in names {
        let ea = a.get(&name).copied().unwrap_or(0);
        let eb = b.get(&name).copied().unwrap_or(0);
        match eb.cmp(&ea) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

const SUPERSCRIPT_DIGITS: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];

fn superscript(n: u32) -> String {
    n.to_string()
        .chars()
        .map(|d| SUPERSCRIPT_DIGITS[d.to_digit(10).unwrap_or(0) as usize])
        .collect()
}

// --- Reading the expression ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(Rational),
    Variable(char),
    Plus,
    Minus,
    Times,
    /// `/` for a fraction, `:` for a division sign (`:` and `÷`)
    Divide(char),
    Power,
    Open,
    Close,
}

fn tokenize(input: &str) -> Option<Vec<Token>> {
    let input = input.trim().trim_end_matches('=').trim_end();
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            let (value, next) = read_number(&chars, i)?;
            tokens.push(Token::Number(value));
            i = next;
            continue;
        }
        if superscript_value(c).is_some() || c == '⁻' {
            // A run of superscript characters is the exponent: x²³ = x^23, 2⁻¹ = 2^(-1)
            let negative = c == '⁻';
            let start = if negative { i + 1 } else { i };
            let mut end = start;
            let mut value: i128 = 0;
            while let Some(digit) = chars.get(end).and_then(|&d| superscript_value(d)) {
                value = value.checked_mul(10)?.checked_add(digit)?;
                end += 1;
            }
            if end == start {
                return None;
            }
            tokens.push(Token::Power);
            if negative {
                tokens.push(Token::Minus);
            }
            tokens.push(Token::Number(Rational::integer(value)));
            i = end;
            continue;
        }
        let token = match c {
            _ if c.is_whitespace() => None,
            '+' => Some(Token::Plus),
            '-' | '−' | '–' => Some(Token::Minus),
            '*' | '·' | '⋅' | '∙' | '×' => Some(Token::Times),
            '/' => Some(Token::Divide('/')),
            ':' | '÷' => Some(Token::Divide(':')),
            '^' => Some(Token::Power),
            '(' | '[' | '{' => Some(Token::Open),
            ')' | ']' | '}' => Some(Token::Close),
            _ if c.is_ascii_alphabetic() => Some(Token::Variable(c)),
            _ => return None,
        };
        tokens.extend(token);
        i += 1;
    }
    Some(tokens)
}

fn superscript_value(c: char) -> Option<i128> {
    SUPERSCRIPT_DIGITS
        .iter()
        .position(|&d| d == c)
        .map(|p| p as i128)
}

/// Reads `12`, `2,5` or `0.125` from `start`; returns the value and the index after it
fn read_number(chars: &[char], start: usize) -> Option<(Rational, usize)> {
    let mut i = start;
    let mut num: i128 = 0;
    let mut den: i128 = 1;
    let mut in_fraction = false;
    while i < chars.len() {
        let c = chars[i];
        if let Some(digit) = c.to_digit(10) {
            num = num.checked_mul(10)?.checked_add(digit as i128)?;
            if in_fraction {
                den = den.checked_mul(10)?;
            }
        } else if (c == ',' || c == '.')
            && !in_fraction
            && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())
        {
            in_fraction = true;
        } else {
            break;
        }
        i += 1;
    }
    Some((Rational::new(num, den)?, i))
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Number(Rational),
    Variable(char),
    Neg(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    /// With the sign it was written with, for [`display`]
    Div(Box<Expr>, Box<Expr>, char),
    Pow(Box<Expr>, Box<Expr>),
}

impl Expr {
    fn eval(&self) -> Option<Polynomial> {
        Some(match self {
            Expr::Number(value) => Polynomial::constant(*value),
            Expr::Variable(name) => Polynomial::variable(*name),
            Expr::Neg(e) => e.eval()?.neg()?,
            Expr::Add(a, b) => a.eval()?.add(&b.eval()?)?,
            Expr::Sub(a, b) => a.eval()?.add(&b.eval()?.neg()?)?,
            Expr::Mul(a, b) => a.eval()?.mul(&b.eval()?)?,
            Expr::Div(a, b, _) => a.eval()?.div(&b.eval()?)?,
            Expr::Pow(a, b) => a.eval()?.pow(&b.eval()?)?,
        })
    }
}

impl Expr {
    /// Binding strength for deciding on parentheses: sums < products < signs < powers < atoms
    fn precedence(&self) -> u8 {
        match self {
            Expr::Add(..) | Expr::Sub(..) => 1,
            Expr::Mul(..) | Expr::Div(..) => 2,
            Expr::Neg(_) => 3,
            Expr::Pow(..) => 4,
            Expr::Number(_) | Expr::Variable(_) => 5,
        }
    }

    /// The text of this expression, in parentheses if it binds weaker than `min`
    fn display_at(&self, min: u8, decimal_comma: bool) -> String {
        let text = self.display(decimal_comma);
        if self.precedence() < min {
            format!("({text})")
        } else {
            text
        }
    }

    fn display(&self, decimal_comma: bool) -> String {
        let dc = decimal_comma;
        match self {
            Expr::Number(value) => value.format_abs(dc),
            Expr::Variable(name) => name.to_string(),
            Expr::Neg(e) => format!("−{}", e.display_at(3, dc)),
            Expr::Add(a, b) => format!("{} + {}", a.display_at(1, dc), b.display_at(1, dc)),
            Expr::Sub(a, b) => format!("{} − {}", a.display_at(1, dc), b.display_at(2, dc)),
            Expr::Mul(a, b) => {
                let left = a.display_at(2, dc);
                let right = b.display_at(3, dc);
                // 2x, 3(x + 1), x², but 2 · 3 and x · 2
                let implicit = !matches!(**a, Expr::Neg(_))
                    && (matches!(**b, Expr::Variable(_))
                        || matches!(&**b, Expr::Pow(base, _) if matches!(**base, Expr::Variable(_)))
                        || right.starts_with('('));
                if implicit {
                    format!("{left}{right}")
                } else {
                    format!("{left} · {right}")
                }
            }
            Expr::Div(a, b, sign) => match sign {
                '/' => format!("{}/{}", a.display_at(4, dc), b.display_at(4, dc)),
                _ => format!("{} : {}", a.display_at(2, dc), b.display_at(3, dc)),
            },
            Expr::Pow(base, exponent) => {
                let base = base.display_at(5, dc);
                let integer = |e: &Expr| match e {
                    Expr::Number(v) => v.as_integer().map(|n| superscript(n as u32)),
                    _ => None,
                };
                let small = match &**exponent {
                    Expr::Neg(e) => integer(e).map(|s| format!("⁻{s}")),
                    e => integer(e),
                };
                match small {
                    Some(small) => format!("{base}{small}"),
                    None => format!("{base}^({})", exponent.display(dc)),
                }
            }
        }
    }
}

/// Recursive descent with the usual precedence: `+ −` < `· :` and implicit
/// multiplication < unary minus < `^` (right-associative, so `−2² = −4` and `2^3^2 = 2^9`).
struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn parse(mut self) -> Option<Expr> {
        let expr = self.sum()?;
        (self.pos == self.tokens.len()).then_some(expr)
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, token: &Token) -> bool {
        let found = self.peek() == Some(token);
        if found {
            self.pos += 1;
        }
        found
    }

    fn sum(&mut self) -> Option<Expr> {
        let mut left = self.product()?;
        loop {
            if self.eat(&Token::Plus) {
                left = Expr::Add(Box::new(left), Box::new(self.product()?));
            } else if self.eat(&Token::Minus) {
                left = Expr::Sub(Box::new(left), Box::new(self.product()?));
            } else {
                return Some(left);
            }
        }
    }

    fn product(&mut self) -> Option<Expr> {
        let mut left = self.unary()?;
        loop {
            if self.eat(&Token::Times) {
                left = Expr::Mul(Box::new(left), Box::new(self.unary()?));
            } else if let Some(&Token::Divide(sign)) = self.peek() {
                self.pos += 1;
                left = Expr::Div(Box::new(left), Box::new(self.unary()?), sign);
            } else if matches!(self.peek(), Some(Token::Variable(_) | Token::Open)) {
                // Implicit multiplication: 2x, 3(x + 1), (a + b)(a − b). Two numbers in a
                // row are not multiplied, they are more likely a misread.
                left = Expr::Mul(Box::new(left), Box::new(self.power()?));
            } else {
                return Some(left);
            }
        }
    }

    fn unary(&mut self) -> Option<Expr> {
        if self.eat(&Token::Minus) {
            return Some(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat(&Token::Plus) {
            return self.unary();
        }
        self.power()
    }

    fn power(&mut self) -> Option<Expr> {
        let base = self.atom()?;
        if self.eat(&Token::Power) {
            return Some(Expr::Pow(Box::new(base), Box::new(self.unary()?)));
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<Expr> {
        let token = self.peek()?.clone();
        self.pos += 1;
        match token {
            Token::Number(value) => Some(Expr::Number(value)),
            Token::Variable(name) => Some(Expr::Variable(name)),
            Token::Open => {
                let inner = self.sum()?;
                self.eat(&Token::Close).then_some(inner)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn de(expression: &str) -> Option<String> {
        solve(expression, true)
    }

    #[test]
    fn arithmetic() {
        assert_eq!(de("2^4 ="), Some("16".into()));
        assert_eq!(de("2⁴="), Some("16".into()));
        assert_eq!(de("1 + 2 · 3"), Some("7".into()));
        assert_eq!(de("(1 + 2) × 3"), Some("9".into()));
        assert_eq!(de("7 − 10"), Some("−3".into()));
        assert_eq!(de("−2^2"), Some("−4".into()));
        assert_eq!(de("(−2)²"), Some("4".into()));
        assert_eq!(de("2^3^2"), Some("512".into()));
        assert_eq!(de("10 : 4"), Some("2,5".into()));
        assert_eq!(de("10 ÷ 4"), Some("2,5".into()));
        assert_eq!(de("3 * [2 + 1]"), Some("9".into()));
    }

    #[test]
    fn decimals_and_fractions() {
        assert_eq!(de("2,5 · 3"), Some("7,5".into()));
        assert_eq!(solve("2.5 * 3", false), Some("7.5".into()));
        assert_eq!(de("0,1 + 0,2"), Some("0,3".into()));
        assert_eq!(de("1 : 3"), Some("1/3".into()));
        assert_eq!(de("1 : 3 · 3"), Some("1".into()));
        assert_eq!(de("2^-2"), Some("0,25".into()));
        assert_eq!(de("2⁻¹"), Some("0,5".into()));
        assert_eq!(de("1 : 8"), Some("0,125".into()));
    }

    #[test]
    fn polynomials() {
        assert_eq!(de("(x + 3)^2 ="), Some("x² + 6x + 9".into()));
        assert_eq!(de("(x+3)²"), Some("x² + 6x + 9".into()));
        assert_eq!(de("(a + b)²"), Some("a² + 2ab + b²".into()));
        assert_eq!(de("(a + b)(a − b)"), Some("a² − b²".into()));
        assert_eq!(de("(a − b)²"), Some("a² − 2ab + b²".into()));
        assert_eq!(de("2x + 3x"), Some("5x".into()));
        assert_eq!(de("x · x"), Some("x²".into()));
        assert_eq!(de("3(x + 1)"), Some("3x + 3".into()));
        assert_eq!(de("(x + 1)^3"), Some("x³ + 3x² + 3x + 1".into()));
        assert_eq!(de("(2x − 1)²"), Some("4x² − 4x + 1".into()));
        assert_eq!(de("x − x"), Some("0".into()));
        assert_eq!(de("(x + 1) : 2"), Some("0,5x + 0,5".into()));
        assert_eq!(de("x : 3"), Some("(1/3)x".into()));
        assert_eq!(de("−(x + 1)"), Some("−x − 1".into()));
        assert_eq!(
            de("(x+1)^10").map(|s| s.starts_with("x¹⁰ + 10x⁹")),
            Some(true)
        );
    }

    #[test]
    fn nothing_to_compute() {
        assert_eq!(de("5"), None);
        assert_eq!(de("x ="), None);
        assert_eq!(de("2x"), None);
        assert_eq!(de("x² + 1"), None);
        assert_eq!(de("2,5"), None);
    }

    #[test]
    fn unreadable_or_not_computable() {
        assert_eq!(de(""), None);
        assert_eq!(de("="), None);
        assert_eq!(de("1 +"), None);
        assert_eq!(de("(1 + 2"), None);
        assert_eq!(de("1 + 2)"), None);
        assert_eq!(de("2 3"), None);
        assert_eq!(de("1 : 0"), None);
        assert_eq!(de("0^-1"), None);
        assert_eq!(de("1 : x"), None);
        assert_eq!(de("x^y"), None);
        assert_eq!(de("x^0,5"), None);
        assert_eq!(de("2 ? 3"), None);
        assert_eq!(de("1,2,3"), None);
    }

    #[test]
    fn displays_expressions() {
        let show = |e: &str| display(e, true).unwrap();
        assert_eq!(show("2^(4)"), "2⁴");
        assert_eq!(show("(x+3)^(2)"), "(x + 3)²");
        assert_eq!(show("2,5·3"), "2,5 · 3");
        assert_eq!(show("9:3"), "9 : 3");
        assert_eq!(show("((x+1)/(2))"), "(x + 1)/2");
        assert_eq!(show("((1)/(2))+((1)/(2·3))"), "1/2 + 1/(2 · 3)");
        assert_eq!(show("2^(-1)"), "2⁻¹");
        assert_eq!(show("2^(x+1)"), "2^(x + 1)");
        assert_eq!(show("3(x+1)"), "3(x + 1)");
        assert_eq!(show("2x^(2)"), "2x²");
        assert_eq!(show("x·2"), "x · 2");
        assert_eq!(show("(a+b)(a-b)"), "(a + b)(a − b)");
        assert_eq!(show("7-(5-2)"), "7 − (5 − 2)");
        assert_eq!(show("-2^(2)"), "−2²");
        assert_eq!(show("(-2)^(2)"), "(−2)²");
        assert_eq!(show("0.5"), "0,5");
        assert!(display("1 +", true).is_none());
    }

    #[test]
    fn overflow_and_limits() {
        assert_eq!(de("10^100"), None);
        assert_eq!(de("2^100000"), None);
        assert_eq!(de("(x + 1)^33"), None);
        assert_eq!(de("(a+b+c+d+e)^8"), None);
        assert_eq!(de("99999999999999999999999999999999999999999999 + 1"), None);
        assert_eq!(de("2^64"), Some("18446744073709551616".into()));
    }
}

//! Calculator: a small recursive-descent expression evaluator.
//!
//! Supports `+ - * / % ^`, parentheses, unary minus, the constants `pi` and
//! `e`, and the functions sqrt, abs, sin, cos, tan, asin, acos, atan, ln,
//! log (base 10), log2, exp, floor, ceil and round. Angles are in radians.
//! Input that is not clearly arithmetic (a bare number, plain words) is not
//! treated as an expression.

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CalcError {
    #[error("unexpected input at position {0}")]
    Unexpected(usize),
    #[error("unknown name '{0}'")]
    UnknownName(String),
    #[error("expression is nested too deeply")]
    TooDeep,
    #[error("result is not a finite number")]
    NotFinite,
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&mut self) -> Option<u8> {
        while self.src.get(self.pos).is_some_and(u8::is_ascii_whitespace) {
            self.pos += 1;
        }
        self.src.get(self.pos).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn enter(&mut self) -> Result<(), CalcError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err(CalcError::TooDeep)
        } else {
            Ok(())
        }
    }

    fn expr(&mut self) -> Result<f64, CalcError> {
        self.enter()?;
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                break;
            }
        }
        self.depth -= 1;
        Ok(v)
    }

    fn term(&mut self) -> Result<f64, CalcError> {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                v /= self.unary()?;
            } else if self.eat(b'%') {
                v %= self.unary()?;
            } else {
                break;
            }
        }
        Ok(v)
    }

    fn unary(&mut self) -> Result<f64, CalcError> {
        self.enter()?;
        let v = if self.eat(b'-') {
            -self.unary()?
        } else if self.eat(b'+') {
            self.unary()?
        } else {
            self.power()?
        };
        self.depth -= 1;
        Ok(v)
    }

    fn power(&mut self) -> Result<f64, CalcError> {
        let base = self.atom()?;
        if self.eat(b'^') {
            // Right associative, and binds tighter than unary minus on the left.
            let exp = self.unary()?;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<f64, CalcError> {
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                let v = self.expr()?;
                if !self.eat(b')') {
                    return Err(CalcError::Unexpected(self.pos));
                }
                Ok(v)
            }
            Some(b) if b.is_ascii_digit() || b == b'.' => self.number(),
            Some(b) if b.is_ascii_alphabetic() => self.name(),
            _ => Err(CalcError::Unexpected(self.pos)),
        }
    }

    fn number(&mut self) -> Result<f64, CalcError> {
        let start = self.pos;
        while self
            .src
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_digit() || *b == b'.')
        {
            self.pos += 1;
        }
        // Exponent only when digits follow, so `2e` stays an error and `2e3` works.
        if matches!(self.src.get(self.pos), Some(b'e' | b'E')) {
            let mut p = self.pos + 1;
            if matches!(self.src.get(p), Some(b'+' | b'-')) {
                p += 1;
            }
            if self.src.get(p).is_some_and(u8::is_ascii_digit) {
                while self.src.get(p).is_some_and(u8::is_ascii_digit) {
                    p += 1;
                }
                self.pos = p;
            }
        }
        std::str::from_utf8(&self.src[start..self.pos])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(CalcError::Unexpected(start))
    }

    fn name(&mut self) -> Result<f64, CalcError> {
        let start = self.pos;
        while self
            .src
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric())
        {
            self.pos += 1;
        }
        let name = String::from_utf8_lossy(&self.src[start..self.pos]).to_lowercase();
        if self.peek() == Some(b'(') {
            self.pos += 1;
            let arg = self.expr()?;
            if !self.eat(b')') {
                return Err(CalcError::Unexpected(self.pos));
            }
            return apply(&name, arg).ok_or(CalcError::UnknownName(name));
        }
        match name.as_str() {
            "pi" => Ok(std::f64::consts::PI),
            "e" => Ok(std::f64::consts::E),
            _ => Err(CalcError::UnknownName(name)),
        }
    }
}

fn apply(name: &str, x: f64) -> Option<f64> {
    Some(match name {
        "sqrt" => x.sqrt(),
        "abs" => x.abs(),
        "sin" => x.sin(),
        "cos" => x.cos(),
        "tan" => x.tan(),
        "asin" => x.asin(),
        "acos" => x.acos(),
        "atan" => x.atan(),
        "ln" => x.ln(),
        "log" => x.log10(),
        "log2" => x.log2(),
        "exp" => x.exp(),
        "floor" => x.floor(),
        "ceil" => x.ceil(),
        "round" => x.round(),
        _ => return None,
    })
}

pub fn evaluate(input: &str) -> Result<f64, CalcError> {
    let mut p = Parser {
        src: input.as_bytes(),
        pos: 0,
        depth: 0,
    };
    let v = p.expr()?;
    if p.peek().is_some() {
        return Err(CalcError::Unexpected(p.pos));
    }
    if v.is_finite() {
        Ok(v)
    } else {
        Err(CalcError::NotFinite)
    }
}

/// Evaluates `input` if it looks like arithmetic (contains an operator or a
/// parenthesis, and parses completely). A bare number or word is not a calculation.
pub fn try_evaluate(input: &str) -> Option<f64> {
    let trimmed = input.trim();
    if !trimmed
        .bytes()
        .any(|b| matches!(b, b'+' | b'-' | b'*' | b'/' | b'^' | b'%' | b'('))
    {
        return None;
    }
    // "-5" or "+5" alone is a signed number, not a calculation.
    if let Some(rest) = trimmed.strip_prefix(['-', '+']) {
        if rest.trim().parse::<f64>().is_ok() {
            return None;
        }
    }
    evaluate(trimmed).ok()
}

/// Up to 12 significant digits, trailing zeros trimmed, scientific for extremes.
pub fn format_number(v: f64) -> String {
    format_significant(v, 12)
}

pub fn format_significant(v: f64, digits: i32) -> String {
    if v == 0.0 {
        return "0".to_owned();
    }
    let mag = v.abs().log10().floor() as i32;
    if !(-6..15).contains(&mag) {
        return format!("{:.*e}", (digits - 1).max(0) as usize, v)
            .split_once('e')
            .map(|(m, e)| {
                let m = if m.contains('.') {
                    m.trim_end_matches('0').trim_end_matches('.')
                } else {
                    m
                };
                format!("{m}e{e}")
            })
            .unwrap_or_default();
    }
    let decimals = (digits - 1 - mag).clamp(0, 15) as usize;
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(s: &str) -> f64 {
        evaluate(s).unwrap()
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(eval("2+2"), 4.0);
        assert_eq!(eval("2+3*4"), 14.0);
        assert_eq!(eval("(2+3)*4"), 20.0);
        assert_eq!(eval("10/4"), 2.5);
        assert_eq!(eval("10 % 4"), 2.0);
        assert_eq!(eval("2^3^2"), 512.0);
        assert_eq!(eval("-2^2"), -4.0);
        assert_eq!(eval("2*-3"), -6.0);
        assert_eq!(eval("1.5e3 + 1"), 1501.0);
    }

    #[test]
    fn functions_and_constants() {
        assert_eq!(eval("sqrt(16)"), 4.0);
        assert!((eval("2*pi") - std::f64::consts::TAU).abs() < 1e-12);
        assert!((eval("ln(e)") - 1.0).abs() < 1e-12);
        assert_eq!(eval("log(1000)"), 3.0);
        assert_eq!(eval("round(2.6)+floor(1.9)"), 4.0);
    }

    #[test]
    fn errors_are_typed_not_panics() {
        assert!(matches!(evaluate("2+"), Err(CalcError::Unexpected(_))));
        assert!(matches!(evaluate("(1+2"), Err(CalcError::Unexpected(_))));
        assert!(matches!(evaluate("foo(1)"), Err(CalcError::UnknownName(_))));
        assert!(matches!(evaluate("1/0"), Err(CalcError::NotFinite)));
        assert!(matches!(evaluate("2e"), Err(CalcError::Unexpected(_))));
        let deep = format!("{}1{}", "(".repeat(500), ")".repeat(500));
        assert!(matches!(evaluate(&deep), Err(CalcError::TooDeep)));
    }

    #[test]
    fn only_arithmetic_looking_input_is_a_calculation() {
        assert_eq!(try_evaluate("2+2"), Some(4.0));
        assert_eq!(try_evaluate("sqrt(16)"), Some(4.0));
        assert_eq!(try_evaluate("42"), None);
        assert_eq!(try_evaluate("-5"), None);
        assert_eq!(try_evaluate("report"), None);
        assert_eq!(try_evaluate("my-file"), None);
        assert_eq!(try_evaluate("2+"), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_number(4.0), "4");
        assert_eq!(format_number(2.5), "2.5");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(1.0 / 3.0), "0.333333333333");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(1e20), "1e20");
        assert_eq!(format_number(1.5e-9), "1.5e-9");
        assert_eq!(format_significant(6.213_711_922, 6), "6.21371");
    }
}

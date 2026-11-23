use std::collections::HashSet;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagExpressionError {
    pub offset: usize,
}

impl fmt::Display for TagExpressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid tag expression at byte {}", self.offset)
    }
}

impl std::error::Error for TagExpressionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Tag(&'a str),
    Open,
    Close,
    Not,
    And,
    Or,
    BitAnd,
    BitOr,
}

/// Evaluate the contents of an FPP tag condition against the enabled tags.
///
/// An absent condition should be handled by the caller as unconditionally true.
/// Both legacy (`-`, `,`, `{}`) and C-style operators are accepted.
pub fn evaluate(expression: &str, enabled: &HashSet<String>) -> Result<bool, TagExpressionError> {
    let mut tokens = Vec::new();
    let mut chars = expression.char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        let token = match ch {
            '(' | '{' => Token::Open,
            ')' | '}' => Token::Close,
            '!' => Token::Not,
            '-' => Token::And,
            ',' => Token::Or,
            '&' => Token::BitAnd,
            '|' => Token::BitOr,
            _ => {
                let mut end = offset + ch.len_utf8();
                while let Some(&(i, next)) = chars.peek() {
                    if next.is_whitespace() || "(){}!, -&|".contains(next) {
                        break;
                    }
                    chars.next();
                    end = i + next.len_utf8();
                }
                Token::Tag(&expression[offset..end])
            }
        };
        // The reference translates tokens before evaluating, so even spaced
        // pairs of single-character operators become && or ||.
        if let Some((_, previous)) = tokens.last_mut() {
            if (*previous, token) == (Token::BitAnd, Token::BitAnd) {
                *previous = Token::And;
                continue;
            }
            if (*previous, token) == (Token::BitOr, Token::BitOr) {
                *previous = Token::Or;
                continue;
            }
        }
        tokens.push((offset, token));
    }

    let mut parser = Parser { tokens, index: 0, end: expression.len(), enabled };
    let value = parser.expression(0)?;
    if parser.index != parser.tokens.len() {
        return Err(parser.error());
    }
    Ok(value)
}

struct Parser<'a> {
    tokens: Vec<(usize, Token<'a>)>,
    index: usize,
    end: usize,
    enabled: &'a HashSet<String>,
}

impl Parser<'_> {
    fn error(&self) -> TagExpressionError {
        TagExpressionError { offset: self.tokens.get(self.index).map_or(self.end, |t| t.0) }
    }

    fn expression(&mut self, min_precedence: u8) -> Result<bool, TagExpressionError> {
        let mut left = self.atom()?;
        while let Some(&(_, op)) = self.tokens.get(self.index) {
            let precedence = match op {
                Token::Or => 1,
                Token::And => 2,
                Token::BitOr => 3,
                Token::BitAnd => 4,
                _ => break,
            };
            if precedence < min_precedence {
                break;
            }
            self.index += 1;
            let right = self.expression(precedence + 1)?;
            left = match op {
                Token::Or | Token::BitOr => left || right,
                Token::And | Token::BitAnd => left && right,
                _ => unreachable!(),
            };
        }
        Ok(left)
    }

    fn atom(&mut self) -> Result<bool, TagExpressionError> {
        let token = self.tokens.get(self.index).map(|t| t.1).ok_or_else(|| self.error())?;
        self.index += 1;
        match token {
            Token::Tag(tag) => Ok(self.enabled.contains(tag)),
            Token::Not => Ok(!self.atom()?),
            Token::Open => {
                let result = self.expression(0)?;
                if self.tokens.get(self.index).map(|t| t.1) != Some(Token::Close) {
                    return Err(self.error());
                }
                self.index += 1;
                Ok(result)
            }
            _ => {
                self.index -= 1;
                Err(self.error())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> HashSet<String> {
        ["ixbix", "bax", "tag_defined", "x86:64"].into_iter().map(String::from).collect()
    }

    #[test]
    fn upstream_examples_and_unknown_tags() {
        let tags = tags();
        for (expr, expected) in [
            ("ixbix-bax", true),
            ("aba-bax", false),
            ("{!bax,!ixbix}", false),
            ("tag_not_defined && tag_defined", false),
            ("tag_not_defined || tag_defined", true),
            ("x86:64", true),
            ("0", false),
            ("!missing", true),
        ] {
            assert_eq!(evaluate(expr, &tags), Ok(expected), "{expr}");
        }
    }

    #[test]
    fn precedence_grouping_and_whitespace() {
        let tags = tags();
        for (expr, expected) in [
            ("ixbix,missing-bax", true),
            ("{ixbix,missing}-!bax", false),
            ("missing && ixbix | bax", false),
            ("ixbix | missing & missing", true),
            ("! ! ixbix & & bax", true),
            ("{ixbix)", true),
        ] {
            assert_eq!(evaluate(expr, &tags), Ok(expected), "{expr}");
        }
    }

    #[test]
    fn rejects_malformed_even_in_short_circuited_branches() {
        let tags = tags();
        for expr in [
            "",
            " ",
            "()",
            "ixbix ||",
            "missing && (",
            "ixbix bax",
            "ixbix)",
            "&ixbix",
            "ixbix &&& bax",
        ] {
            assert!(evaluate(expr, &tags).is_err(), "{expr}");
        }
    }
}

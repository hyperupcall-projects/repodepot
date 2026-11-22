//! Tag queries: `rust AND work`, `rust OR go`, `work AND NOT archived`, parentheses.
//! Adjacent terms mean AND; `*` matches every clone. Keywords are case-insensitive.

use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    All,
    Tag(String),
    Not(Box<Query>),
    And(Vec<Query>),
    Or(Vec<Query>),
}

impl Query {
    pub fn matches(&self, tags: &BTreeSet<String>) -> bool {
        match self {
            Query::All => true,
            Query::Tag(tag) => tags.contains(tag),
            Query::Not(inner) => !inner.matches(tags),
            Query::And(parts) => parts.iter().all(|p| p.matches(tags)),
            Query::Or(parts) => parts.iter().any(|p| p.matches(tags)),
        }
    }

    /// Every tag name the query mentions.
    pub fn tags(&self) -> Vec<&str> {
        match self {
            Query::All => Vec::new(),
            Query::Tag(tag) => vec![tag],
            Query::Not(inner) => inner.tags(),
            Query::And(parts) | Query::Or(parts) => parts.iter().flat_map(Query::tags).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    And,
    Or,
    Not,
    All,
    Word(String),
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, tokens: &mut Vec<Token>| {
        if word.is_empty() {
            return;
        }
        tokens.push(match word.to_lowercase().as_str() {
            "and" => Token::And,
            "or" => Token::Or,
            "not" => Token::Not,
            "*" => Token::All,
            lower => Token::Word(lower.to_owned()),
        });
        word.clear();
    };
    for c in text.chars() {
        match c {
            '(' | ')' => {
                flush(&mut word, &mut tokens);
                tokens.push(if c == '(' { Token::Open } else { Token::Close });
            }
            c if c.is_whitespace() => flush(&mut word, &mut tokens),
            c => word.push(c),
        }
    }
    flush(&mut word, &mut tokens);
    tokens
}

/// Parses a query. An empty query is an error ("type a tag").
pub fn parse(text: &str) -> Result<Query, String> {
    let tokens = tokenize(text);
    if tokens.is_empty() {
        return Err("Type a tag, or * for every clone.".to_owned());
    }
    let mut parser = Parser { tokens, at: 0 };
    let query = parser.or()?;
    if parser.at < parser.tokens.len() {
        return Err(match &parser.tokens[parser.at] {
            Token::Close => "There's a ) without a matching (.".to_owned(),
            other => format!("Didn't expect {} here.", describe(other)),
        });
    }
    Ok(query)
}

fn describe(token: &Token) -> String {
    match token {
        Token::Open => "(".into(),
        Token::Close => ")".into(),
        Token::And => "AND".into(),
        Token::Or => "OR".into(),
        Token::Not => "NOT".into(),
        Token::All => "*".into(),
        Token::Word(w) => format!("\"{w}\""),
    }
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn or(&mut self) -> Result<Query, String> {
        let mut parts = vec![self.and()?];
        while self.peek() == Some(&Token::Or) {
            self.at += 1;
            parts.push(self.and()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Query::Or(parts)
        })
    }

    fn and(&mut self) -> Result<Query, String> {
        let mut parts = vec![self.not()?];
        loop {
            match self.peek() {
                Some(Token::And) => {
                    self.at += 1;
                    parts.push(self.not()?);
                }
                // Two terms side by side mean AND.
                Some(Token::Word(_) | Token::Not | Token::Open | Token::All) => {
                    parts.push(self.not()?);
                }
                _ => break,
            }
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Query::And(parts)
        })
    }

    fn not(&mut self) -> Result<Query, String> {
        if self.peek() == Some(&Token::Not) {
            self.at += 1;
            return Ok(Query::Not(Box::new(self.not()?)));
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Query, String> {
        let token = self.peek().cloned();
        self.at += 1;
        match token {
            Some(Token::Word(word)) => Ok(Query::Tag(word)),
            Some(Token::All) => Ok(Query::All),
            Some(Token::Open) => {
                let inner = self.or()?;
                if self.peek() != Some(&Token::Close) {
                    return Err("A ( is missing its ).".to_owned());
                }
                self.at += 1;
                Ok(inner)
            }
            Some(other) => Err(format!("Expected a tag before {}.", describe(&other))),
            None => Err("The query ends too early; it needs a tag at the end.".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn matches(query: &str, list: &[&str]) -> bool {
        parse(query).unwrap().matches(&tags(list))
    }

    #[test]
    fn operators_and_precedence() {
        assert!(matches("rust", &["rust", "work"]));
        assert!(matches("rust AND work", &["rust", "work"]));
        assert!(!matches("rust AND work", &["rust"]));
        assert!(matches("rust work", &["rust", "work"]), "implicit AND");
        assert!(matches("rust OR go", &["go"]));
        assert!(matches("work AND NOT archived", &["work"]));
        assert!(!matches("work AND NOT archived", &["work", "archived"]));
        // AND binds tighter than OR.
        assert!(matches("a OR b AND c", &["a"]));
        assert!(!matches("(a OR b) AND c", &["a"]));
        assert!(matches("not not x", &["x"]));
        assert!(matches("*", &[]));
        assert!(matches("* NOT archived", &["rust"]));
        assert!(matches("RUST", &["rust"]), "tags are lowercase");
    }

    #[test]
    fn mentioned_tags() {
        let query = parse("(rust OR go) AND NOT lang:c").unwrap();
        assert_eq!(query.tags(), ["rust", "go", "lang:c"]);
    }

    #[test]
    fn errors_say_what_is_wrong() {
        assert!(parse("").unwrap_err().contains("Type a tag"));
        assert!(parse("(rust").unwrap_err().contains("missing its )"));
        assert!(parse("rust)").unwrap_err().contains("without a matching ("));
        assert!(parse("rust AND").unwrap_err().contains("ends too early"));
        assert!(
            parse("OR rust")
                .unwrap_err()
                .contains("Expected a tag before OR")
        );
    }
}

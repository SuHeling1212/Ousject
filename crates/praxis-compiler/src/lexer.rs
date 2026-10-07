use crate::CompileError;
use tf_format::FloatValue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lexeme {
    Identifier(String),
    Integer(i64),
    Float(FloatValue),
    String(String),
    True,
    False,
    Null,
    If,
    Else,
    While,
    Break,
    Continue,
    Func,
    Return,
    Class,
    Extends,
    Public,
    Private,
    Try,
    Catch,
    Transaction,
    Link,
    New,
    And,
    Or,
    NotWord,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Equal,
    EqualEqual,
    Bang,
    BangEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    PlusPlus,
    MinusMinus,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Comma,
    Colon,
    Hash,
    Semicolon,
    End,
}

#[derive(Debug, Clone)]
pub(crate) struct Spanned {
    pub(crate) lexeme: Lexeme,
    pub(crate) position: usize,
}

pub(crate) struct Lexer<'a> {
    source: &'a [u8],
    position: usize,
    lexemes: Vec<Spanned>,
}

impl<'a> Lexer<'a> {
    pub(crate) const fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            position: 0,
            lexemes: Vec::new(),
        }
    }

    pub(crate) fn scan(mut self) -> Result<Vec<Spanned>, CompileError> {
        while let Some(byte) = self.peek() {
            match byte {
                b' ' | b'\t' | b'\r' => self.position += 1,
                b'\n' | b';' => {
                    self.push(Lexeme::Semicolon, self.position);
                    self.position += 1;
                }
                b'/' if self.peek_next() == Some(b'/') => self.skip_comment(),
                b'0'..=b'9' => self.number()?,
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.identifier(),
                b'"' => self.string()?,
                b'+' => self.simple_pair(b'+', Lexeme::PlusPlus, Lexeme::Plus),
                b'-' => self.simple_pair(b'-', Lexeme::MinusMinus, Lexeme::Minus),
                b'=' => self.simple_pair(b'=', Lexeme::EqualEqual, Lexeme::Equal),
                b'!' => self.simple_pair(b'=', Lexeme::BangEqual, Lexeme::Bang),
                b'<' => self.simple_pair(b'=', Lexeme::LessEqual, Lexeme::Less),
                b'>' => self.simple_pair(b'=', Lexeme::GreaterEqual, Lexeme::Greater),
                b'*' => self.single(Lexeme::Star),
                b'/' => self.single(Lexeme::Slash),
                b'%' => self.single(Lexeme::Percent),
                b'&' if self.peek_next() == Some(b'&') => {
                    self.simple_pair(b'&', Lexeme::And, Lexeme::And);
                }
                b'|' if self.peek_next() == Some(b'|') => {
                    self.simple_pair(b'|', Lexeme::Or, Lexeme::Or);
                }
                b'(' => self.single(Lexeme::LeftParen),
                b')' => self.single(Lexeme::RightParen),
                b'{' => self.single(Lexeme::LeftBrace),
                b'}' => self.single(Lexeme::RightBrace),
                b'[' => self.single(Lexeme::LeftBracket),
                b']' => self.single(Lexeme::RightBracket),
                b',' => self.single(Lexeme::Comma),
                b':' => self.single(Lexeme::Colon),
                b'#' => self.single(Lexeme::Hash),
                _ => return Err(self.error("unexpected character")),
            }
        }
        self.push(Lexeme::End, self.position);
        Ok(self.lexemes)
    }

    fn peek(&self) -> Option<u8> {
        self.source.get(self.position).copied()
    }

    fn peek_next(&self) -> Option<u8> {
        self.source.get(self.position + 1).copied()
    }

    fn push(&mut self, lexeme: Lexeme, position: usize) {
        self.lexemes.push(Spanned { lexeme, position });
    }

    fn single(&mut self, lexeme: Lexeme) {
        let position = self.position;
        self.position += 1;
        self.push(lexeme, position);
    }

    fn simple_pair(&mut self, second: u8, pair: Lexeme, single: Lexeme) {
        let position = self.position;
        self.position += 1;
        if self.peek() == Some(second) {
            self.position += 1;
            self.push(pair, position);
        } else {
            self.push(single, position);
        }
    }

    fn skip_comment(&mut self) {
        while self.peek().is_some_and(|byte| byte != b'\n') {
            self.position += 1;
        }
    }

    fn number(&mut self) -> Result<(), CompileError> {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        let text = std::str::from_utf8(&self.source[start..self.position])
            .map_err(|_| self.error("invalid number"))?;
        if self.peek() == Some(b'.')
            && self
                .source
                .get(self.position + 1)
                .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.position += 1;
            }
            let text = std::str::from_utf8(&self.source[start..self.position])
                .map_err(|_| self.error("invalid float"))?;
            let value = text
                .parse::<f64>()
                .map_err(|_| self.error("float literal is out of range"))?;
            self.push(Lexeme::Float(FloatValue::new(value)), start);
        } else {
            let value = text
                .parse::<i64>()
                .map_err(|_| self.error("integer literal is out of range"))?;
            self.push(Lexeme::Integer(value), start);
        }
        Ok(())
    }

    fn identifier(&mut self) {
        let start = self.position;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
        {
            self.position += 1;
        }
        let text = std::str::from_utf8(&self.source[start..self.position]).unwrap_or_default();
        let lexeme = match text {
            "true" => Lexeme::True,
            "false" => Lexeme::False,
            "null" => Lexeme::Null,
            "if" => Lexeme::If,
            "else" => Lexeme::Else,
            "while" => Lexeme::While,
            "break" => Lexeme::Break,
            "continue" => Lexeme::Continue,
            "func" => Lexeme::Func,
            "return" => Lexeme::Return,
            "class" => Lexeme::Class,
            "extends" => Lexeme::Extends,
            "public" => Lexeme::Public,
            "private" => Lexeme::Private,
            "try" => Lexeme::Try,
            "catch" => Lexeme::Catch,
            "transaction" => Lexeme::Transaction,
            "link" => Lexeme::Link,
            "new" => Lexeme::New,
            "and" => Lexeme::And,
            "or" => Lexeme::Or,
            "not" => Lexeme::NotWord,
            _ => Lexeme::Identifier(text.to_owned()),
        };
        self.push(lexeme, start);
    }

    fn string(&mut self) -> Result<(), CompileError> {
        let start = self.position;
        self.position += 1;
        let mut value = String::new();
        while let Some(byte) = self.peek() {
            self.position += 1;
            match byte {
                b'"' => {
                    self.push(Lexeme::String(value), start);
                    return Ok(());
                }
                b'\\' => {
                    let escaped = self.peek().ok_or_else(|| self.error("unfinished escape"))?;
                    self.position += 1;
                    value.push(match escaped {
                        b'e' => '\u{1b}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'"' => '"',
                        b'\\' => '\\',
                        _ => return Err(self.error("unsupported string escape")),
                    });
                }
                b'\n' => return Err(self.error("unterminated string")),
                value_byte if value_byte.is_ascii() => value.push(char::from(value_byte)),
                _ => {
                    let character_start = self.position - 1;
                    let remaining = std::str::from_utf8(&self.source[character_start..])
                        .map_err(|_| self.error("source is not valid UTF-8"))?;
                    let character = remaining
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("invalid UTF-8 character"))?;
                    self.position = character_start + character.len_utf8();
                    value.push(character);
                }
            }
        }
        Err(self.error("unterminated string"))
    }

    fn error(&self, message: &str) -> CompileError {
        CompileError {
            position: self.position,
            message: message.to_owned(),
        }
    }
}

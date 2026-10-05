//! Compiler for the executable Praxis subset used by the system MVP.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use tf_format::{FloatValue, Program, Token, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub position: usize,
    pub message: String,
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Praxis error at byte {}: {}",
            self.position, self.message
        )
    }
}

impl std::error::Error for CompileError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Lexeme {
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
struct Spanned {
    lexeme: Lexeme,
    position: usize,
}

/// Compiles Praxis source into a validated TF program.
///
/// The MVP subset supports variables, integer/string/bool/null literals,
/// arithmetic, short-circuit Boolean operators, comparisons, `if`/`else if`,
/// `while`, loop control, postfix increment/decrement and Object capability calls.
///
/// # Errors
///
/// Returns a positioned error for invalid source or a program too large for TF.
pub fn compile(source: &str) -> Result<Program, CompileError> {
    if source.lines().any(|line| parse_directive(line).is_some()) {
        return Err(CompileError {
            position: 0,
            message: "import/include requires compile_with_loader or the ousject CLI".to_owned(),
        });
    }
    compile_expanded(source)
}

/// Compiles Praxis after recursively expanding top-level `import` and `include` directives.
/// `import` loads a module once, while `include` expands it at every occurrence.
///
/// # Errors
///
/// Returns a positioned compiler error or a loader/cycle error at position zero.
pub fn compile_with_loader(
    source: &str,
    mut loader: impl FnMut(&str) -> Result<String, String>,
) -> Result<Program, CompileError> {
    let mut imported = BTreeSet::new();
    let mut stack = Vec::new();
    let expanded = expand_source(source, &mut loader, &mut imported, &mut stack)?;
    compile_expanded(&expanded)
}

fn compile_expanded(source: &str) -> Result<Program, CompileError> {
    let lexemes = Lexer::new(source).scan()?;
    Parser::new(lexemes).compile()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectiveKind {
    Import,
    Include,
}

fn parse_directive(line: &str) -> Option<(DirectiveKind, String)> {
    let trimmed = line.trim();
    let (kind, rest) = if let Some(rest) = trimmed.strip_prefix("import") {
        (DirectiveKind::Import, rest)
    } else if let Some(rest) = trimmed.strip_prefix("include") {
        (DirectiveKind::Include, rest)
    } else {
        return None;
    };
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let quoted = rest.strip_prefix('"')?;
    let end = quoted.find('"')?;
    let path = &quoted[..end];
    let trailing = quoted[end + 1..].trim();
    if path.is_empty()
        || !(trailing.is_empty()
            || trailing == ";"
            || trailing.starts_with("//")
            || trailing
                .strip_prefix(';')
                .is_some_and(|value| value.trim_start().starts_with("//")))
    {
        return None;
    }
    Some((kind, path.to_owned()))
}

fn expand_source(
    source: &str,
    loader: &mut impl FnMut(&str) -> Result<String, String>,
    imported: &mut BTreeSet<String>,
    stack: &mut Vec<String>,
) -> Result<String, CompileError> {
    let mut output = String::new();
    let mut brace_depth = 0_i64;
    for line in source.lines() {
        if brace_depth == 0 {
            if let Some((kind, path)) = parse_directive(line) {
                if kind == DirectiveKind::Import && !imported.insert(path.clone()) {
                    output.push('\n');
                    continue;
                }
                if stack.contains(&path) {
                    return Err(CompileError {
                        position: 0,
                        message: format!("cyclic import/include involving '{path}'"),
                    });
                }
                stack.push(path.clone());
                let module_source = loader(&path).map_err(|message| CompileError {
                    position: 0,
                    message: format!("cannot load '{path}': {message}"),
                })?;
                let expanded = expand_source(&module_source, loader, imported, stack)?;
                stack.pop();
                output.push_str(&expanded);
                output.push('\n');
                continue;
            }
        }
        output.push_str(line);
        output.push('\n');
        brace_depth += brace_delta(line);
        if brace_depth < 0 {
            brace_depth = 0;
        }
    }
    Ok(output)
}

fn brace_delta(line: &str) -> i64 {
    let mut delta = 0_i64;
    let mut quoted = false;
    let mut escaped = false;
    let bytes = line.as_bytes();
    let mut position = 0;
    while position < bytes.len() {
        let byte = bytes[position];
        if !quoted && byte == b'/' && bytes.get(position + 1) == Some(&b'/') {
            break;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        } else if byte == b'{' {
            delta += 1;
        } else if byte == b'}' {
            delta -= 1;
        }
        position += 1;
    }
    delta
}

struct Lexer<'a> {
    source: &'a [u8],
    position: usize,
    lexemes: Vec<Spanned>,
}

impl<'a> Lexer<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            position: 0,
            lexemes: Vec::new(),
        }
    }

    fn scan(mut self) -> Result<Vec<Spanned>, CompileError> {
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

struct Parser {
    lexemes: Vec<Spanned>,
    position: usize,
    tokens: Vec<Token>,
    loops: Vec<LoopContext>,
    function_depth: usize,
    transaction_depth: usize,
}

struct LoopContext {
    continue_target: usize,
    break_jumps: Vec<usize>,
}

impl Parser {
    const fn new(lexemes: Vec<Spanned>) -> Self {
        Self {
            lexemes,
            position: 0,
            tokens: Vec::new(),
            loops: Vec::new(),
            function_depth: 0,
            transaction_depth: 0,
        }
    }

    fn compile(mut self) -> Result<Program, CompileError> {
        self.statements(false)?;
        self.tokens.push(Token::Halt);
        let source_position = self.current().position;
        let program = Program {
            tokens: self.tokens,
        };
        program.validate().map_err(|error| CompileError {
            position: source_position,
            message: error.to_string(),
        })?;
        Ok(program)
    }

    fn statements(&mut self, in_block: bool) -> Result<(), CompileError> {
        self.skip_semicolons();
        while !matches!(self.current().lexeme, Lexeme::End | Lexeme::RightBrace) {
            self.statement()?;
            self.skip_semicolons();
        }
        if in_block {
            self.expect_simple(&Lexeme::RightBrace, "expected '}'")?;
        }
        Ok(())
    }

    fn statement(&mut self) -> Result<(), CompileError> {
        if self.transaction_depth > 0 && !matches!(self.current().lexeme, Lexeme::Identifier(_)) {
            return Err(self.error(
                "transaction blocks currently contain only assignments, field/index updates or link",
            ));
        }
        match self.current().lexeme.clone() {
            Lexeme::If => self.if_statement(),
            Lexeme::While => self.while_statement(),
            Lexeme::Break => self.break_statement(),
            Lexeme::Continue => self.continue_statement(),
            Lexeme::Func => self.function_declaration(None, true),
            Lexeme::Class => self.class_declaration(true),
            Lexeme::Public if matches!(self.peek_lexeme(1), Some(Lexeme::Class)) => {
                self.advance();
                self.class_declaration(true)
            }
            Lexeme::Private if matches!(self.peek_lexeme(1), Some(Lexeme::Class)) => {
                self.advance();
                self.class_declaration(false)
            }
            Lexeme::Return => self.return_statement(),
            Lexeme::Try => self.try_statement(),
            Lexeme::Transaction => self.transaction_statement(),
            Lexeme::New => Err(self.error("new was removed; use object.create(type, value)")),
            Lexeme::Identifier(name)
                if matches!(
                    self.lexemes.get(self.position + 1).map(|item| &item.lexeme),
                    Some(Lexeme::LeftParen)
                ) =>
            {
                self.advance();
                self.call_expression(&name)?;
                self.tokens.push(Token::Pop);
                self.expect_terminator()
            }
            Lexeme::Identifier(name) => self.assignment_statement(name),
            _ => Err(self.error("expected a statement")),
        }
    }

    fn assignment_statement(&mut self, name: String) -> Result<(), CompileError> {
        self.advance();
        if matches!(self.current().lexeme, Lexeme::Link) {
            self.advance();
            let Lexeme::Identifier(target) = self.current().lexeme.clone() else {
                return Err(self.error("expected target name after link"));
            };
            self.advance();
            self.tokens.push(Token::BindLink { name, target });
            return self.expect_terminator();
        }
        if let Some((receiver, field)) = split_field(&name) {
            return self.field_assignment_statement(receiver, field);
        }
        match self.current().lexeme {
            Lexeme::Equal => {
                self.advance();
                self.expression()?;
                if let Some(Token::RegistryCall { method, arguments }) = self.tokens.last() {
                    let method = method.clone();
                    let arguments = *arguments;
                    if method == "create" {
                        self.tokens.pop();
                        self.tokens.push(Token::BindCreated { name, arguments });
                    } else if method == "find" {
                        self.tokens.pop();
                        self.tokens.push(Token::BindFound { name, arguments });
                    } else {
                        self.tokens.push(Token::Store(name));
                    }
                } else {
                    self.tokens.push(Token::Store(name));
                }
            }
            Lexeme::PlusPlus => {
                self.advance();
                self.tokens.push(Token::Load(name.clone()));
                self.tokens.push(Token::Push(Value::Integer(1)));
                self.tokens.push(Token::Add);
                self.tokens.push(Token::Store(name));
            }
            Lexeme::MinusMinus => {
                self.advance();
                self.tokens.push(Token::Load(name.clone()));
                self.tokens.push(Token::Push(Value::Integer(1)));
                self.tokens.push(Token::Subtract);
                self.tokens.push(Token::Store(name));
            }
            Lexeme::LeftBracket => {
                self.advance();
                self.tokens.push(Token::Load(name.clone()));
                self.expression()?;
                self.expect_simple(&Lexeme::RightBracket, "expected ']' after index")?;
                match self.current().lexeme {
                    Lexeme::Equal => {
                        self.advance();
                        self.expression()?;
                        self.tokens.push(Token::IndexSet);
                    }
                    Lexeme::PlusPlus => {
                        self.advance();
                        self.tokens.push(Token::IndexIncrement);
                    }
                    Lexeme::MinusMinus => {
                        self.advance();
                        self.tokens.push(Token::IndexDecrement);
                    }
                    _ => {
                        return Err(self.error("expected '=', '++' or '--' after indexed target"));
                    }
                }
                self.tokens.push(Token::Store(name));
            }
            _ => {
                return Err(self
                    .error("expected '=', '++', '--' or indexed assignment after variable name"));
            }
        }
        self.expect_terminator()
    }

    fn field_assignment_statement(
        &mut self,
        receiver: &str,
        field: &str,
    ) -> Result<(), CompileError> {
        match self.current().lexeme {
            Lexeme::Equal => {
                self.advance();
                self.tokens.push(Token::LoadIdentity(receiver.to_owned()));
                self.expression()?;
                self.tokens.push(Token::SetField(field.to_owned()));
            }
            Lexeme::PlusPlus | Lexeme::MinusMinus => {
                let add = matches!(self.current().lexeme, Lexeme::PlusPlus);
                self.advance();
                self.tokens.push(Token::LoadIdentity(receiver.to_owned()));
                self.tokens.push(Token::LoadIdentity(receiver.to_owned()));
                self.tokens.push(Token::GetField(field.to_owned()));
                self.tokens.push(Token::Push(Value::Integer(1)));
                self.tokens
                    .push(if add { Token::Add } else { Token::Subtract });
                self.tokens.push(Token::SetField(field.to_owned()));
            }
            _ => return Err(self.error("expected '=', '++' or '--' after field")),
        }
        self.expect_terminator()
    }

    fn return_statement(&mut self) -> Result<(), CompileError> {
        if self.function_depth == 0 {
            return Err(self.error("return is only valid inside a function or method"));
        }
        self.advance();
        if matches!(
            self.current().lexeme,
            Lexeme::Semicolon | Lexeme::RightBrace | Lexeme::End
        ) {
            self.tokens.push(Token::Push(Value::Null));
        } else {
            self.expression()?;
        }
        self.tokens.push(Token::Return);
        self.expect_terminator()
    }

    fn try_statement(&mut self) -> Result<(), CompileError> {
        self.advance();
        self.expect_simple(&Lexeme::LeftBrace, "expected '{' after try")?;
        let begin = self.tokens.len();
        self.tokens.push(Token::BeginTry {
            catch: 0,
            end: 0,
            error: String::new(),
        });
        self.statements(true)?;
        let leave = self.tokens.len();
        self.tokens.push(Token::EndTry { end: 0 });
        self.skip_semicolons();
        self.expect_simple(&Lexeme::Catch, "expected catch after try block")?;
        self.expect_simple(&Lexeme::LeftParen, "expected '(' after catch")?;
        let Lexeme::Identifier(error) = self.current().lexeme.clone() else {
            return Err(self.error("expected error name in catch"));
        };
        self.advance();
        self.expect_simple(&Lexeme::RightParen, "expected ')' after catch error")?;
        self.expect_simple(&Lexeme::LeftBrace, "expected '{' before catch body")?;
        let catch = self.token_position(self.tokens.len())?;
        self.statements(true)?;
        let end = self.token_position(self.tokens.len())?;
        let Some(Token::BeginTry {
            catch: catch_target,
            end: end_target,
            error: error_name,
        }) = self.tokens.get_mut(begin)
        else {
            return Err(self.error("internal try definition error"));
        };
        *catch_target = catch;
        *end_target = end;
        *error_name = error;
        let Some(Token::EndTry { end: target }) = self.tokens.get_mut(leave) else {
            return Err(self.error("internal try definition error"));
        };
        *target = end;
        Ok(())
    }

    fn transaction_statement(&mut self) -> Result<(), CompileError> {
        if self.transaction_depth != 0 {
            return Err(self.error("nested transaction blocks are not supported"));
        }
        self.advance();
        self.expect_simple(&Lexeme::LeftBrace, "expected '{' after transaction")?;
        let begin = self.tokens.len();
        self.tokens.push(Token::Transaction { end: 0 });
        self.transaction_depth += 1;
        self.statements(true)?;
        self.transaction_depth -= 1;
        self.tokens.push(Token::CommitTransaction);
        let end = self.token_position(self.tokens.len())?;
        let Some(Token::Transaction { end: target }) = self.tokens.get_mut(begin) else {
            return Err(self.error("internal transaction definition error"));
        };
        *target = end;
        Ok(())
    }

    fn function_declaration(
        &mut self,
        class: Option<&str>,
        public: bool,
    ) -> Result<(), CompileError> {
        self.advance();
        let Lexeme::Identifier(name) = self.current().lexeme.clone() else {
            return Err(self.error("expected function name"));
        };
        self.advance();
        let parameters = self.parameter_names()?;
        self.expect_simple(&Lexeme::LeftBrace, "expected '{' before function body")?;
        let definition = self.tokens.len();
        if let Some(class) = class {
            self.tokens.push(Token::DefineMethod {
                class: class.to_owned(),
                name,
                parameters,
                public,
                end: 0,
            });
        } else {
            self.tokens.push(Token::DefineFunction {
                name,
                parameters,
                end: 0,
            });
        }
        self.function_depth += 1;
        self.statements(true)?;
        self.function_depth -= 1;
        self.tokens.push(Token::Push(Value::Null));
        self.tokens.push(Token::Return);
        let end = self.token_position(self.tokens.len())?;
        match self.tokens.get_mut(definition) {
            Some(
                Token::DefineFunction { end: target, .. } | Token::DefineMethod { end: target, .. },
            ) => *target = end,
            _ => return Err(self.error("internal function definition error")),
        }
        Ok(())
    }

    fn class_declaration(&mut self, _public: bool) -> Result<(), CompileError> {
        self.advance();
        let Lexeme::Identifier(name) = self.current().lexeme.clone() else {
            return Err(self.error("expected class name"));
        };
        self.advance();
        let parent = if matches!(self.current().lexeme, Lexeme::Extends) {
            self.advance();
            let Lexeme::Identifier(parent) = self.current().lexeme.clone() else {
                return Err(self.error("expected parent class name"));
            };
            self.advance();
            Some(parent)
        } else {
            None
        };
        self.expect_simple(&Lexeme::LeftBrace, "expected '{' after class name")?;
        let definition = self.tokens.len();
        self.tokens.push(Token::DefineClass {
            name: name.clone(),
            parent,
            fields: BTreeMap::new(),
            private_fields: Vec::new(),
            end: 0,
        });
        self.skip_semicolons();
        while !matches!(self.current().lexeme, Lexeme::RightBrace | Lexeme::End) {
            let public = match self.current().lexeme {
                Lexeme::Public => {
                    self.advance();
                    true
                }
                Lexeme::Private => {
                    self.advance();
                    false
                }
                _ => true,
            };
            if matches!(self.current().lexeme, Lexeme::Func) {
                self.function_declaration(Some(&name), public)?;
            } else {
                let Lexeme::Identifier(field) = self.current().lexeme.clone() else {
                    return Err(self.error("expected field or func in class"));
                };
                if field == "init" {
                    return Err(self
                        .error("init was removed with new; pass initial fields to object.create"));
                }
                self.advance();
                self.expect_simple(&Lexeme::Equal, "expected '=' after field name")?;
                let value = self.constant_value()?;
                let Some(Token::DefineClass {
                    fields,
                    private_fields,
                    ..
                }) = self.tokens.get_mut(definition)
                else {
                    return Err(self.error("internal class definition error"));
                };
                if fields.insert(field.clone(), value).is_some() {
                    return Err(self.error("duplicate class field"));
                }
                if !public {
                    private_fields.push(field);
                }
                self.expect_terminator()?;
            }
            self.skip_semicolons();
        }
        self.expect_simple(&Lexeme::RightBrace, "expected '}' after class")?;
        let end = self.token_position(self.tokens.len())?;
        let Some(Token::DefineClass { end: target, .. }) = self.tokens.get_mut(definition) else {
            return Err(self.error("internal class definition error"));
        };
        *target = end;
        Ok(())
    }

    fn parameter_names(&mut self) -> Result<Vec<String>, CompileError> {
        self.expect_simple(&Lexeme::LeftParen, "expected '('")?;
        let mut parameters = Vec::new();
        if !matches!(self.current().lexeme, Lexeme::RightParen) {
            loop {
                let Lexeme::Identifier(name) = self.current().lexeme.clone() else {
                    return Err(self.error("expected parameter name"));
                };
                self.advance();
                if parameters.contains(&name) {
                    return Err(self.error("duplicate parameter name"));
                }
                parameters.push(name);
                if matches!(self.current().lexeme, Lexeme::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect_simple(&Lexeme::RightParen, "expected ')' after parameters")?;
        Ok(parameters)
    }

    fn constant_value(&mut self) -> Result<Value, CompileError> {
        let lexeme = self.current().lexeme.clone();
        self.advance();
        match lexeme {
            Lexeme::Integer(value) => Ok(Value::Integer(value)),
            Lexeme::Float(value) => Ok(Value::Float(value)),
            Lexeme::String(value) => Ok(Value::Text(value)),
            Lexeme::True => Ok(Value::Bool(true)),
            Lexeme::False => Ok(Value::Bool(false)),
            Lexeme::Null => Ok(Value::Null),
            Lexeme::Minus => match self.constant_value()? {
                Value::Integer(value) => value
                    .checked_neg()
                    .map(Value::Integer)
                    .ok_or_else(|| self.error("integer literal is out of range")),
                Value::Float(value) => Ok(Value::Float(FloatValue::new(-value.get()))),
                _ => Err(self.error("'-' in a class default requires a number")),
            },
            Lexeme::LeftBracket => {
                let mut values = Vec::new();
                self.skip_semicolons();
                while !matches!(self.current().lexeme, Lexeme::RightBracket) {
                    values.push(self.constant_value()?);
                    self.skip_semicolons();
                    if matches!(self.current().lexeme, Lexeme::Comma) {
                        self.advance();
                        self.skip_semicolons();
                    } else {
                        break;
                    }
                }
                self.expect_simple(&Lexeme::RightBracket, "expected ']' in class default")?;
                Ok(Value::Array(values))
            }
            Lexeme::LeftBrace => {
                let mut values = BTreeMap::new();
                self.skip_semicolons();
                while !matches!(self.current().lexeme, Lexeme::RightBrace) {
                    let (Lexeme::Identifier(key) | Lexeme::String(key)) =
                        self.current().lexeme.clone()
                    else {
                        return Err(self.error("expected key in class Map default"));
                    };
                    self.advance();
                    self.expect_simple(&Lexeme::Colon, "expected ':' in class Map default")?;
                    let value = self.constant_value()?;
                    if values.insert(key, value).is_some() {
                        return Err(self.error("duplicate key in class Map default"));
                    }
                    self.skip_semicolons();
                    if matches!(self.current().lexeme, Lexeme::Comma) {
                        self.advance();
                        self.skip_semicolons();
                    } else {
                        break;
                    }
                }
                self.expect_simple(&Lexeme::RightBrace, "expected '}' in class Map default")?;
                Ok(Value::Map(values))
            }
            _ => Err(self.error("class field default must be a literal")),
        }
    }

    fn if_statement(&mut self) -> Result<(), CompileError> {
        self.advance();
        self.expression()?;
        self.expect_simple(&Lexeme::LeftBrace, "expected '{' after if condition")?;
        let false_jump = self.emit_jump_if_false();
        self.statements(true)?;
        self.skip_semicolons();
        if matches!(self.current().lexeme, Lexeme::Else) {
            self.advance();
            let end_jump = self.emit_jump();
            self.patch_jump(false_jump)?;
            if matches!(self.current().lexeme, Lexeme::If) {
                self.if_statement()?;
            } else {
                self.expect_simple(&Lexeme::LeftBrace, "expected '{' or 'if' after else")?;
                self.statements(true)?;
            }
            self.patch_jump(end_jump)?;
        } else {
            self.patch_jump(false_jump)?;
        }
        Ok(())
    }

    fn while_statement(&mut self) -> Result<(), CompileError> {
        self.advance();
        let loop_start = self.tokens.len();
        self.expression()?;
        self.expect_simple(&Lexeme::LeftBrace, "expected '{' after while condition")?;
        let exit_jump = self.emit_jump_if_false();
        self.loops.push(LoopContext {
            continue_target: loop_start,
            break_jumps: Vec::new(),
        });
        self.statements(true)?;
        let loop_context = self
            .loops
            .pop()
            .ok_or_else(|| self.error("internal loop error"))?;
        self.tokens
            .push(Token::Jump(self.token_position(loop_start)?));
        self.patch_jump(exit_jump)?;
        for break_jump in loop_context.break_jumps {
            self.patch_jump(break_jump)?;
        }
        Ok(())
    }

    fn expression(&mut self) -> Result<(), CompileError> {
        self.logical_or()
    }

    fn break_statement(&mut self) -> Result<(), CompileError> {
        self.advance();
        if self.loops.is_empty() {
            return Err(self.error("'break' is only valid inside while"));
        }
        let jump = self.emit_jump();
        self.loops
            .last_mut()
            .ok_or_else(|| CompileError {
                position: 0,
                message: "internal loop error".to_owned(),
            })?
            .break_jumps
            .push(jump);
        self.expect_terminator()
    }

    fn continue_statement(&mut self) -> Result<(), CompileError> {
        self.advance();
        let target = self
            .loops
            .last()
            .map(|context| context.continue_target)
            .ok_or_else(|| self.error("'continue' is only valid inside while"))?;
        self.tokens.push(Token::Jump(self.token_position(target)?));
        self.expect_terminator()
    }

    fn logical_or(&mut self) -> Result<(), CompileError> {
        self.logical_and()?;
        while matches!(self.current().lexeme, Lexeme::Or) {
            self.advance();
            let evaluate_right = self.emit_jump_if_false();
            self.tokens.push(Token::Push(Value::Bool(true)));
            let end_jump = self.emit_jump();
            self.patch_jump(evaluate_right)?;
            self.logical_and()?;
            self.tokens.push(Token::Not);
            self.tokens.push(Token::Not);
            self.patch_jump(end_jump)?;
        }
        Ok(())
    }

    fn logical_and(&mut self) -> Result<(), CompileError> {
        self.comparison()?;
        while matches!(self.current().lexeme, Lexeme::And) {
            self.advance();
            let false_jump = self.emit_jump_if_false();
            self.comparison()?;
            self.tokens.push(Token::Not);
            self.tokens.push(Token::Not);
            let end_jump = self.emit_jump();
            self.patch_jump(false_jump)?;
            self.tokens.push(Token::Push(Value::Bool(false)));
            self.patch_jump(end_jump)?;
        }
        Ok(())
    }

    fn comparison(&mut self) -> Result<(), CompileError> {
        self.term()?;
        while matches!(
            self.current().lexeme,
            Lexeme::EqualEqual
                | Lexeme::BangEqual
                | Lexeme::Less
                | Lexeme::LessEqual
                | Lexeme::Greater
                | Lexeme::GreaterEqual
        ) {
            let operator = self.current().lexeme.clone();
            self.advance();
            self.term()?;
            self.tokens.push(match operator {
                Lexeme::EqualEqual => Token::Equal,
                Lexeme::BangEqual => Token::NotEqual,
                Lexeme::Less => Token::Less,
                Lexeme::LessEqual => Token::LessEqual,
                Lexeme::Greater => Token::Greater,
                Lexeme::GreaterEqual => Token::GreaterEqual,
                _ => unreachable!(),
            });
        }
        Ok(())
    }

    fn term(&mut self) -> Result<(), CompileError> {
        self.factor()?;
        while matches!(self.current().lexeme, Lexeme::Plus | Lexeme::Minus) {
            let add = matches!(self.current().lexeme, Lexeme::Plus);
            self.advance();
            self.factor()?;
            self.tokens
                .push(if add { Token::Add } else { Token::Subtract });
        }
        Ok(())
    }

    fn factor(&mut self) -> Result<(), CompileError> {
        self.unary()?;
        while matches!(
            self.current().lexeme,
            Lexeme::Star | Lexeme::Slash | Lexeme::Percent
        ) {
            let operator = self.current().lexeme.clone();
            self.advance();
            self.unary()?;
            self.tokens.push(match operator {
                Lexeme::Star => Token::Multiply,
                Lexeme::Slash => Token::Divide,
                Lexeme::Percent => Token::Modulo,
                _ => unreachable!(),
            });
        }
        Ok(())
    }

    fn unary(&mut self) -> Result<(), CompileError> {
        match self.current().lexeme {
            Lexeme::Bang | Lexeme::NotWord => {
                self.advance();
                self.unary()?;
                self.tokens.push(Token::Not);
                Ok(())
            }
            Lexeme::Minus => {
                self.advance();
                self.tokens.push(Token::Push(Value::Integer(0)));
                self.unary()?;
                self.tokens.push(Token::Subtract);
                Ok(())
            }
            Lexeme::Hash => {
                self.advance();
                self.unary()?;
                self.tokens.push(Token::Length);
                Ok(())
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<(), CompileError> {
        self.primary()?;
        while matches!(self.current().lexeme, Lexeme::LeftBracket) {
            self.advance();
            self.expression()?;
            self.expect_simple(&Lexeme::RightBracket, "expected ']' after index")?;
            self.tokens.push(Token::IndexGet);
        }
        Ok(())
    }

    fn primary(&mut self) -> Result<(), CompileError> {
        let lexeme = self.current().lexeme.clone();
        self.advance();
        match lexeme {
            Lexeme::Integer(value) => self.tokens.push(Token::Push(Value::Integer(value))),
            Lexeme::Float(value) => self.tokens.push(Token::Push(Value::Float(value))),
            Lexeme::String(value) => self.tokens.push(Token::Push(Value::Text(value))),
            Lexeme::True => self.tokens.push(Token::Push(Value::Bool(true))),
            Lexeme::False => self.tokens.push(Token::Push(Value::Bool(false))),
            Lexeme::Null => self.tokens.push(Token::Push(Value::Null)),
            Lexeme::Identifier(name) => {
                if matches!(self.current().lexeme, Lexeme::LeftParen) {
                    self.call_expression(&name)?;
                } else if let Some((receiver, fields)) = split_fields(&name) {
                    self.tokens.push(Token::LoadIdentity(receiver.to_owned()));
                    for field in fields {
                        self.tokens.push(Token::GetField(field.to_owned()));
                    }
                } else {
                    self.tokens.push(Token::Load(name));
                }
            }
            Lexeme::LeftBracket => self.array_literal()?,
            Lexeme::LeftBrace => self.map_literal()?,
            Lexeme::LeftParen => {
                self.expression()?;
                self.expect_simple(&Lexeme::RightParen, "expected ')'")?;
            }
            _ => return Err(self.error("expected expression")),
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn call_expression(&mut self, name: &str) -> Result<(), CompileError> {
        if self.transaction_depth > 0 {
            return Err(self.error("capability and function calls are not allowed in transaction"));
        }
        if name == "io.println" {
            return Err(self.error("io.println was removed; discover a console Object"));
        }
        let registry = registry_call(name);
        let object_capability = if registry.is_none() {
            name.rsplit_once('.').filter(|(receiver, capability)| {
                !receiver.is_empty()
                    && !capability.is_empty()
                    && !matches!(*receiver, "object" | "objects")
            })
        } else {
            None
        };
        if let Some((receiver, _)) = object_capability {
            self.tokens.push(Token::LoadIdentity(receiver.to_owned()));
        } else if registry.is_none() && name.contains('.') {
            return Err(self.error("invalid capability call"));
        }
        self.expect_simple(&Lexeme::LeftParen, "expected '(' after call name")?;
        let mut arguments = 0_u32;
        if !matches!(self.current().lexeme, Lexeme::RightParen) {
            loop {
                let registry_method = registry.map(|entry| entry.0);
                let capability_method = object_capability.map(|entry| entry.1);
                let identity_argument = if registry_method == Some("create") {
                    arguments == 2
                } else if capability_method == Some("link") {
                    arguments == 1
                } else {
                    false
                };
                if identity_argument {
                    if let Lexeme::Identifier(variable) = self.current().lexeme.clone() {
                        if matches!(
                            self.lexemes.get(self.position + 1).map(|item| &item.lexeme),
                            Some(Lexeme::Comma | Lexeme::RightParen)
                        ) {
                            self.tokens.push(Token::LoadIdentity(variable));
                            self.advance();
                        } else {
                            self.expression()?;
                        }
                    } else {
                        self.expression()?;
                    }
                } else {
                    self.expression()?;
                }
                arguments = arguments
                    .checked_add(1)
                    .ok_or_else(|| self.error("too many call arguments"))?;
                if matches!(self.current().lexeme, Lexeme::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect_simple(&Lexeme::RightParen, "expected ')' after call arguments")?;
        if let Some((method, minimum, maximum)) = registry {
            if !(minimum..=maximum).contains(&arguments) {
                return Err(self.error("wrong number of Registry arguments"));
            }
            self.tokens.push(Token::RegistryCall {
                method: method.to_owned(),
                arguments,
            });
        } else if let Some((_, capability)) = object_capability {
            if name.starts_with("super.") {
                self.tokens.push(Token::SuperCall {
                    method: capability.to_owned(),
                    arguments,
                });
            } else {
                self.tokens.push(Token::ObjectCall {
                    method: capability.to_owned(),
                    arguments,
                });
            }
        } else {
            self.tokens.push(Token::CallFunction {
                name: name.to_owned(),
                arguments,
            });
        }
        Ok(())
    }

    fn array_literal(&mut self) -> Result<(), CompileError> {
        let mut count = 0usize;
        self.skip_semicolons();
        if matches!(self.current().lexeme, Lexeme::RightBracket) {
            self.advance();
            self.tokens.push(Token::MakeArray(0));
            return Ok(());
        }
        loop {
            self.expression()?;
            count = count
                .checked_add(1)
                .ok_or_else(|| self.error("array is too large"))?;
            self.skip_semicolons();
            if matches!(self.current().lexeme, Lexeme::Comma) {
                self.advance();
                self.skip_semicolons();
                if matches!(self.current().lexeme, Lexeme::RightBracket) {
                    self.advance();
                    break;
                }
            } else {
                self.expect_simple(&Lexeme::RightBracket, "expected ',' or ']' in array")?;
                break;
            }
        }
        self.tokens.push(Token::MakeArray(self.item_count(count)?));
        Ok(())
    }

    fn map_literal(&mut self) -> Result<(), CompileError> {
        let mut count = 0usize;
        self.skip_semicolons();
        if matches!(self.current().lexeme, Lexeme::RightBrace) {
            self.advance();
            self.tokens.push(Token::MakeMap(0));
            return Ok(());
        }
        loop {
            let (Lexeme::Identifier(key) | Lexeme::String(key)) = self.current().lexeme.clone()
            else {
                return Err(self.error("expected text key in map"));
            };
            self.advance();
            self.expect_simple(&Lexeme::Colon, "expected ':' after map key")?;
            self.tokens.push(Token::Push(Value::Text(key)));
            self.expression()?;
            count = count
                .checked_add(1)
                .ok_or_else(|| self.error("map is too large"))?;
            self.skip_semicolons();
            if matches!(self.current().lexeme, Lexeme::Comma) {
                self.advance();
                self.skip_semicolons();
                if matches!(self.current().lexeme, Lexeme::RightBrace) {
                    self.advance();
                    break;
                }
            } else {
                self.expect_simple(&Lexeme::RightBrace, "expected ',' or '}' in map")?;
                break;
            }
        }
        self.tokens.push(Token::MakeMap(self.item_count(count)?));
        Ok(())
    }

    fn item_count(&self, count: usize) -> Result<u32, CompileError> {
        u32::try_from(count).map_err(|_| self.error("collection is too large"))
    }

    fn expect_terminator(&mut self) -> Result<(), CompileError> {
        if matches!(
            self.current().lexeme,
            Lexeme::Semicolon | Lexeme::RightBrace | Lexeme::End
        ) {
            Ok(())
        } else {
            Err(self.error("expected a newline or ';'"))
        }
    }

    fn expect_simple(&mut self, expected: &Lexeme, message: &str) -> Result<(), CompileError> {
        if std::mem::discriminant(&self.current().lexeme) == std::mem::discriminant(expected) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(message))
        }
    }

    fn skip_semicolons(&mut self) {
        while matches!(self.current().lexeme, Lexeme::Semicolon) {
            self.advance();
        }
    }

    fn emit_jump(&mut self) -> usize {
        let position = self.tokens.len();
        self.tokens.push(Token::Jump(0));
        position
    }

    fn emit_jump_if_false(&mut self) -> usize {
        let position = self.tokens.len();
        self.tokens.push(Token::JumpIfFalse(0));
        position
    }

    fn patch_jump(&mut self, position: usize) -> Result<(), CompileError> {
        let target = self.token_position(self.tokens.len())?;
        match self.tokens.get_mut(position) {
            Some(Token::Jump(value) | Token::JumpIfFalse(value)) => *value = target,
            _ => return Err(self.error("internal jump patch error")),
        }
        Ok(())
    }

    fn token_position(&self, position: usize) -> Result<u32, CompileError> {
        u32::try_from(position).map_err(|_| self.error("program is too large"))
    }

    fn current(&self) -> &Spanned {
        &self.lexemes[self.position]
    }

    fn peek_lexeme(&self, offset: usize) -> Option<&Lexeme> {
        self.lexemes
            .get(self.position + offset)
            .map(|spanned| &spanned.lexeme)
    }

    fn advance(&mut self) {
        if !matches!(self.current().lexeme, Lexeme::End) {
            self.position += 1;
        }
    }

    fn error(&self, message: &str) -> CompileError {
        CompileError {
            position: self.current().position,
            message: message.to_owned(),
        }
    }
}

fn split_field(name: &str) -> Option<(&str, &str)> {
    let (receiver, field) = name.split_once('.')?;
    if receiver.is_empty() || field.is_empty() || field.contains('.') {
        None
    } else {
        Some((receiver, field))
    }
}

fn split_fields(name: &str) -> Option<(&str, Vec<&str>)> {
    let mut parts = name.split('.');
    let receiver = parts.next()?;
    let fields: Vec<_> = parts.collect();
    if receiver.is_empty() || fields.is_empty() || fields.iter().any(|field| field.is_empty()) {
        None
    } else {
        Some((receiver, fields))
    }
}

fn registry_call(name: &str) -> Option<(&'static str, u32, u32)> {
    match name {
        "object.create" => Some(("create", 2, 3)),
        "object.find" => Some(("find", 1, 1)),
        "object.query" => Some(("query", 1, 2)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_variables_and_console_capability() {
        let program = compile(
            "console = object.find(\"console\")\nanswer = 40 + 2\nconsole.println(answer)\n",
        )
        .unwrap();
        assert_eq!(
            program.tokens,
            vec![
                Token::Push(Value::Text("console".to_owned())),
                Token::BindFound {
                    name: "console".to_owned(),
                    arguments: 1,
                },
                Token::Push(Value::Integer(40)),
                Token::Push(Value::Integer(2)),
                Token::Add,
                Token::Store("answer".to_owned()),
                Token::LoadIdentity("console".to_owned()),
                Token::Load("answer".to_owned()),
                Token::ObjectCall {
                    method: "println".to_owned(),
                    arguments: 1,
                },
                Token::Pop,
                Token::Halt,
            ]
        );
    }

    #[test]
    fn compiles_explicit_object_registry_calls() {
        let program =
            compile("item = object.create(\"core.text\", \"one\")\nitem.replace(\"two\")\n")
                .unwrap();
        assert!(program.tokens.contains(&Token::BindCreated {
            name: "item".to_owned(),
            arguments: 2,
        }));
        assert!(program.tokens.contains(&Token::ObjectCall {
            method: "replace".to_owned(),
            arguments: 1,
        }));
        assert!(compile("objects.create(\"core.text\", \"one\")").is_err());
        assert!(compile("io.println(\"old\")").is_err());
        let discovered = compile("aaa = object.find(\"console\")\naaa.println(\"hello\")").unwrap();
        assert!(discovered.tokens.contains(&Token::BindFound {
            name: "aaa".to_owned(),
            arguments: 1,
        }));
        assert!(discovered.tokens.contains(&Token::ObjectCall {
            method: "println".to_owned(),
            arguments: 1,
        }));
        assert!(compile("object.call(\"console\", \"print\")").is_err());
        assert!(compile("object.console()").is_err());
        assert!(compile("missing_call()").is_ok());
    }

    #[test]
    fn compiles_loop_and_condition() {
        let source = r#"
count = 0
console = object.find("console")
while count < 3 {
    count++
}
if count == 3 {
    console.println("ok")
} else {
    console.println("bad")
}
"#;
        let program = compile(source).unwrap();
        assert!(
            program
                .tokens
                .iter()
                .any(|token| matches!(token, Token::Jump(_)))
        );
        assert!(
            program
                .tokens
                .iter()
                .any(|token| matches!(token, Token::JumpIfFalse(_)))
        );
        program.validate().unwrap();
    }

    #[test]
    fn reports_invalid_statement() {
        let error = compile("value ? 1").unwrap_err();
        assert!(error.message.contains("unexpected character"));
    }

    #[test]
    fn compiles_extended_control_flow() {
        let source = r#"
value = 10 % 3
console = object.find("console")
value--
if false && missing {
    console.println("bad")
} else if true or missing {
    console.println(value)
}
while value < 10 {
    value++
    if value == 3 { continue }
    if value == 4 { break }
}
"#;
        let program = compile(source).unwrap();
        assert!(program.tokens.iter().any(|token| token == &Token::Modulo));
        assert!(
            program
                .tokens
                .iter()
                .filter(|token| matches!(token, Token::Jump(_)))
                .count()
                >= 4
        );
        program.validate().unwrap();
    }

    #[test]
    fn rejects_loop_control_outside_loop() {
        assert!(
            compile("break\n")
                .unwrap_err()
                .message
                .contains("inside while")
        );
        assert!(
            compile("continue\n")
                .unwrap_err()
                .message
                .contains("inside while")
        );
    }

    #[test]
    fn compiles_collections_indexing_and_length() {
        let source = r#"
items = [1, 2, 3]
console = object.find("console")
items[0] = 10
user = { name: "Ada", age: 18 }
user["age"] = 19
total = #items + #user
console.println(items[0] + user["age"])
"#;
        let program = compile(source).unwrap();
        assert!(program.tokens.contains(&Token::MakeArray(3)));
        assert!(program.tokens.contains(&Token::MakeMap(2)));
        assert!(program.tokens.contains(&Token::IndexGet));
        assert!(program.tokens.contains(&Token::IndexSet));
        assert!(program.tokens.contains(&Token::Length));
        program.validate().unwrap();
    }

    #[test]
    fn compiles_functions_classes_and_uniform_object_creation() {
        let source = r#"
func twice(value) { return value * 2 }

class Counter {
    value = 0
    private secret = 4
    func increment(amount) {
        this.value = this.value + amount
        return this.value
    }
}

counter = object.create("Counter", { value: twice(5) })
alias link counter
result = alias.increment(2)
"#;
        let program = compile(source).unwrap();
        assert!(
            program.tokens.iter().any(
                |token| matches!(token, Token::DefineFunction { name, .. } if name == "twice")
            )
        );
        assert!(
            program
                .tokens
                .iter()
                .any(|token| matches!(token, Token::DefineClass { name, .. } if name == "Counter"))
        );
        assert!(
            program.tokens.iter().any(
                |token| matches!(token, Token::DefineMethod { name, .. } if name == "increment")
            )
        );
        assert!(program.tokens.contains(&Token::BindLink {
            name: "alias".to_owned(),
            target: "counter".to_owned(),
        }));
        assert!(compile("item = new Counter()").is_err());
        program.validate().unwrap();
    }

    #[test]
    fn expands_import_once_and_include_each_time() {
        let source = "import \"math\"\nimport \"math\"\ninclude \"values\"\nresult = twice(a)\n";
        let program = compile_with_loader(source, |name| match name {
            "math" => Ok("func twice(value) { return value * 2 }\n".to_owned()),
            "values" => Ok("a = 3\n".to_owned()),
            _ => Err("not found".to_owned()),
        })
        .unwrap();
        assert_eq!(
            program
                .tokens
                .iter()
                .filter(
                    |token| matches!(token, Token::DefineFunction { name, .. } if name == "twice")
                )
                .count(),
            1
        );
        assert!(compile(source).unwrap_err().message.contains("loader"));
    }
}

#![allow(clippy::wildcard_imports)]

use super::*;
use crate::lexer::{Lexeme, Spanned};
use std::collections::BTreeMap;
use tf_format::{FloatValue, Program, Token, Value};

pub(super) struct Parser {
    lexemes: Vec<Spanned>,
    position: usize,
    tokens: Vec<Token>,
    loops: Vec<LoopContext>,
    function_depth: usize,
    transaction_depth: usize,
    interactive: bool,
    top_level_mode: TopLevelMode,
    main_functions: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TopLevelMode {
    Any,
    Program,
    Module,
}

struct LoopContext {
    continue_target: usize,
    break_jumps: Vec<usize>,
}

impl Parser {
    pub(super) const fn new(lexemes: Vec<Spanned>, interactive: bool) -> Self {
        Self {
            lexemes,
            position: 0,
            tokens: Vec::new(),
            loops: Vec::new(),
            function_depth: 0,
            transaction_depth: 0,
            interactive,
            top_level_mode: TopLevelMode::Any,
            main_functions: 0,
        }
    }

    pub(super) const fn validating(lexemes: Vec<Spanned>, mode: TopLevelMode) -> Self {
        Self {
            lexemes,
            position: 0,
            tokens: Vec::new(),
            loops: Vec::new(),
            function_depth: 0,
            transaction_depth: 0,
            interactive: false,
            top_level_mode: mode,
            main_functions: 0,
        }
    }

    pub(super) fn compile(mut self) -> Result<Program, CompileError> {
        self.statements(false)?;
        if self.top_level_mode == TopLevelMode::Program && self.main_functions != 1 {
            return Err(self.error("an executable Praxis program must define exactly one main()"));
        }
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
}

mod control_flow;
mod declarations;
mod expression;
mod statements;

impl Parser {
    pub(super) fn item_count(&self, count: usize) -> Result<u32, CompileError> {
        u32::try_from(count).map_err(|_| self.error("collection is too large"))
    }

    pub(super) fn expect_terminator(&mut self) -> Result<(), CompileError> {
        if matches!(
            self.current().lexeme,
            Lexeme::Semicolon | Lexeme::RightBrace | Lexeme::End
        ) {
            Ok(())
        } else {
            Err(self.error("expected a newline or ';'"))
        }
    }

    pub(super) fn expect_simple(
        &mut self,
        expected: &Lexeme,
        message: &str,
    ) -> Result<(), CompileError> {
        if std::mem::discriminant(&self.current().lexeme) == std::mem::discriminant(expected) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(message))
        }
    }

    pub(super) fn skip_semicolons(&mut self) {
        while matches!(self.current().lexeme, Lexeme::Semicolon) {
            self.advance();
        }
    }

    pub(super) fn emit_jump(&mut self) -> usize {
        let position = self.tokens.len();
        self.tokens.push(Token::Jump(0));
        position
    }

    pub(super) fn emit_jump_if_false(&mut self) -> usize {
        let position = self.tokens.len();
        self.tokens.push(Token::JumpIfFalse(0));
        position
    }

    pub(super) fn patch_jump(&mut self, position: usize) -> Result<(), CompileError> {
        let target = self.token_position(self.tokens.len())?;
        match self.tokens.get_mut(position) {
            Some(Token::Jump(value) | Token::JumpIfFalse(value)) => *value = target,
            _ => return Err(self.error("internal jump patch error")),
        }
        Ok(())
    }

    pub(super) fn token_position(&self, position: usize) -> Result<u32, CompileError> {
        u32::try_from(position).map_err(|_| self.error("program is too large"))
    }

    pub(super) fn current(&self) -> &Spanned {
        &self.lexemes[self.position]
    }

    pub(super) fn peek_lexeme(&self, offset: usize) -> Option<&Lexeme> {
        self.lexemes
            .get(self.position + offset)
            .map(|spanned| &spanned.lexeme)
    }

    pub(super) fn advance(&mut self) {
        if !matches!(self.current().lexeme, Lexeme::End) {
            self.position += 1;
        }
    }

    pub(super) fn error(&self, message: &str) -> CompileError {
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

#![allow(clippy::wildcard_imports)]

use super::*;

impl Parser {
    pub(super) fn if_statement(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn while_statement(&mut self) -> Result<(), CompileError> {
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
}

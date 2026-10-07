#![allow(clippy::wildcard_imports)]

use super::*;

impl Parser {
    pub(super) fn statements(&mut self, in_block: bool) -> Result<(), CompileError> {
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

    pub(super) fn statement(&mut self) -> Result<(), CompileError> {
        if self.function_depth == 0
            && self.top_level_mode == TopLevelMode::Program
            && !matches!(
                self.current().lexeme,
                Lexeme::Func | Lexeme::Class | Lexeme::Public | Lexeme::Private
            )
        {
            return Err(self.error(
                "executable statements belong inside main(); only declarations are allowed at program top level",
            ));
        }
        if self.interactive && self.function_depth == 0 && self.try_interactive_expression()? {
            return Ok(());
        }
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

    pub(super) fn try_interactive_expression(&mut self) -> Result<bool, CompileError> {
        let original_position = self.position;
        let token_start = self.tokens.len();
        if self.expression().is_err()
            || !matches!(
                self.current().lexeme,
                Lexeme::Semicolon | Lexeme::RightBrace | Lexeme::End
            )
        {
            self.position = original_position;
            self.tokens.truncate(token_start);
            return Ok(false);
        }
        let is_terminal_output = matches!(
            self.tokens.last(),
            Some(Token::ObjectCall { method, .. }) if method == "print" || method == "println"
        );
        if !is_terminal_output {
            self.tokens
                .insert(token_start, Token::LoadIdentity("terminal".to_owned()));
            self.tokens.push(Token::ObjectCall {
                method: "println".to_owned(),
                arguments: 1,
            });
            self.tokens.push(Token::Pop);
        }
        self.expect_terminator()?;
        Ok(true)
    }

    pub(super) fn assignment_statement(&mut self, name: String) -> Result<(), CompileError> {
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

    pub(super) fn field_assignment_statement(
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

    pub(super) fn return_statement(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn try_statement(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn transaction_statement(&mut self) -> Result<(), CompileError> {
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
}

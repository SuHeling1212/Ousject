#![allow(clippy::wildcard_imports)]

use super::*;

impl Parser {
    pub(super) fn expression(&mut self) -> Result<(), CompileError> {
        self.logical_or()
    }

    pub(super) fn break_statement(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn continue_statement(&mut self) -> Result<(), CompileError> {
        self.advance();
        let target = self
            .loops
            .last()
            .map(|context| context.continue_target)
            .ok_or_else(|| self.error("'continue' is only valid inside while"))?;
        self.tokens.push(Token::Jump(self.token_position(target)?));
        self.expect_terminator()
    }

    pub(super) fn logical_or(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn logical_and(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn comparison(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn term(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn factor(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn unary(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn postfix(&mut self) -> Result<(), CompileError> {
        self.primary()?;
        while matches!(self.current().lexeme, Lexeme::LeftBracket) {
            self.advance();
            self.expression()?;
            self.expect_simple(&Lexeme::RightBracket, "expected ']' after index")?;
            self.tokens.push(Token::IndexGet);
        }
        Ok(())
    }

    pub(super) fn primary(&mut self) -> Result<(), CompileError> {
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
    pub(super) fn call_expression(&mut self, name: &str) -> Result<(), CompileError> {
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

    pub(super) fn array_literal(&mut self) -> Result<(), CompileError> {
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

    pub(super) fn map_literal(&mut self) -> Result<(), CompileError> {
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
}

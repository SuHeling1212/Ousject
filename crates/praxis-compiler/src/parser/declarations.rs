#![allow(clippy::wildcard_imports)]

use super::*;

impl Parser {
    pub(super) fn function_declaration(
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
        if class.is_none() && self.function_depth == 0 && name == "main" {
            match self.top_level_mode {
                TopLevelMode::Program => {
                    if !parameters.is_empty() {
                        return Err(self.error("main() cannot have parameters"));
                    }
                    self.main_functions += 1;
                    if self.main_functions > 1 {
                        return Err(
                            self.error("an executable Praxis program can define only one main()")
                        );
                    }
                }
                TopLevelMode::Module => {
                    return Err(self.error("imported Praxis source cannot define main()"));
                }
                TopLevelMode::Any => {}
            }
        }
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

    pub(super) fn class_declaration(&mut self, _public: bool) -> Result<(), CompileError> {
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

    pub(super) fn parameter_names(&mut self) -> Result<Vec<String>, CompileError> {
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

    pub(super) fn constant_value(&mut self) -> Result<Value, CompileError> {
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
}

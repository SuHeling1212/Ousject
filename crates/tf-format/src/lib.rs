//! TF program representation and stable binary encoding.

use oms_types::ValueError;
pub use oms_types::{FloatValue, Value};
use std::collections::BTreeMap;
use std::fmt;

const MAGIC: &[u8; 4] = b"OTF0";
const MAX_STRING_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOKENS: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Push(Value),
    Load(String),
    Store(String),
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Not,
    Jump(u32),
    JumpIfFalse(u32),
    Pop,
    MakeArray(u32),
    MakeMap(u32),
    IndexGet,
    IndexSet,
    IndexIncrement,
    IndexDecrement,
    Length,
    RegistryCall {
        method: String,
        arguments: u32,
    },
    ObjectCall {
        method: String,
        arguments: u32,
    },
    LoadIdentity(String),
    BindCreated {
        name: String,
        arguments: u32,
    },
    BindFound {
        name: String,
        arguments: u32,
    },
    DefineFunction {
        name: String,
        parameters: Vec<String>,
        end: u32,
    },
    CallFunction {
        name: String,
        arguments: u32,
    },
    Return,
    DefineClass {
        name: String,
        parent: Option<String>,
        fields: BTreeMap<String, Value>,
        private_fields: Vec<String>,
        end: u32,
    },
    DefineMethod {
        class: String,
        name: String,
        parameters: Vec<String>,
        public: bool,
        end: u32,
    },
    GetField(String),
    SetField(String),
    BindLink {
        name: String,
        target: String,
    },
    SuperCall {
        method: String,
        arguments: u32,
    },
    BeginTry {
        catch: u32,
        end: u32,
        error: String,
    },
    EndTry {
        end: u32,
    },
    Transaction {
        end: u32,
    },
    CommitTransaction,
    Halt,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Program {
    pub tokens: Vec<Token>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TfError {
    InvalidMagic,
    UnsupportedVersion,
    UnexpectedEnd,
    InvalidTag(u8),
    InvalidUtf8,
    InvalidValue(ValueError),
    LimitExceeded,
    TrailingData,
    InvalidJump { position: usize, target: u32 },
}

impl fmt::Display for TfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for TfError {}

impl From<ValueError> for TfError {
    fn from(error: ValueError) -> Self {
        Self::InvalidValue(error)
    }
}

impl Program {
    /// Validates token targets and program limits.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized program or an out-of-range jump.
    pub fn validate(&self) -> Result<(), TfError> {
        if self.tokens.len() > MAX_TOKENS {
            return Err(TfError::LimitExceeded);
        }
        for (position, token) in self.tokens.iter().enumerate() {
            if let Token::Jump(target)
            | Token::JumpIfFalse(target)
            | Token::DefineFunction { end: target, .. }
            | Token::DefineClass { end: target, .. }
            | Token::DefineMethod { end: target, .. }
            | Token::EndTry { end: target }
            | Token::Transaction { end: target } = token
            {
                let target_index = usize::try_from(*target).map_err(|_| TfError::InvalidJump {
                    position,
                    target: *target,
                })?;
                if target_index >= self.tokens.len() {
                    return Err(TfError::InvalidJump {
                        position,
                        target: *target,
                    });
                }
            }
            if let Token::BeginTry { catch, end, .. } = token {
                for target in [catch, end] {
                    let target_index =
                        usize::try_from(*target).map_err(|_| TfError::InvalidJump {
                            position,
                            target: *target,
                        })?;
                    if target_index >= self.tokens.len() {
                        return Err(TfError::InvalidJump {
                            position,
                            target: *target,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Encodes this program into the stable TF binary format.
    ///
    /// # Errors
    ///
    /// Returns an error if validation fails or a length exceeds the format limit.
    pub fn encode(&self) -> Result<Vec<u8>, TfError> {
        self.validate()?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        write_u32(&mut bytes, usize_to_u32(self.tokens.len())?);
        for token in &self.tokens {
            encode_token(&mut bytes, token)?;
        }
        Ok(bytes)
    }

    /// Decodes and validates a TF binary program.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, oversized or unsupported input.
    pub fn decode(bytes: &[u8]) -> Result<Self, TfError> {
        let mut reader = Reader::new(bytes);
        if reader.take(4)? != MAGIC {
            return Err(TfError::InvalidMagic);
        }
        let count = usize::try_from(reader.u32()?).map_err(|_| TfError::LimitExceeded)?;
        if count > MAX_TOKENS {
            return Err(TfError::LimitExceeded);
        }
        let mut tokens = Vec::with_capacity(count);
        for _ in 0..count {
            tokens.push(decode_token(&mut reader)?);
        }
        if !reader.is_empty() {
            return Err(TfError::TrailingData);
        }
        let program = Self { tokens };
        program.validate()?;
        Ok(program)
    }
}

#[allow(clippy::too_many_lines)]
fn encode_token(bytes: &mut Vec<u8>, token: &Token) -> Result<(), TfError> {
    match token {
        Token::Push(value) => {
            bytes.push(0);
            encode_value(bytes, value)?;
        }
        Token::Load(name) => {
            bytes.push(1);
            write_string(bytes, name)?;
        }
        Token::Store(name) => {
            bytes.push(2);
            write_string(bytes, name)?;
        }
        Token::Add => bytes.push(3),
        Token::Subtract => bytes.push(4),
        Token::Multiply => bytes.push(5),
        Token::Divide => bytes.push(6),
        Token::Modulo => bytes.push(19),
        Token::Equal => bytes.push(7),
        Token::NotEqual => bytes.push(8),
        Token::Less => bytes.push(9),
        Token::LessEqual => bytes.push(10),
        Token::Greater => bytes.push(11),
        Token::GreaterEqual => bytes.push(12),
        Token::Not => bytes.push(13),
        Token::Jump(target) => {
            bytes.push(14);
            write_u32(bytes, *target);
        }
        Token::JumpIfFalse(target) => {
            bytes.push(15);
            write_u32(bytes, *target);
        }
        Token::Pop => bytes.push(17),
        Token::Halt => bytes.push(18),
        Token::MakeArray(count) => {
            bytes.push(20);
            write_u32(bytes, *count);
        }
        Token::MakeMap(count) => {
            bytes.push(21);
            write_u32(bytes, *count);
        }
        Token::IndexGet => bytes.push(22),
        Token::IndexSet => bytes.push(23),
        Token::IndexIncrement => bytes.push(39),
        Token::IndexDecrement => bytes.push(40),
        Token::Length => bytes.push(24),
        Token::RegistryCall { method, arguments } => {
            bytes.push(25);
            write_string(bytes, method)?;
            write_u32(bytes, *arguments);
        }
        Token::ObjectCall { method, arguments } => {
            bytes.push(26);
            write_string(bytes, method)?;
            write_u32(bytes, *arguments);
        }
        Token::LoadIdentity(name) => {
            bytes.push(27);
            write_string(bytes, name)?;
        }
        Token::BindCreated { name, arguments } => {
            bytes.push(28);
            write_string(bytes, name)?;
            write_u32(bytes, *arguments);
        }
        Token::BindFound { name, arguments } => {
            bytes.push(29);
            write_string(bytes, name)?;
            write_u32(bytes, *arguments);
        }
        Token::DefineFunction {
            name,
            parameters,
            end,
        } => {
            bytes.push(30);
            write_string(bytes, name)?;
            write_strings(bytes, parameters)?;
            write_u32(bytes, *end);
        }
        Token::CallFunction { name, arguments } => {
            bytes.push(31);
            write_string(bytes, name)?;
            write_u32(bytes, *arguments);
        }
        Token::Return => bytes.push(32),
        Token::DefineClass {
            name,
            parent,
            fields,
            private_fields,
            end,
        } => {
            bytes.push(33);
            write_string(bytes, name)?;
            write_optional_string(bytes, parent.as_deref())?;
            write_u32(bytes, usize_to_u32(fields.len())?);
            for (field, value) in fields {
                write_string(bytes, field)?;
                encode_value(bytes, value)?;
            }
            write_strings(bytes, private_fields)?;
            write_u32(bytes, *end);
        }
        Token::DefineMethod {
            class,
            name,
            parameters,
            public,
            end,
        } => {
            bytes.push(34);
            write_string(bytes, class)?;
            write_string(bytes, name)?;
            write_strings(bytes, parameters)?;
            bytes.push(u8::from(*public));
            write_u32(bytes, *end);
        }
        Token::GetField(field) => {
            bytes.push(35);
            write_string(bytes, field)?;
        }
        Token::SetField(field) => {
            bytes.push(36);
            write_string(bytes, field)?;
        }
        Token::BindLink { name, target } => {
            bytes.push(37);
            write_string(bytes, name)?;
            write_string(bytes, target)?;
        }
        Token::SuperCall { method, arguments } => {
            bytes.push(38);
            write_string(bytes, method)?;
            write_u32(bytes, *arguments);
        }
        Token::BeginTry { catch, end, error } => {
            bytes.push(41);
            write_u32(bytes, *catch);
            write_u32(bytes, *end);
            write_string(bytes, error)?;
        }
        Token::EndTry { end } => {
            bytes.push(42);
            write_u32(bytes, *end);
        }
        Token::Transaction { end } => {
            bytes.push(43);
            write_u32(bytes, *end);
        }
        Token::CommitTransaction => bytes.push(44),
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn decode_token(reader: &mut Reader<'_>) -> Result<Token, TfError> {
    Ok(match reader.u8()? {
        0 => Token::Push(decode_value(reader)?),
        1 => Token::Load(reader.string()?),
        2 => Token::Store(reader.string()?),
        3 => Token::Add,
        4 => Token::Subtract,
        5 => Token::Multiply,
        6 => Token::Divide,
        7 => Token::Equal,
        8 => Token::NotEqual,
        9 => Token::Less,
        10 => Token::LessEqual,
        11 => Token::Greater,
        12 => Token::GreaterEqual,
        13 => Token::Not,
        14 => Token::Jump(reader.u32()?),
        15 => Token::JumpIfFalse(reader.u32()?),
        17 => Token::Pop,
        18 => Token::Halt,
        19 => Token::Modulo,
        20 => Token::MakeArray(reader.u32()?),
        21 => Token::MakeMap(reader.u32()?),
        22 => Token::IndexGet,
        23 => Token::IndexSet,
        24 => Token::Length,
        25 => Token::RegistryCall {
            method: reader.string()?,
            arguments: reader.u32()?,
        },
        26 => Token::ObjectCall {
            method: reader.string()?,
            arguments: reader.u32()?,
        },
        27 => Token::LoadIdentity(reader.string()?),
        28 => Token::BindCreated {
            name: reader.string()?,
            arguments: reader.u32()?,
        },
        29 => Token::BindFound {
            name: reader.string()?,
            arguments: reader.u32()?,
        },
        30 => Token::DefineFunction {
            name: reader.string()?,
            parameters: reader.strings()?,
            end: reader.u32()?,
        },
        31 => Token::CallFunction {
            name: reader.string()?,
            arguments: reader.u32()?,
        },
        32 => Token::Return,
        33 => {
            let name = reader.string()?;
            let parent = reader.optional_string()?;
            let mut fields = BTreeMap::new();
            for _ in 0..reader.count()? {
                let field = reader.string()?;
                let value = decode_value(reader)?;
                if fields.insert(field, value).is_some() {
                    return Err(TfError::InvalidValue(ValueError::DuplicateKey(
                        "class field".to_owned(),
                    )));
                }
            }
            Token::DefineClass {
                name,
                parent,
                fields,
                private_fields: reader.strings()?,
                end: reader.u32()?,
            }
        }
        34 => Token::DefineMethod {
            class: reader.string()?,
            name: reader.string()?,
            parameters: reader.strings()?,
            public: match reader.u8()? {
                0 => false,
                1 => true,
                _ => return Err(TfError::InvalidTag(34)),
            },
            end: reader.u32()?,
        },
        35 => Token::GetField(reader.string()?),
        36 => Token::SetField(reader.string()?),
        37 => Token::BindLink {
            name: reader.string()?,
            target: reader.string()?,
        },
        38 => Token::SuperCall {
            method: reader.string()?,
            arguments: reader.u32()?,
        },
        39 => Token::IndexIncrement,
        40 => Token::IndexDecrement,
        41 => Token::BeginTry {
            catch: reader.u32()?,
            end: reader.u32()?,
            error: reader.string()?,
        },
        42 => Token::EndTry { end: reader.u32()? },
        43 => Token::Transaction { end: reader.u32()? },
        44 => Token::CommitTransaction,
        tag => return Err(TfError::InvalidTag(tag)),
    })
}

fn encode_value(bytes: &mut Vec<u8>, value: &Value) -> Result<(), TfError> {
    let encoded = value.encode()?;
    write_u32(bytes, usize_to_u32(encoded.len())?);
    bytes.extend_from_slice(&encoded);
    Ok(())
}

fn decode_value(reader: &mut Reader<'_>) -> Result<Value, TfError> {
    Value::decode(reader.bytes()?).map_err(TfError::from)
}

fn write_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), TfError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(TfError::LimitExceeded);
    }
    write_u32(bytes, usize_to_u32(value.len())?);
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_strings(bytes: &mut Vec<u8>, values: &[String]) -> Result<(), TfError> {
    write_u32(bytes, usize_to_u32(values.len())?);
    for value in values {
        write_string(bytes, value)?;
    }
    Ok(())
}

fn write_optional_string(bytes: &mut Vec<u8>, value: Option<&str>) -> Result<(), TfError> {
    if let Some(value) = value {
        bytes.push(1);
        write_string(bytes, value)?;
    } else {
        bytes.push(0);
    }
    Ok(())
}

fn usize_to_u32(value: usize) -> Result<u32, TfError> {
    u32::try_from(value).map_err(|_| TfError::LimitExceeded)
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], TfError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(TfError::LimitExceeded)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(TfError::UnexpectedEnd)?;
        self.position = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, TfError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, TfError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn count(&mut self) -> Result<usize, TfError> {
        let count = usize::try_from(self.u32()?).map_err(|_| TfError::LimitExceeded)?;
        if count > MAX_TOKENS {
            return Err(TfError::LimitExceeded);
        }
        Ok(count)
    }

    fn strings(&mut self) -> Result<Vec<String>, TfError> {
        let mut values = Vec::new();
        for _ in 0..self.count()? {
            values.push(self.string()?);
        }
        Ok(values)
    }

    fn optional_string(&mut self) -> Result<Option<String>, TfError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            tag => Err(TfError::InvalidTag(tag)),
        }
    }

    fn string(&mut self) -> Result<String, TfError> {
        let length = usize::try_from(self.u32()?).map_err(|_| TfError::LimitExceeded)?;
        if length > MAX_STRING_BYTES {
            return Err(TfError::LimitExceeded);
        }
        let value = std::str::from_utf8(self.take(length)?).map_err(|_| TfError::InvalidUtf8)?;
        Ok(value.to_owned())
    }

    fn bytes(&mut self) -> Result<&'a [u8], TfError> {
        let length = usize::try_from(self.u32()?).map_err(|_| TfError::LimitExceeded)?;
        if length > MAX_STRING_BYTES {
            return Err(TfError::LimitExceeded);
        }
        self.take(length)
    }

    fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_round_trip() {
        let program = Program {
            tokens: vec![
                Token::Push(Value::Integer(40)),
                Token::Push(Value::Integer(2)),
                Token::Modulo,
                Token::Push(Value::Integer(0)),
                Token::Add,
                Token::Store("answer".to_owned()),
                Token::Load("answer".to_owned()),
                Token::Halt,
            ],
        };
        assert_eq!(Program::decode(&program.encode().unwrap()), Ok(program));
    }

    #[test]
    fn object_calls_round_trip_and_old_versions_are_rejected() {
        let program = Program {
            tokens: vec![
                Token::RegistryCall {
                    method: "create".to_owned(),
                    arguments: 2,
                },
                Token::ObjectCall {
                    method: "value".to_owned(),
                    arguments: 0,
                },
                Token::BindFound {
                    name: "terminal".to_owned(),
                    arguments: 1,
                },
                Token::Halt,
            ],
        };
        let encoded = program.encode().unwrap();
        assert!(encoded.starts_with(b"OTF0"));
        assert_eq!(Program::decode(&encoded), Ok(program.clone()));
        let mut previous = encoded.clone();
        previous[..4].copy_from_slice(b"OTF3");
        assert_eq!(Program::decode(&previous), Err(TfError::InvalidMagic));
    }

    #[test]
    fn rejects_invalid_jump() {
        let program = Program {
            tokens: vec![Token::Jump(9), Token::Halt],
        };
        assert!(matches!(
            program.validate(),
            Err(TfError::InvalidJump { .. })
        ));
    }

    #[test]
    fn rejects_truncated_program() {
        assert_eq!(Program::decode(b"OTF0\x01"), Err(TfError::UnexpectedEnd));
    }

    #[test]
    fn rejects_old_program_versions() {
        assert_eq!(Program::decode(b"OTF1"), Err(TfError::InvalidMagic));
    }
}

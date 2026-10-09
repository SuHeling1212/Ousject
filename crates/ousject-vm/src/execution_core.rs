//! OTF token and Process-state semantics shared across Hosted execution paths.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use oms_types::{ObjectId, OmsError, SubjectId, ValueError};
use tf_format::{Program, TfError, Token, Value};

#[allow(clippy::wildcard_imports)]
pub(crate) mod operations {
    use super::*;
    use std::io::Read;
    include!("machine/builtins.rs");
    include!("machine/value_ops.rs");
}

pub(crate) use operations::{
    arithmetic, compare, execute_collection_token, math_capability, text_capability,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Ready,
    Running,
    Waiting,
    Suspended,
    Halted,
    Terminated,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WaitReason {
    #[default]
    None,
    Timer {
        timer: Option<ObjectId>,
        deadline_unix_ms: u64,
    },
    Ipc(ObjectId),
    Effect(ObjectId),
    Input(ObjectId),
    Process(ObjectId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerLease {
    pub owner: ObjectId,
    pub generation: u64,
    pub deadline_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessState {
    pub program: ObjectId,
    pub subject: SubjectId,
    pub token_position: u32,
    pub stack: Vec<Value>,
    pub variables: BTreeMap<String, ObjectId>,
    pub status: ProcessStatus,
    pub wait_reason: WaitReason,
    pub lease_owner: Option<ObjectId>,
    pub lease_generation: u64,
    pub lease_deadline_unix_ms: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<Value>,
    pub ended_at_unix_ms: Option<u64>,
    pub frames: Vec<CallFrame>,
    pub handlers: Vec<ExceptionHandler>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFrame {
    pub return_position: u32,
    pub stack_base: u32,
    pub locals: BTreeMap<String, ObjectId>,
    pub receiver: Option<ObjectId>,
    pub class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionHandler {
    pub catch_position: u32,
    pub error_name: String,
    pub frame_depth: u32,
    pub stack_base: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
    Oms(OmsError),
    Tf(TfError),
    Value(ValueError),
    InvalidProcessState(String),
    StackUnderflow,
    UndefinedVariable(String),
    TypeError(&'static str),
    DivisionByZero,
    IndexOutOfBounds,
    MissingKey(String),
    MissingProvider(&'static str),
    Provider(String),
    TokenPositionOutOfRange(u32),
    StepLimitExceeded(u64),
    WorkerLeaseBusy(ObjectId),
    WorkerLeaseExpired(ObjectId),
}

impl fmt::Display for VmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for VmError {}

impl From<OmsError> for VmError {
    fn from(error: OmsError) -> Self {
        Self::Oms(error)
    }
}

impl From<TfError> for VmError {
    fn from(error: TfError) -> Self {
        Self::Tf(error)
    }
}

impl From<ValueError> for VmError {
    fn from(error: ValueError) -> Self {
        Self::Value(error)
    }
}

pub(crate) trait TokenHost {
    fn load_variable(&mut self, state: &ProcessState, name: &str) -> Result<Value, VmError>;
    fn store_variable(
        &mut self,
        state: &mut ProcessState,
        name: String,
        value: &Value,
    ) -> Result<(), VmError>;
    fn get_field(
        &mut self,
        state: &ProcessState,
        receiver: &Value,
        field: &str,
        program: &Program,
    ) -> Result<Value, VmError>;
}

pub(crate) fn execute_binary_pure_token(
    state: &mut ProcessState,
    token: &Token,
) -> Result<(), VmError> {
    let start = state
        .stack
        .len()
        .checked_sub(2)
        .ok_or(VmError::StackUnderflow)?;
    if matches!(
        token,
        Token::Add | Token::Subtract | Token::Multiply | Token::Divide | Token::Modulo
    ) {
        let result =
            operations::arithmetic_ref(token, &state.stack[start], &state.stack[start + 1])?;
        state.stack.truncate(start);
        state.stack.push(result);
    } else {
        let result = operations::compare(token, &state.stack[start], &state.stack[start + 1])?;
        state.stack.truncate(start);
        state.stack.push(Value::Bool(result));
    }
    Ok(())
}

/// Executes one token handled by the shared side-effect-free VM core.
/// Returns `false` for tokens that require a Hosted system service.
pub(crate) fn execute_token(
    host: &mut impl TokenHost,
    state: &mut ProcessState,
    program: &Program,
) -> Result<bool, VmError> {
    let position = usize::try_from(state.token_position)
        .map_err(|_| VmError::TokenPositionOutOfRange(state.token_position))?;
    let token = program
        .tokens
        .get(position)
        .cloned()
        .ok_or(VmError::TokenPositionOutOfRange(state.token_position))?;
    let next = state
        .token_position
        .checked_add(1)
        .ok_or(VmError::TokenPositionOutOfRange(state.token_position))?;

    if execute_control_token(state, &token, next)? {
        return Ok(true);
    }

    match token {
        Token::Push(value) => state.stack.push(value),
        Token::Load(name) => state.stack.push(host.load_variable(state, &name)?),
        Token::LoadIdentity(name) => {
            let object = binding_id(state, &name)?;
            state.stack.push(Value::Text(object.to_string()));
        }
        Token::Add
        | Token::Subtract
        | Token::Multiply
        | Token::Divide
        | Token::Modulo
        | Token::Equal
        | Token::NotEqual
        | Token::Less
        | Token::LessEqual
        | Token::Greater
        | Token::GreaterEqual => execute_binary_pure_token(state, &token)?,
        Token::Not => {
            let value = state.stack.pop().ok_or(VmError::StackUnderflow)?;
            state.stack.push(Value::Bool(!value.is_truthy()));
        }
        Token::Pop => {
            state.stack.pop().ok_or(VmError::StackUnderflow)?;
        }
        Token::Store(name) => {
            let value = state.stack.pop().ok_or(VmError::StackUnderflow)?;
            if let Err(error) = host.store_variable(state, name, &value) {
                state.stack.push(value);
                return Err(error);
            }
            state.token_position = next;
            return Ok(true);
        }
        Token::MakeArray(_)
        | Token::MakeMap(_)
        | Token::IndexGet
        | Token::IndexSet
        | Token::IndexIncrement
        | Token::IndexDecrement
        | Token::Length => operations::execute_collection_token(&token, &mut state.stack)?,
        Token::GetField(field) => {
            let receiver = state.stack.last().ok_or(VmError::StackUnderflow)?;
            let value = host.get_field(state, receiver, &field, program)?;
            state.stack.pop();
            state.stack.push(value);
        }
        Token::BindLink { name, target } => {
            let target = binding_id(state, &target)?;
            bind_name(state, name, target);
        }
        _ => return Ok(false),
    }

    state.token_position = next;
    Ok(true)
}

pub(crate) fn apply_halt(state: &mut ProcessState, next: u32) {
    state.status = ProcessStatus::Halted;
    state.result = state.stack.last().cloned();
    state.error = None;
    state.wait_reason = WaitReason::None;
    state.ended_at_unix_ms = None;
    state.token_position = next;
}

fn execute_control_token(
    state: &mut ProcessState,
    token: &Token,
    next: u32,
) -> Result<bool, VmError> {
    match token {
        Token::Jump(target) => state.token_position = *target,
        Token::JumpIfFalse(target) => {
            let condition = state.stack.pop().ok_or(VmError::StackUnderflow)?;
            state.token_position = if condition.is_truthy() { next } else { *target };
        }
        Token::DefineFunction { end, .. }
        | Token::DefineClass { end, .. }
        | Token::DefineMethod { end, .. } => state.token_position = *end,
        Token::Return => {
            let frame = state
                .frames
                .last()
                .ok_or(VmError::TypeError("return outside function"))?;
            let stack_base =
                usize::try_from(frame.stack_base).map_err(|_| VmError::StackUnderflow)?;
            let return_position = frame.return_position;
            let result = state.stack.pop().ok_or(VmError::StackUnderflow)?;
            state.frames.pop();
            state.stack.truncate(stack_base);
            state.stack.push(result);
            state.token_position = return_position;
            state.handlers.retain(|handler| {
                usize::try_from(handler.frame_depth).is_ok_and(|depth| depth <= state.frames.len())
            });
        }
        Token::BeginTry { catch, error, .. } => {
            state.handlers.push(ExceptionHandler {
                catch_position: *catch,
                error_name: error.clone(),
                frame_depth: u32::try_from(state.frames.len())
                    .map_err(|_| VmError::InvalidProcessState(String::from("too many frames")))?,
                stack_base: u32::try_from(state.stack.len())
                    .map_err(|_| VmError::InvalidProcessState(String::from("stack too large")))?,
            });
            state.token_position = next;
        }
        Token::EndTry { end } => {
            state
                .handlers
                .pop()
                .ok_or(VmError::TypeError("try handler stack is empty"))?;
            state.token_position = *end;
        }
        Token::Halt => {
            apply_halt(state, next);
            state.lease_owner = None;
            state.lease_deadline_unix_ms = None;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn binding_id(state: &ProcessState, name: &str) -> Result<ObjectId, VmError> {
    if matches!(name, "this" | "super") {
        return state
            .frames
            .last()
            .and_then(|frame| frame.receiver)
            .ok_or(VmError::TypeError("this or super outside method"));
    }
    state
        .frames
        .last()
        .and_then(|frame| frame.locals.get(name))
        .or_else(|| state.variables.get(name))
        .copied()
        .ok_or_else(|| VmError::UndefinedVariable(name.to_owned()))
}

fn bind_name(state: &mut ProcessState, name: String, object: ObjectId) {
    if let Some(frame) = state.frames.last_mut() {
        frame.locals.insert(name, object);
    } else {
        state.variables.insert(name, object);
    }
}

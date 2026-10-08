//! Native, cooperative adapter over the shared Process and OTF execution core.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use oms_runtime::{AccessContext, CreateObject, InMemoryObjectManager};
use oms_types::{CORE_PROCESS_TYPE, CORE_PROGRAM_TYPE, CORE_VALUE_TYPE, ObjectId, SYSTEM_SUBJECT};
use tf_format::{Program, Token, Value};

use crate::execution_core::{
    CallFrame, ProcessState, ProcessStatus, TokenHost, VmError, WaitReason, decode_process_state,
    encode_process_state, execute_token,
};

pub const PROCESS_TYPE: oms_types::TypeId = CORE_PROCESS_TYPE;
pub const PROGRAM_TYPE: oms_types::TypeId = CORE_PROGRAM_TYPE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRunReport {
    pub process: ObjectId,
    pub steps: u64,
    pub status: ProcessStatus,
}

#[derive(Debug)]
pub struct NativeVirtualMachine {
    manager: Rc<InMemoryObjectManager>,
    context: AccessContext,
}

impl NativeVirtualMachine {
    pub fn new(manager: Rc<InMemoryObjectManager>) -> Self {
        Self {
            manager,
            context: AccessContext::new(SYSTEM_SUBJECT),
        }
    }

    pub fn manager(&self) -> &Rc<InMemoryObjectManager> {
        &self.manager
    }

    pub fn process_state(&self, process: ObjectId) -> Result<ProcessState, VmError> {
        let view = self.manager.read(self.context, process)?;
        if view.header().type_id != PROCESS_TYPE {
            return Err(VmError::TypeError("Object is not a Process"));
        }
        decode_process_state(view.state())
    }

    /// Creates a Program Object and its Ready Process atomically in the OMS.
    pub fn create_process(&self, program: &Program) -> Result<ObjectId, VmError> {
        let encoded = program.encode()?;
        let halt = program
            .tokens
            .iter()
            .position(|token| matches!(token, Token::Halt))
            .ok_or(VmError::TypeError("Program has no Halt token"))?;
        let halt = u32::try_from(halt).map_err(|_| VmError::TokenPositionOutOfRange(u32::MAX))?;
        let entry = program
            .tokens
            .iter()
            .position(|token| matches!(token, Token::DefineFunction { name, parameters, .. } if name == "main" && parameters.is_empty()))
            .map_or(Ok(0), |position| {
                u32::try_from(position + 1)
                    .map_err(|_| VmError::TokenPositionOutOfRange(u32::MAX))
            })?;
        let program_request = CreateObject::new(PROGRAM_TYPE, encoded);
        let program_id = program_request.id;
        let state = ProcessState {
            program: program_id,
            subject: self.context.subject,
            token_position: entry,
            stack: Vec::new(),
            variables: BTreeMap::new(),
            status: ProcessStatus::Ready,
            wait_reason: WaitReason::None,
            lease_owner: None,
            lease_generation: 0,
            lease_deadline_unix_ms: None,
            result: None,
            error: None,
            ended_at_unix_ms: None,
            frames: alloc::vec![CallFrame {
                return_position: halt,
                stack_base: 0,
                locals: BTreeMap::new(),
                receiver: None,
                class: None,
            }],
            handlers: Vec::new(),
        };
        let process_request = CreateObject::new(PROCESS_TYPE, encode_process_state(&state)?)
            .with_link("program", program_id);
        let process_id = process_request.id;
        let mut transaction = self.manager.begin(self.context);
        transaction.create(program_request).create(process_request);
        self.manager.commit(transaction)?;
        Ok(process_id)
    }

    /// Executes a bounded token slice. Every token and its variable writes are
    /// published together with ProcessState through one OMS transaction.
    pub fn run_slice(
        &self,
        process: ObjectId,
        maximum_tokens: u64,
    ) -> Result<NativeRunReport, VmError> {
        let limit = maximum_tokens.clamp(1, 4096);
        let mut steps = 0;
        let initial = self.manager.read(self.context, process)?;
        let mut initial_state = decode_process_state(initial.state())?;
        if initial_state.status == ProcessStatus::Ready {
            initial_state.status = ProcessStatus::Running;
            let mut transition = self.manager.begin(self.context);
            transition
                .expect(process, initial.header().version)
                .update_state(process, encode_process_state(&initial_state)?);
            self.manager.commit(transition)?;
        }
        while steps < limit {
            let view = self.manager.read(self.context, process)?;
            if view.header().type_id != PROCESS_TYPE {
                return Err(VmError::TypeError("Object is not a Process"));
            }
            let mut state = decode_process_state(view.state())?;
            if state.status == ProcessStatus::Halted {
                break;
            }
            if !matches!(state.status, ProcessStatus::Ready | ProcessStatus::Running) {
                return Err(VmError::TypeError("Process is not runnable"));
            }
            let program_view = self.manager.read(self.context, state.program)?;
            if program_view.header().type_id != PROGRAM_TYPE {
                return Err(VmError::TypeError("Process program link is invalid"));
            }
            let program = Program::decode(program_view.state())?;
            state.status = ProcessStatus::Running;
            let mut staged = StagedWrites::default();
            let mut host = NativeTokenHost {
                manager: &self.manager,
                context: self.context,
                staged: &mut staged,
            };
            let before_token = state.clone();
            let token_result = match execute_token(&mut host, &mut state, &program) {
                Ok(true) => Ok(()),
                Ok(false) => Err(VmError::MissingProvider("Native token service")),
                Err(error) => Err(error),
            };
            if let Err(error) = token_result {
                // The token and its staged Object writes are discarded together;
                // persist a terminal failure state so the Process cannot remain
                // spuriously Running after an unavailable Native service.
                state = before_token;
                state.status = ProcessStatus::Failed;
                state.error = Some(Value::Text(error.to_string()));
                state.wait_reason = WaitReason::None;
                state.ended_at_unix_ms = None;
                state.lease_owner = None;
                state.lease_deadline_unix_ms = None;
                let mut failure = self.manager.begin(self.context);
                failure
                    .expect(process, view.header().version)
                    .update_state(process, encode_process_state(&state)?);
                self.manager.commit(failure)?;
                return Err(error);
            }
            if state.status == ProcessStatus::Running {
                state.status = ProcessStatus::Ready;
            }
            let mut transaction = self.manager.begin(self.context);
            transaction.expect(process, view.header().version);
            for (object, encoded) in staged.updates {
                let version = staged.versions.get(&object).copied().ok_or_else(|| {
                    VmError::InvalidProcessState(String::from(
                        "staged Object update has no version",
                    ))
                })?;
                transaction
                    .expect(object, version)
                    .update_state(object, encoded);
            }
            for request in staged.creates {
                transaction.create(request);
            }
            transaction.update_state(process, encode_process_state(&state)?);
            self.manager.commit(transaction)?;
            steps += 1;
            if state.status == ProcessStatus::Halted {
                break;
            }
        }
        let state = decode_process_state(self.manager.read(self.context, process)?.state())?;
        Ok(NativeRunReport {
            process,
            steps,
            status: state.status,
        })
    }
}

#[derive(Default)]
struct StagedWrites {
    updates: BTreeMap<ObjectId, Vec<u8>>,
    versions: BTreeMap<ObjectId, oms_types::ObjectVersion>,
    creates: Vec<CreateObject>,
    values: BTreeMap<ObjectId, Value>,
}

struct NativeTokenHost<'a> {
    manager: &'a InMemoryObjectManager,
    context: AccessContext,
    staged: &'a mut StagedWrites,
}

impl TokenHost for NativeTokenHost<'_> {
    fn load_variable(&mut self, state: &ProcessState, name: &str) -> Result<Value, VmError> {
        let object = state
            .frames
            .last()
            .and_then(|frame| frame.locals.get(name))
            .or_else(|| state.variables.get(name))
            .copied()
            .ok_or_else(|| VmError::UndefinedVariable(name.to_string()))?;
        if let Some(value) = self.staged.values.get(&object) {
            return Ok(value.clone());
        }
        Value::decode(self.manager.read(self.context, object)?.state()).map_err(Into::into)
    }

    fn store_variable(
        &mut self,
        state: &mut ProcessState,
        name: String,
        value: &Value,
    ) -> Result<(), VmError> {
        let existing = state
            .frames
            .last()
            .and_then(|frame| frame.locals.get(&name))
            .or_else(|| state.variables.get(&name))
            .copied();
        let object = if let Some(object) = existing {
            let (version, encoded) =
                self.manager
                    .prepare_replace_value(self.context, object, value)?;
            self.staged.versions.insert(object, version);
            self.staged.updates.insert(object, encoded);
            object
        } else {
            let request = CreateObject::new(CORE_VALUE_TYPE, value.encode()?);
            let object = request.id;
            self.staged.creates.push(request);
            object
        };
        self.staged.values.insert(object, value.clone());
        if let Some(frame) = state.frames.last_mut() {
            frame.locals.insert(name, object);
        } else {
            state.variables.insert(name, object);
        }
        Ok(())
    }

    fn get_field(
        &mut self,
        _state: &ProcessState,
        _receiver: &Value,
        _field: &str,
        _program: &Program,
    ) -> Result<Value, VmError> {
        Err(VmError::MissingProvider("Native object field access"))
    }
}

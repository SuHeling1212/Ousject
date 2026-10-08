//! Native, cooperative adapter over the shared Process and OTF execution core.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use oms_runtime::{
    AccessContext, CreateObject, CreateSpec, InMemoryObjectManager, ObjectQuery, Transaction,
};
use oms_types::{
    CORE_EFFECT_TYPE, CORE_PROCESS_TYPE, CORE_PROGRAM_TYPE, CORE_VALUE_TYPE, Capability, ObjectId,
    SYSTEM_SUBJECT,
};
use ousject_provider::{
    EffectRecord, EffectRecoveryPolicy, EffectStatus, ObjectProvider, ProviderError,
    ProviderRegistry,
};
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
    providers: ProviderRegistry,
}

impl NativeVirtualMachine {
    pub fn new(manager: Rc<InMemoryObjectManager>) -> Self {
        Self {
            manager,
            context: AccessContext::new(SYSTEM_SUBJECT),
            providers: ProviderRegistry::new(),
        }
    }

    /// Registers one trusted Native Provider before user Processes are run.
    ///
    /// # Errors
    ///
    /// Returns duplicate-registration or sealed-registry errors.
    pub fn register_provider(
        &self,
        provider: Arc<dyn ObjectProvider>,
    ) -> Result<(), ProviderError> {
        self.providers.register(provider)
    }

    /// Seals Native Provider registration for this VM instance.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry is unavailable.
    pub fn seal_providers(&self) -> Result<(), ProviderError> {
        self.providers.seal()
    }

    #[must_use]
    pub fn manager(&self) -> &Rc<InMemoryObjectManager> {
        &self.manager
    }

    /// Reads and decodes a Process Object's durable state.
    ///
    /// # Errors
    ///
    /// Returns an OMS error, a type error, or a process-state decoding error.
    pub fn process_state(&self, process: ObjectId) -> Result<ProcessState, VmError> {
        let view = self.manager.read(self.context, process)?;
        if view.header().type_id != PROCESS_TYPE {
            return Err(VmError::TypeError("Object is not a Process"));
        }
        decode_process_state(view.state())
    }

    /// Resolves a boot-scoped secret token owned by a registered Provider.
    ///
    /// # Errors
    ///
    /// Returns a Provider error when its protected in-memory store is unavailable.
    pub fn resolve_secret(&self, token: &str) -> Result<Option<String>, VmError> {
        for type_id in self.providers.types()? {
            if let Some(secret) = self.providers.get(type_id)?.resolve_secret(token)? {
                return Ok(Some(secret));
            }
        }
        Ok(None)
    }

    /// Recovers runnable Processes from durable OMS state after a Native reboot.
    ///
    /// A Process whose last committed state was `Running` has not committed
    /// its next token. It is returned to `Ready` without changing its program
    /// counter, stack, variables, or Object state. All such transitions share
    /// one OMS transaction.
    ///
    /// # Errors
    ///
    /// Returns an OMS error or a Process state decoding/encoding error.
    #[allow(clippy::too_many_lines)] // One recovery transaction spans all durable wait kinds.
    pub fn recover_ready_processes(&self) -> Result<Vec<ObjectId>, VmError> {
        let headers = self.manager.list(self.context)?;
        let mut transaction = self.manager.begin(self.context);
        let mut ready = Vec::new();
        let mut recovered = false;

        for header in headers {
            if header.type_id != PROCESS_TYPE {
                continue;
            }
            let view = self.manager.read(self.context, header.id)?;
            let mut state = decode_process_state(view.state())?;
            match state.status {
                ProcessStatus::Ready => ready.push(header.id),
                ProcessStatus::Running => {
                    state.status = ProcessStatus::Ready;
                    state.lease_owner = None;
                    state.lease_deadline_unix_ms = None;
                    transaction
                        .expect(header.id, view.header().version)
                        .update_state(header.id, encode_process_state(&state)?);
                    ready.push(header.id);
                    recovered = true;
                }
                ProcessStatus::Waiting => {
                    if let WaitReason::Process(child) = state.wait_reason {
                        let child_state = self.process_state(child)?;
                        if matches!(
                            child_state.status,
                            ProcessStatus::Halted
                                | ProcessStatus::Terminated
                                | ProcessStatus::Failed
                        ) {
                            state.status = ProcessStatus::Ready;
                            state.wait_reason = WaitReason::None;
                            let child_view = self.manager.read(self.context, child)?;
                            transaction
                                .expect(child, child_view.header().version)
                                .expect(header.id, view.header().version)
                                .remove_link(header.id, "$waiting_on")
                                .remove_link(child, alloc::format!("$wait:{}", header.id))
                                .update_state(header.id, encode_process_state(&state)?);
                            ready.push(header.id);
                            recovered = true;
                        }
                    } else if let WaitReason::Effect(effect) | WaitReason::Input(effect) =
                        state.wait_reason
                    {
                        let effect_view = self.manager.read(self.context, effect)?;
                        if effect_view.header().type_id != CORE_EFFECT_TYPE {
                            return Err(VmError::InvalidProcessState(String::from(
                                "Process wait reason does not reference a core.effect Object",
                            )));
                        }
                        let mut record =
                            EffectRecord::decode(effect_view.state()).map_err(VmError::from)?;
                        match record.status {
                            EffectStatus::Pending | EffectStatus::Completed => {
                                state.status = ProcessStatus::Ready;
                                state.wait_reason = WaitReason::None;
                                transaction.expect(header.id, view.header().version);
                                if record.status == EffectStatus::Completed
                                    && record.capability == "read_secret"
                                {
                                    // Secret handles live only in boot-scoped Provider memory.
                                    // Re-prompt after reboot instead of restoring a dead handle.
                                    transaction
                                        .expect(effect, effect_view.header().version)
                                        .remove_link(header.id, "$effect");
                                }
                                transaction.update_state(header.id, encode_process_state(&state)?);
                                ready.push(header.id);
                                recovered = true;
                            }
                            EffectStatus::Running => {
                                if record.recovery_policy == EffectRecoveryPolicy::RetryIdempotent {
                                    record = record.retry();
                                    state.status = ProcessStatus::Ready;
                                    state.wait_reason = WaitReason::None;
                                    transaction
                                        .expect(effect, effect_view.header().version)
                                        .expect(header.id, view.header().version)
                                        .update_state(
                                            effect,
                                            record.encode().map_err(VmError::from)?,
                                        )
                                        .update_state(header.id, encode_process_state(&state)?);
                                    ready.push(header.id);
                                } else {
                                    record = record.unknown();
                                    state.status = ProcessStatus::Failed;
                                    state.wait_reason = WaitReason::None;
                                    state.error = Some(Value::Text(String::from(
                                        "Native Provider Effect outcome is unknown after restart",
                                    )));
                                    transaction
                                        .expect(effect, effect_view.header().version)
                                        .expect(header.id, view.header().version)
                                        .update_state(
                                            effect,
                                            record.encode().map_err(VmError::from)?,
                                        )
                                        .remove_link(header.id, "$effect")
                                        .update_state(header.id, encode_process_state(&state)?);
                                }
                                recovered = true;
                            }
                            EffectStatus::Failed | EffectStatus::Unknown => {
                                state.status = ProcessStatus::Failed;
                                state.wait_reason = WaitReason::None;
                                state.error = Some(Value::Text(String::from(
                                    "Native Provider Effect requires explicit resolution",
                                )));
                                transaction
                                    .expect(effect, effect_view.header().version)
                                    .expect(header.id, view.header().version)
                                    .remove_link(header.id, "$effect")
                                    .update_state(header.id, encode_process_state(&state)?);
                                recovered = true;
                            }
                        }
                    }
                }
                ProcessStatus::Suspended
                | ProcessStatus::Halted
                | ProcessStatus::Failed
                | ProcessStatus::Terminated => {}
            }
        }

        if recovered {
            self.manager.commit(transaction)?;
        }
        Ok(ready)
    }

    /// Polls each durable Provider wait once and commits completed operations
    /// together with the awakened Process state.
    ///
    /// This method is intentionally non-blocking. A kernel idle loop can call
    /// it after a timer wakeup; an empty result leaves Processes Waiting.
    ///
    /// # Errors
    ///
    /// Returns an OMS, Provider, or Process-state error.
    pub fn poll_waiting_providers(&self) -> Result<Vec<ObjectId>, VmError> {
        let mut awakened = Vec::new();
        for header in self.manager.list(self.context)? {
            if header.type_id != PROCESS_TYPE {
                continue;
            }
            let process_view = self.manager.read(self.context, header.id)?;
            let state = decode_process_state(process_view.state())?;
            let effect = match state.wait_reason {
                WaitReason::Input(effect) | WaitReason::Effect(effect)
                    if state.status == ProcessStatus::Waiting =>
                {
                    effect
                }
                _ => continue,
            };
            if self.poll_waiting_process(header.id, &process_view, state, effect)? {
                awakened.push(header.id);
            }
        }
        Ok(awakened)
    }

    #[allow(clippy::too_many_lines)] // Process, Effect and Provider outputs share one commit.
    fn poll_waiting_process(
        &self,
        process: ObjectId,
        process_view: &oms_runtime::ObjectView,
        mut state: ProcessState,
        effect: ObjectId,
    ) -> Result<bool, VmError> {
        let effect_view = self.manager.read(self.context, effect)?;
        if effect_view.header().type_id != CORE_EFFECT_TYPE {
            return Err(VmError::InvalidProcessState(String::from(
                "waiting Process references a non-Effect Object",
            )));
        }
        let record = EffectRecord::decode(effect_view.state()).map_err(VmError::from)?;
        if record.process != process || record.token_position != state.token_position {
            return Err(VmError::InvalidProcessState(String::from(
                "waiting Effect does not match its Process checkpoint",
            )));
        }
        if record.status != EffectStatus::Pending {
            return Ok(false);
        }
        let program_view = self.manager.read(self.context, state.program)?;
        let program = Program::decode(program_view.state())?;
        let Some(Token::ObjectCall { method, arguments }) =
            program.tokens.get(state.token_position as usize)
        else {
            return Err(VmError::InvalidProcessState(String::from(
                "waiting Process is not positioned on an ObjectCall",
            )));
        };
        if method != &record.capability || *arguments as usize != record.arguments.len() {
            return Err(VmError::InvalidProcessState(String::from(
                "waiting Process call does not match its Effect request",
            )));
        }
        let receiver_index = state
            .stack
            .len()
            .checked_sub(record.arguments.len().saturating_add(1))
            .ok_or(VmError::StackUnderflow)?;
        let target = native_object_id(&state.stack[receiver_index])?;
        if target != record.target
            || EffectRecord::redact_arguments(&state.stack[receiver_index + 1..])
                != record.arguments
        {
            return Err(VmError::InvalidProcessState(String::from(
                "waiting Process stack does not match its Effect request",
            )));
        }
        let target_view = self.manager.read(self.context, target)?;
        let provider = self.providers.get(target_view.header().type_id)?;
        let target_state = Value::decode(target_view.state())?;
        let Some(outcome) = provider.poll_for_process(
            process,
            target,
            &target_state,
            &record.capability,
            &record.arguments,
            effect,
        )?
        else {
            return Ok(false);
        };

        let completed = record.complete(outcome.result.clone());
        state.stack.truncate(receiver_index);
        state.stack.push(outcome.result);
        state.token_position = state
            .token_position
            .checked_add(1)
            .ok_or(VmError::TokenPositionOutOfRange(state.token_position))?;
        state.status = ProcessStatus::Ready;
        state.wait_reason = WaitReason::None;
        state.lease_owner = None;
        state.lease_deadline_unix_ms = None;

        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, process_view.header().version)
            .expect(effect, effect_view.header().version)
            .expect(target, target_view.header().version)
            .remove_link(process, "$effect")
            .update_state(process, encode_process_state(&state)?)
            .update_state(effect, completed.encode().map_err(VmError::from)?);
        if let Some(object_state) = outcome.object_state {
            transaction.update_state(target, object_state.encode()?);
        }
        for request in outcome.created {
            if let Some(parent) = request.parent {
                let parent_view = self.manager.read(self.context, parent)?;
                transaction.expect(parent, parent_view.header().version);
            }
            transaction.create(request);
        }
        self.manager.commit(transaction)?;
        Ok(true)
    }

    /// Makes Processes waiting on a terminal child Ready in one OMS commit.
    ///
    /// The child's terminal `ProcessState` and every awakened waiter are read
    /// and committed together, so a waiter is never made runnable while the
    /// child still appears active in the same observed version.
    ///
    /// # Errors
    ///
    /// Returns an OMS or Process-state decoding/encoding error.
    pub fn wake_process_waiters(&self, child: ObjectId) -> Result<Vec<ObjectId>, VmError> {
        let child_state = self.process_state(child)?;
        if !matches!(
            child_state.status,
            ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
        ) {
            return Ok(Vec::new());
        }

        let mut awakened = Vec::new();
        let mut transaction = self.manager.begin(self.context);
        let child_view = self.manager.read(self.context, child)?;
        transaction.expect(child, child_view.header().version);
        for header in self.manager.list(self.context)? {
            if header.type_id != PROCESS_TYPE || header.id == child {
                continue;
            }
            let view = self.manager.read(self.context, header.id)?;
            let mut state = decode_process_state(view.state())?;
            if state.status == ProcessStatus::Waiting
                && state.wait_reason == WaitReason::Process(child)
            {
                state.status = ProcessStatus::Ready;
                state.wait_reason = WaitReason::None;
                transaction
                    .expect(header.id, view.header().version)
                    .remove_link(header.id, "$waiting_on")
                    .remove_link(child, alloc::format!("$wait:{}", header.id))
                    .update_state(header.id, encode_process_state(&state)?);
                awakened.push(header.id);
            }
        }
        if !awakened.is_empty() {
            self.manager.commit(transaction)?;
        }
        Ok(awakened)
    }

    /// Creates a Program Object and its Ready Process atomically in the OMS.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding, validation, or the atomic OMS commit fails.
    pub fn create_process(&self, program: &Program) -> Result<ObjectId, VmError> {
        self.create_process_with_bindings(program, &BTreeMap::new())
    }

    /// Creates a Program and Ready Process with initial Object identity locals.
    ///
    /// This is the Native bootstrap equivalent of resolving stable system
    /// Objects before starting user code; values remain ordinary OMS Objects.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding, validation, or the atomic OMS commit fails.
    pub fn create_process_with_bindings(
        &self,
        program: &Program,
        bindings: &BTreeMap<String, ObjectId>,
    ) -> Result<ObjectId, VmError> {
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
                locals: bindings.clone(),
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
    /// published together with `ProcessState` through one OMS transaction.
    ///
    /// # Errors
    ///
    /// Returns an OMS, decoding, token execution, or provider error. A token
    /// failure is also persisted as the Process's terminal Failed state.
    #[allow(clippy::too_many_lines)] // Per-token staging and commit boundaries stay together.
    pub fn run_slice(
        &self,
        process: ObjectId,
        maximum_tokens: u64,
    ) -> Result<NativeRunReport, VmError> {
        let limit = maximum_tokens.clamp(1, 4096);
        let mut steps = 0;
        while steps < limit {
            let view = self.manager.read(self.context, process)?;
            if view.header().type_id != PROCESS_TYPE {
                return Err(VmError::TypeError("Object is not a Process"));
            }
            let mut state = decode_process_state(view.state())?;
            if matches!(
                state.status,
                ProcessStatus::Halted
                    | ProcessStatus::Waiting
                    | ProcessStatus::Suspended
                    | ProcessStatus::Terminated
            ) {
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
                providers: &self.providers,
                staged: &mut staged,
            };
            let before_token = state.clone();
            let next = state
                .token_position
                .checked_add(1)
                .ok_or(VmError::TokenPositionOutOfRange(state.token_position))?;
            let token_result = match execute_token(&mut host, &mut state, &program) {
                Ok(true) => Ok(()),
                Ok(false) => match program.tokens.get(
                    usize::try_from(state.token_position)
                        .map_err(|_| VmError::TokenPositionOutOfRange(state.token_position))?,
                ) {
                    Some(Token::SetField(field)) => {
                        host.set_field(&mut state, field)?;
                        state.token_position = state
                            .token_position
                            .checked_add(1)
                            .ok_or(VmError::TokenPositionOutOfRange(state.token_position))?;
                        Ok(())
                    }
                    Some(Token::ObjectCall { method, arguments }) => {
                        match host.process_wait(process, &mut state, method, *arguments, next) {
                            Err(error) => Err(error),
                            Ok(true) => Ok(()),
                            Ok(false) => {
                                match host.object_call(&mut state, method, *arguments, next) {
                                    Ok(true) => Ok(()),
                                    Ok(false) => match host.provider_call(
                                        process,
                                        view.header().version,
                                        &mut state,
                                        method,
                                        *arguments,
                                        next,
                                    ) {
                                        Ok(true) => Ok(()),
                                        Ok(false) => {
                                            Err(VmError::MissingProvider("Native token service"))
                                        }
                                        Err(error) => Err(error),
                                    },
                                    Err(error) => Err(error),
                                }
                            }
                        }
                    }
                    Some(Token::RegistryCall { method, arguments }) => {
                        host.registry_call(process, &mut state, method, *arguments, next)
                    }
                    Some(Token::BindCreated { name, arguments }) => host.bind_registry_result(
                        process,
                        &mut state,
                        name.clone(),
                        *arguments,
                        next,
                        true,
                    ),
                    Some(Token::BindFound { name, arguments }) => host.bind_registry_result(
                        process,
                        &mut state,
                        name.clone(),
                        *arguments,
                        next,
                        false,
                    ),
                    Some(Token::CallFunction { name, arguments }) => host.call_function(
                        &mut state,
                        program.tokens.as_slice(),
                        name,
                        *arguments,
                        next,
                    ),
                    _ => Err(VmError::MissingProvider("Native token service")),
                },
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
                failure.expect(
                    process,
                    staged.process_version.unwrap_or(view.header().version),
                );
                apply_staged(&mut failure, staged)?;
                failure.update_state(process, encode_process_state(&state)?);
                self.manager.commit(failure)?;
                self.providers.process_ended(process)?;
                return Err(error);
            }
            if state.status == ProcessStatus::Running {
                state.status = ProcessStatus::Ready;
            }
            let mut transaction = self.manager.begin(self.context);
            transaction.expect(
                process,
                staged.process_version.unwrap_or(view.header().version),
            );
            apply_staged(&mut transaction, staged)?;
            transaction.update_state(process, encode_process_state(&state)?);
            self.manager.commit(transaction)?;
            if matches!(
                state.status,
                ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
            ) {
                self.providers.process_ended(process)?;
            }
            steps += 1;
            if matches!(
                state.status,
                ProcessStatus::Halted
                    | ProcessStatus::Waiting
                    | ProcessStatus::Suspended
                    | ProcessStatus::Terminated
            ) {
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
    process_version: Option<oms_types::ObjectVersion>,
    updates: BTreeMap<ObjectId, Vec<u8>>,
    versions: BTreeMap<ObjectId, oms_types::ObjectVersion>,
    expectations: BTreeMap<ObjectId, oms_types::ObjectVersion>,
    creates: Vec<CreateObject>,
    values: BTreeMap<ObjectId, Value>,
    links: Vec<StagedLink>,
}

struct StagedLink {
    source: ObjectId,
    name: String,
    target: Option<ObjectId>,
}

struct NativeTokenHost<'a> {
    manager: &'a InMemoryObjectManager,
    context: AccessContext,
    providers: &'a ProviderRegistry,
    staged: &'a mut StagedWrites,
}

fn apply_staged(transaction: &mut Transaction, staged: StagedWrites) -> Result<(), VmError> {
    for (object, encoded) in staged.updates {
        let version = staged.versions.get(&object).copied().ok_or_else(|| {
            VmError::InvalidProcessState(String::from("staged Object update has no version"))
        })?;
        transaction
            .expect(object, version)
            .update_state(object, encoded);
    }
    for request in staged.creates {
        transaction.create(request);
    }
    for (object, version) in staged.expectations {
        transaction.expect(object, version);
    }
    for change in staged.links {
        match change.target {
            Some(target) => {
                transaction.set_link(change.source, change.name, target);
            }
            None => {
                transaction.remove_link(change.source, change.name);
            }
        }
    }
    Ok(())
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
        receiver: &Value,
        field: &str,
        _program: &Program,
    ) -> Result<Value, VmError> {
        if let Value::Map(fields) | Value::Record(fields) = receiver {
            return fields
                .get(field)
                .cloned()
                .ok_or_else(|| VmError::MissingKey(field.to_string()));
        }

        let object = native_object_id(receiver)?;
        let view = self.manager.read(self.context, object)?;
        if let Ok(Value::Map(fields) | Value::Record(fields)) = Value::decode(view.state()) {
            if let Some(value) = fields.get(field) {
                return Ok(value.clone());
            }
        }
        let property = match field {
            "id" => Some(Value::Text(object.to_string())),
            "type" => Some(Value::Text(
                self.manager.type_by_id(view.header().type_id)?.name,
            )),
            "parent" => Some(
                view.header()
                    .parent_id
                    .map_or(Value::Null, |parent| Value::Text(parent.to_string())),
            ),
            "version" => Some(Value::Text(view.header().version.get().to_string())),
            "owner" => Some(Value::Text(view.owner().to_string())),
            "children" => Some(Value::Array(
                view.children()
                    .iter()
                    .map(|child| Value::Text(child.to_string()))
                    .collect(),
            )),
            "links" => Some(Value::Map(
                view.links()
                    .iter()
                    .map(|(name, target)| (name.clone(), Value::Text(target.to_string())))
                    .collect(),
            )),
            "value" => Some(self.manager.value(self.context, object)?),
            _ => None,
        };
        property.ok_or_else(|| VmError::MissingKey(field.to_string()))
    }
}

impl NativeTokenHost<'_> {
    #[allow(clippy::too_many_lines)] // Argument Objects and the new frame commit with this token.
    fn call_function(
        &mut self,
        state: &mut ProcessState,
        tokens: &[Token],
        name: &str,
        arguments: u32,
        next: u32,
    ) -> Result<(), VmError> {
        let argument_count = usize::try_from(arguments).map_err(|_| VmError::StackUnderflow)?;
        let stack_base = state
            .stack
            .len()
            .checked_sub(argument_count)
            .ok_or(VmError::StackUnderflow)?;
        let definition =
            tokens
                .iter()
                .enumerate()
                .rev()
                .find_map(|(position, token)| match token {
                    Token::DefineFunction {
                        name: candidate,
                        parameters,
                        ..
                    } if candidate == name && parameters.len() == argument_count => {
                        Some((position, parameters.clone()))
                    }
                    _ => None,
                });
        let Some((position, parameters)) = definition else {
            return Err(VmError::TypeError("unknown function or argument count"));
        };
        let entry =
            u32::try_from(position + 1).map_err(|_| VmError::TokenPositionOutOfRange(u32::MAX))?;
        let arguments = state.stack.split_off(stack_base);
        let mut locals = BTreeMap::new();
        for (parameter, value) in parameters.into_iter().zip(arguments) {
            let request = CreateObject::new(CORE_VALUE_TYPE, value.encode()?);
            let object = request.id;
            self.staged.creates.push(request);
            self.staged.values.insert(object, value);
            locals.insert(parameter, object);
        }
        state.frames.push(CallFrame {
            return_position: next,
            stack_base: u32::try_from(stack_base)
                .map_err(|_| VmError::InvalidProcessState(String::from("stack is too large")))?,
            locals,
            receiver: None,
            class: None,
        });
        state.token_position = entry;
        Ok(())
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn provider_call(
        &mut self,
        process: ObjectId,
        process_version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        capability: &str,
        arguments: u32,
        next: u32,
    ) -> Result<bool, VmError> {
        let argument_count = usize::try_from(arguments).map_err(|_| VmError::StackUnderflow)?;
        let receiver_index = state
            .stack
            .len()
            .checked_sub(argument_count.saturating_add(1))
            .ok_or(VmError::StackUnderflow)?;
        let receiver = state.stack[receiver_index].clone();
        let object = native_object_id(&receiver)?;
        let target = self.manager.read(self.context, object)?;
        let provider = match self.providers.get(target.header().type_id) {
            Ok(provider) => provider,
            Err(ProviderError::Missing(_)) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if !provider.capabilities().contains(capability) {
            return Err(ProviderError::UnsupportedCapability(capability.to_owned()).into());
        }
        self.manager
            .require_capability(self.context, object, Capability::Invoke)?;
        let arguments_start = receiver_index + 1;
        let call_arguments = state.stack[arguments_start..].to_vec();

        if provider.ephemeral_capabilities().contains(capability) {
            let target_value = Value::decode(target.state())?;
            let outcome = provider.invoke_ephemeral_for_process(
                process,
                object,
                &target_value,
                capability,
                &call_arguments,
            )?;
            if outcome.object_state.is_some() || !outcome.created.is_empty() {
                return Err(VmError::InvalidProcessState(String::from(
                    "ephemeral Provider returned persistent Object changes",
                )));
            }
            state.stack.truncate(receiver_index);
            state.stack.push(outcome.result);
            state.token_position = next;
            return Ok(true);
        }

        let process_view = self.manager.read(self.context, process)?;
        let mut effect = process_view.links().get("$effect").copied();
        let mut record = if let Some(effect_id) = effect {
            let effect_view = self.manager.read(self.context, effect_id)?;
            if effect_view.header().type_id != CORE_EFFECT_TYPE {
                return Err(VmError::InvalidProcessState(String::from(
                    "Process $effect link does not reference a core.effect Object",
                )));
            }
            let record = EffectRecord::decode(effect_view.state())?;
            if record.process != process
                || record.target != object
                || record.token_position != state.token_position
                || record.capability != capability
                || record.arguments != EffectRecord::redact_arguments(&call_arguments)
            {
                return Err(VmError::InvalidProcessState(String::from(
                    "pending Effect does not match the Native Process token",
                )));
            }
            record
        } else {
            let effect_id = ObjectId::new();
            let record = EffectRecord::pending_with_policy(
                process,
                object,
                state.token_position,
                capability,
                call_arguments.clone(),
                provider.effect_recovery_policy(capability),
            );
            let request = record
                .create_object(effect_id)?
                .with_grant(state.subject, Capability::Inspect)
                .with_grant(state.subject, Capability::ViewValue)
                .with_grant(state.subject, Capability::Invoke);
            let mut intent = self.manager.begin(self.context);
            intent
                .expect(process, process_version)
                .expect(object, target.header().version)
                .create(request)
                .set_link(process, "$effect", effect_id);
            self.manager.commit(intent)?;
            self.staged.process_version =
                Some(self.manager.inspect(self.context, process)?.version);
            effect = Some(effect_id);
            record
        };
        let effect = effect.ok_or_else(|| {
            VmError::InvalidProcessState(String::from("Native Effect intent has no identity"))
        })?;
        let mut effect_view = self.manager.read(self.context, effect)?;

        match record.status {
            EffectStatus::Completed => {
                state.stack.truncate(receiver_index);
                state
                    .stack
                    .push(record.result.take().unwrap_or(Value::Null));
                state.token_position = next;
                self.staged
                    .expectations
                    .insert(effect, effect_view.header().version);
                self.staged.links.push(StagedLink {
                    source: process,
                    name: String::from("$effect"),
                    target: None,
                });
                return Ok(true);
            }
            EffectStatus::Failed => {
                self.staged.links.push(StagedLink {
                    source: process,
                    name: String::from("$effect"),
                    target: None,
                });
                return Err(VmError::Provider(
                    record
                        .error
                        .unwrap_or_else(|| String::from("Provider operation failed")),
                ));
            }
            EffectStatus::Unknown => {
                self.staged.links.push(StagedLink {
                    source: process,
                    name: String::from("$effect"),
                    target: None,
                });
                return Err(VmError::Provider(String::from(
                    "Effect outcome is unknown and must be resolved by local",
                )));
            }
            EffectStatus::Running => {
                if record.recovery_policy == EffectRecoveryPolicy::RetryIdempotent {
                    record = record.retry();
                } else {
                    record = record.unknown();
                    self.stage_effect(effect, &effect_view, &record)?;
                    self.staged.links.push(StagedLink {
                        source: process,
                        name: String::from("$effect"),
                        target: None,
                    });
                    return Err(VmError::Provider(String::from(
                        "Effect outcome is unknown; Native Provider was not retried",
                    )));
                }
            }
            EffectStatus::Pending => {}
        }
        record = record.running();
        let mut running = self.manager.begin(self.context);
        running
            .expect(effect, effect_view.header().version)
            .update_state(effect, record.encode()?);
        self.manager.commit(running)?;
        effect_view = self.manager.read(self.context, effect)?;

        let target_value = Value::decode(target.state())?;
        match provider.invoke_for_process(
            process,
            object,
            &target_value,
            capability,
            &call_arguments,
            effect,
        ) {
            Ok(outcome) => {
                record = record.complete(outcome.result.clone());
                self.stage_effect(effect, &effect_view, &record)?;
                if let Some(object_state) = outcome.object_state {
                    self.staged
                        .expectations
                        .insert(object, target.header().version);
                    self.staged.updates.insert(object, object_state.encode()?);
                    self.staged.versions.insert(object, target.header().version);
                }
                for request in outcome.created {
                    if let Some(parent) = request.parent {
                        let parent_version = self.manager.inspect(self.context, parent)?.version;
                        self.staged.expectations.insert(parent, parent_version);
                    }
                    self.staged.creates.push(request);
                }
                self.staged.links.push(StagedLink {
                    source: process,
                    name: String::from("$effect"),
                    target: None,
                });
                state.stack.truncate(receiver_index);
                state.stack.push(outcome.result);
                state.token_position = next;
                state.wait_reason = WaitReason::None;
                Ok(true)
            }
            Err(ProviderError::Pending) => {
                record = record.retry();
                self.stage_effect(effect, &effect_view, &record)?;
                state.status = ProcessStatus::Waiting;
                state.wait_reason = if target.header().type_id == oms_types::CORE_TERMINAL_TYPE
                    && matches!(capability, "read_line" | "read_secret")
                {
                    WaitReason::Input(effect)
                } else {
                    WaitReason::Effect(effect)
                };
                Ok(true)
            }
            Err(error) => {
                record = record.fail("Provider operation failed");
                self.stage_effect(effect, &effect_view, &record)?;
                self.staged.links.push(StagedLink {
                    source: process,
                    name: String::from("$effect"),
                    target: None,
                });
                Err(error.into())
            }
        }
    }

    fn stage_effect(
        &mut self,
        effect: ObjectId,
        view: &oms_runtime::ObjectView,
        record: &EffectRecord,
    ) -> Result<(), VmError> {
        self.staged.versions.insert(effect, view.header().version);
        self.staged.updates.insert(effect, record.encode()?);
        Ok(())
    }

    fn registry_call(
        &mut self,
        process: ObjectId,
        state: &mut ProcessState,
        method: &str,
        arguments: u32,
        next: u32,
    ) -> Result<(), VmError> {
        let count = usize::try_from(arguments).map_err(|_| VmError::StackUnderflow)?;
        let start = state
            .stack
            .len()
            .checked_sub(count)
            .ok_or(VmError::StackUnderflow)?;
        let values = state.stack[start..].to_vec();
        let result = match (method, values.as_slice()) {
            ("create", [Value::Text(type_name), value]) => {
                self.create_registry_object(process, type_name, value, process)?
            }
            ("create", [Value::Text(type_name), value, parent]) => {
                let parent = native_object_id(parent)?;
                self.create_registry_object(process, type_name, value, parent)?
            }
            ("find", [Value::Text(identity)]) => {
                let object = if let Ok(object) = identity.parse::<ObjectId>() {
                    object
                } else {
                    self.manager
                        .read(self.context, process)?
                        .links()
                        .get(identity)
                        .copied()
                        .ok_or_else(|| VmError::MissingKey(identity.clone()))?
                };
                self.manager.inspect(self.context, object)?;
                Value::Text(object.to_string())
            }
            ("query", [Value::Text(type_name)] | [Value::Text(type_name), Value::Null]) => {
                let type_id = self.manager.type_by_name(type_name)?.id;
                let headers = self
                    .manager
                    .query(self.context, &ObjectQuery::new().with_type(type_id))?;
                Value::Array(
                    headers
                        .into_iter()
                        .map(|header| Value::Text(header.id.to_string()))
                        .collect(),
                )
            }
            ("query", [Value::Text(type_name), Value::Text(capability)]) => {
                let type_id = self.manager.type_by_name(type_name)?.id;
                let query = ObjectQuery::new()
                    .with_type(type_id)
                    .with_domain_capability(capability.clone());
                let headers = self.manager.query(self.context, &query)?;
                Value::Array(
                    headers
                        .into_iter()
                        .map(|header| Value::Text(header.id.to_string()))
                        .collect(),
                )
            }
            _ => return Err(VmError::TypeError("invalid object registry call")),
        };
        state.stack.truncate(start);
        state.stack.push(result);
        state.token_position = next;
        Ok(())
    }

    fn bind_registry_result(
        &mut self,
        process: ObjectId,
        state: &mut ProcessState,
        name: String,
        arguments: u32,
        next: u32,
        create: bool,
    ) -> Result<(), VmError> {
        let count = usize::try_from(arguments).map_err(|_| VmError::StackUnderflow)?;
        let start = state
            .stack
            .len()
            .checked_sub(count)
            .ok_or(VmError::StackUnderflow)?;
        let values = state.stack[start..].to_vec();
        let result = if create {
            match values.as_slice() {
                [Value::Text(type_name), value] => {
                    self.create_registry_object(process, type_name, value, process)?
                }
                [Value::Text(type_name), value, parent] => self.create_registry_object(
                    process,
                    type_name,
                    value,
                    native_object_id(parent)?,
                )?,
                _ => return Err(VmError::TypeError("invalid object.create arguments")),
            }
        } else {
            match values.as_slice() {
                [Value::Text(identity)] => {
                    let object = if let Ok(object) = identity.parse::<ObjectId>() {
                        object
                    } else {
                        self.manager
                            .read(self.context, process)?
                            .links()
                            .get(identity)
                            .copied()
                            .ok_or_else(|| VmError::MissingKey(identity.clone()))?
                    };
                    self.manager.inspect(self.context, object)?;
                    Value::Text(object.to_string())
                }
                _ => return Err(VmError::TypeError("invalid object.find arguments")),
            }
        };
        let object = native_object_id(&result)?;
        if let Some(frame) = state.frames.last_mut() {
            frame.locals.insert(name, object);
        } else {
            state.variables.insert(name, object);
        }
        // Bind directly to the managed Object, matching Hosted `bind_name`.
        state.stack.truncate(start);
        state.token_position = next;
        Ok(())
    }

    fn create_registry_object(
        &mut self,
        _process: ObjectId,
        type_name: &str,
        value: &Value,
        parent: ObjectId,
    ) -> Result<Value, VmError> {
        let request = self
            .manager
            .prepare_create(CreateSpec::new(type_name, value.clone()).with_parent(parent))?;
        let parent_version = self.manager.inspect(self.context, parent)?.version;
        self.staged.expectations.insert(parent, parent_version);
        let identity = request.id;
        self.staged.creates.push(request);
        // Keep the identity as a Value Object in the process frame, just like
        // every other local; no Native-only binding representation is added.
        Ok(Value::Text(identity.to_string()))
    }

    #[allow(clippy::too_many_lines)] // Keep Native OMS reads and staged Object mutations together.
    fn object_call(
        &mut self,
        state: &mut ProcessState,
        method: &str,
        arguments: u32,
        next: u32,
    ) -> Result<bool, VmError> {
        let count = usize::try_from(arguments).map_err(|_| VmError::StackUnderflow)?;
        let receiver_index = state
            .stack
            .len()
            .checked_sub(count.saturating_add(1))
            .ok_or(VmError::StackUnderflow)?;
        let receiver = state.stack[receiver_index].clone();
        let object = native_object_id(&receiver)?;
        let view = self.manager.read(self.context, object)?;
        let args = state.stack[receiver_index + 1..].to_vec();
        let result = match (method, args.as_slice()) {
            ("id", []) => Value::Text(object.to_string()),
            ("type", []) => Value::Text(self.manager.type_by_id(view.header().type_id)?.name),
            ("parent", []) => view
                .header()
                .parent_id
                .map_or(Value::Null, |id| Value::Text(id.to_string())),
            ("value", []) => self.manager.value(self.context, object)?,
            ("children", []) => Value::Array(
                view.children()
                    .iter()
                    .map(|id| Value::Text(id.to_string()))
                    .collect(),
            ),
            ("links", []) => Value::Map(
                view.links()
                    .iter()
                    .map(|(name, target)| (name.clone(), Value::Text(target.to_string())))
                    .collect(),
            ),
            ("inspect", []) => Value::Record(BTreeMap::from([
                ("id".into(), Value::Text(object.to_string())),
                (
                    "type".into(),
                    Value::Text(self.manager.type_by_id(view.header().type_id)?.name),
                ),
                (
                    "parent".into(),
                    view.header()
                        .parent_id
                        .map_or(Value::Null, |id| Value::Text(id.to_string())),
                ),
                (
                    "version".into(),
                    Value::Text(view.header().version.get().to_string()),
                ),
            ])),
            ("replace", [replacement]) => {
                self.manager
                    .require_capability(self.context, object, Capability::ReplaceValue)?;
                let (version, encoded) =
                    self.manager
                        .prepare_replace_value(self.context, object, replacement)?;
                self.staged.versions.insert(object, version);
                self.staged.updates.insert(object, encoded);
                self.staged.values.insert(object, replacement.clone());
                receiver.clone()
            }
            ("link", [Value::Text(name), target]) => {
                self.manager
                    .require_capability(self.context, object, Capability::Link)?;
                let target = native_object_id(target)?;
                let target_view = self.manager.read(self.context, target)?;
                self.staged
                    .expectations
                    .insert(object, view.header().version);
                self.staged
                    .expectations
                    .insert(target, target_view.header().version);
                self.staged.links.push(StagedLink {
                    source: object,
                    name: name.clone(),
                    target: Some(target),
                });
                receiver.clone()
            }
            ("unlink", [Value::Text(name)]) => {
                self.manager
                    .require_capability(self.context, object, Capability::Link)?;
                self.staged
                    .expectations
                    .insert(object, view.header().version);
                self.staged.links.push(StagedLink {
                    source: object,
                    name: name.clone(),
                    target: None,
                });
                receiver.clone()
            }
            ("id" | "type" | "parent" | "value" | "children" | "links" | "inspect", _) => {
                return Err(VmError::TypeError(
                    "Object inspection method expects no arguments",
                ));
            }
            ("replace", _) => return Err(VmError::TypeError("Object.replace expects one value")),
            ("link", _) => {
                return Err(VmError::TypeError(
                    "Object.link expects a name and Object ID",
                ));
            }
            ("unlink", _) => return Err(VmError::TypeError("Object.unlink expects a link name")),
            _ => return Ok(false),
        };
        state.stack.truncate(receiver_index);
        state.stack.push(result);
        state.token_position = next;
        Ok(true)
    }

    fn process_wait(
        &mut self,
        current_process: ObjectId,
        state: &mut ProcessState,
        method: &str,
        arguments: u32,
        next: u32,
    ) -> Result<bool, VmError> {
        if method != "wait" {
            return Ok(false);
        }
        let argument_count = usize::try_from(arguments).map_err(|_| VmError::StackUnderflow)?;
        if argument_count != 0 {
            return Err(VmError::TypeError("process.wait expects no arguments"));
        }
        let receiver_index = state
            .stack
            .len()
            .checked_sub(1)
            .ok_or(VmError::StackUnderflow)?;
        let receiver = state.stack[receiver_index].clone();
        let child = native_object_id(&receiver)?;
        if child == current_process
            || self.manager.inspect(self.context, child)?.type_id != PROCESS_TYPE
        {
            return Ok(false);
        }
        self.manager
            .require_capability(self.context, child, Capability::Invoke)?;
        let child_view = self.manager.read(self.context, child)?;
        let child_state = decode_process_state(child_view.state())?;
        if matches!(
            child_state.status,
            ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
        ) {
            state.stack.pop();
            state.stack.push(Value::Text(
                native_process_status_name(child_state.status).into(),
            ));
            state.token_position = next;
        } else if child_state.status == ProcessStatus::Waiting
            && matches!(
                child_state.wait_reason,
                WaitReason::Ipc(_) | WaitReason::Input(_)
            )
        {
            self.staged
                .expectations
                .insert(child, child_view.header().version);
            self.staged.links.extend([
                StagedLink {
                    source: current_process,
                    name: String::from("$waiting_on"),
                    target: None,
                },
                StagedLink {
                    source: child,
                    name: alloc::format!("$wait:{current_process}"),
                    target: None,
                },
            ]);
            state.stack.pop();
            state.stack.push(Value::Text("suspended".into()));
            state.token_position = next;
        } else {
            // Keep the receiver and instruction position so the shared Process
            // call can retry and obtain the child's eventual terminal status.
            state.status = ProcessStatus::Waiting;
            state.wait_reason = WaitReason::Process(child);
            state.ended_at_unix_ms = None;
            self.staged
                .expectations
                .insert(child, child_view.header().version);
            self.staged.links.extend([
                StagedLink {
                    source: current_process,
                    name: String::from("$waiting_on"),
                    target: Some(child),
                },
                StagedLink {
                    source: child,
                    name: alloc::format!("$wait:{current_process}"),
                    target: Some(current_process),
                },
            ]);
        }
        Ok(true)
    }

    fn set_field(&mut self, state: &mut ProcessState, field: &str) -> Result<(), VmError> {
        let value = state.stack.pop().ok_or(VmError::StackUnderflow)?;
        let receiver = state.stack.pop().ok_or(VmError::StackUnderflow)?;
        let object = native_object_id(&receiver)?;
        self.manager
            .require_capability(self.context, object, Capability::ReplaceValue)?;
        let view = self.manager.read(self.context, object)?;
        let current = self
            .staged
            .values
            .get(&object)
            .cloned()
            .map_or_else(|| Value::decode(view.state()), Ok)?;
        let (Value::Map(mut fields) | Value::Record(mut fields)) = current else {
            return Err(VmError::TypeError("Object value has no fields"));
        };
        if !fields.contains_key(field) {
            return Err(VmError::MissingKey(field.to_string()));
        }
        fields.insert(field.to_string(), value);
        let replacement = Value::Record(fields);
        let (version, encoded) =
            self.manager
                .prepare_replace_value(self.context, object, &replacement)?;
        self.staged.versions.insert(object, version);
        self.staged.updates.insert(object, encoded);
        self.staged.values.insert(object, replacement);
        Ok(())
    }
}

const fn native_process_status_name(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Ready => "ready",
        ProcessStatus::Running => "running",
        ProcessStatus::Waiting => "waiting",
        ProcessStatus::Suspended => "suspended",
        ProcessStatus::Halted => "halted",
        ProcessStatus::Terminated => "terminated",
        ProcessStatus::Failed => "failed",
    }
}

fn native_object_id(value: &Value) -> Result<ObjectId, VmError> {
    match value {
        Value::Text(id) => id
            .parse()
            .map_err(|_| VmError::TypeError("invalid hexadecimal ObjectId")),
        _ => Err(VmError::TypeError("expected hexadecimal ObjectId text")),
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::collections::BTreeSet;
    use alloc::rc::Rc;
    use alloc::string::{String, ToString};
    use alloc::sync::Arc;
    use alloc::vec;
    use oms_runtime::{AccessContext, CreateObject, CreateSpec, InMemoryObjectManager};
    use oms_types::{CORE_TERMINAL_TYPE, ObjectId, SYSTEM_SUBJECT, Value, seed_id_generator};
    use ousject_provider::{ObjectProvider, ProviderError, ProviderOutcome};
    use tf_format::{Program, Token};

    use super::{NativeVirtualMachine, ProcessStatus, encode_process_state};
    use crate::native_scheduler::NativeCooperativeScheduler;

    #[derive(Debug)]
    struct PendingInputProvider;

    impl ObjectProvider for PendingInputProvider {
        fn type_id(&self) -> oms_types::TypeId {
            CORE_TERMINAL_TYPE
        }

        fn create(&self, initial: &Value) -> Result<Value, ProviderError> {
            Ok(initial.clone())
        }

        fn invoke(
            &self,
            _object: ObjectId,
            _state: &Value,
            _capability: &str,
            _arguments: &[Value],
            _effect: ObjectId,
        ) -> Result<ProviderOutcome, ProviderError> {
            Err(ProviderError::Pending)
        }

        fn poll_for_process(
            &self,
            _process: ObjectId,
            _object: ObjectId,
            _state: &Value,
            _capability: &str,
            _arguments: &[Value],
            _effect: ObjectId,
        ) -> Result<Option<ProviderOutcome>, ProviderError> {
            Ok(Some(ProviderOutcome::result(Value::Text(String::from(
                "typed input",
            )))))
        }

        fn capabilities(&self) -> BTreeSet<String> {
            ["read_line"].into_iter().map(String::from).collect()
        }
    }

    #[test]
    fn terminal_provider_poll_resumes_waiting_process_atomically() {
        let _ = seed_id_generator(0x494e_5055_5457_4149);
        let manager = Rc::new(InMemoryObjectManager::new(1).expect("create OMS"));
        let vm = NativeVirtualMachine::new(manager.clone());
        vm.register_provider(Arc::new(PendingInputProvider))
            .expect("register input provider");
        let context = AccessContext::new(SYSTEM_SUBJECT);
        let mut request = CreateObject::new(
            CORE_TERMINAL_TYPE,
            Value::Record(BTreeMap::new())
                .encode()
                .expect("encode terminal"),
        );
        request.capabilities = manager
            .type_by_id(CORE_TERMINAL_TYPE)
            .expect("terminal type")
            .capabilities;
        let terminal = request.id;
        let mut transaction = manager.begin(context);
        transaction.create(request);
        manager.commit(transaction).expect("publish terminal");
        let process = vm
            .create_process(&Program {
                tokens: vec![
                    Token::Push(Value::Text(terminal.to_string())),
                    Token::ObjectCall {
                        method: String::from("read_line"),
                        arguments: 0,
                    },
                    Token::Halt,
                ],
            })
            .expect("create reader process");
        let mut scheduler = NativeCooperativeScheduler::new(&vm);
        scheduler.enqueue(process).expect("enqueue reader");
        let report = scheduler.run(8).expect("yield for input");
        assert_eq!(report.statuses[&process], ProcessStatus::Waiting);
        assert_eq!(scheduler.poll_waiting_providers().expect("poll input"), 1);
        let resumed = vm.process_state(process).expect("read resumed state");
        assert_eq!(resumed.status, ProcessStatus::Ready);
        assert_eq!(resumed.token_position, 2);
        assert_eq!(
            resumed.stack.last(),
            Some(&Value::Text(String::from("typed input")))
        );
        assert!(
            !manager
                .read(context, process)
                .expect("read process links")
                .links()
                .contains_key("$effect")
        );
        assert_eq!(
            scheduler.run(8).expect("halt after input").statuses[&process],
            ProcessStatus::Halted
        );
    }

    #[test]
    fn scheduler_recovers_interrupted_process_at_its_last_committed_token() {
        let _ = seed_id_generator(0x4e41_5449_5645_5253);
        let manager = Rc::new(InMemoryObjectManager::new(1).expect("create OMS"));
        let vm = NativeVirtualMachine::new(manager.clone());
        let process = vm
            .create_process(&Program {
                tokens: vec![
                    Token::Push(Value::Integer(5)),
                    Token::Store("committed".into()),
                    Token::Push(Value::Integer(7)),
                    Token::Halt,
                ],
            })
            .expect("create process");

        let first = vm.run_slice(process, 2).expect("commit first token slice");
        assert_eq!(first.status, ProcessStatus::Ready);
        let committed = vm.process_state(process).expect("read committed process");
        assert_eq!(committed.token_position, 2);
        assert!(committed.frames[0].locals.contains_key("committed"));

        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = manager.read(system, process).expect("read process object");
        let mut interrupted = committed;
        interrupted.status = ProcessStatus::Running;
        let mut transaction = manager.begin(system);
        transaction
            .expect(process, view.header().version)
            .update_state(
                process,
                encode_process_state(&interrupted).expect("encode state"),
            );
        manager
            .commit(transaction)
            .expect("persist interrupted state");

        let mut scheduler = NativeCooperativeScheduler::new(&vm);
        assert_eq!(
            scheduler.recover_ready_processes().expect("recover queue"),
            1
        );
        let recovered = vm.process_state(process).expect("read recovered state");
        assert_eq!(recovered.status, ProcessStatus::Ready);
        assert_eq!(recovered.token_position, 2);
        assert!(recovered.frames[0].locals.contains_key("committed"));

        let report = scheduler.run(8).expect("resume process");
        assert_eq!(report.statuses[&process], ProcessStatus::Halted);
        assert_eq!(
            vm.process_state(process)
                .expect("read finished state")
                .result,
            Some(Value::Integer(7))
        );
    }

    #[test]
    fn native_vm_reads_and_atomically_replaces_persistent_object_fields() {
        let _ = seed_id_generator(0x4e41_5449_5645_4649);
        let manager = Rc::new(InMemoryObjectManager::new(1).expect("create OMS"));
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let object = manager
            .create_object(
                system,
                CreateSpec::new(
                    "core.value",
                    Value::Record(BTreeMap::from([("count".into(), Value::Integer(1))])),
                ),
            )
            .expect("create object");
        let vm = NativeVirtualMachine::new(manager.clone());
        let process = vm
            .create_process(&Program {
                tokens: vec![
                    Token::Push(Value::Text(object.to_string())),
                    Token::Push(Value::Integer(9)),
                    Token::SetField("count".into()),
                    Token::Push(Value::Text(object.to_string())),
                    Token::GetField("count".into()),
                    Token::Halt,
                ],
            })
            .expect("create process");

        let report = vm.run_slice(process, 16).expect("run field program");
        assert_eq!(report.status, ProcessStatus::Halted);
        assert_eq!(
            manager.value(system, object).expect("read updated object"),
            Value::Record(BTreeMap::from([("count".into(), Value::Integer(9))]))
        );
        assert_eq!(
            vm.process_state(process).expect("read process").result,
            Some(Value::Integer(9))
        );
    }

    #[test]
    fn native_vm_runs_object_registry_and_base_operations_in_process_commit() {
        let _ = seed_id_generator(0x4e41_5449_5645_4f42);
        let manager = Rc::new(InMemoryObjectManager::new(1).expect("create OMS"));
        let vm = NativeVirtualMachine::new(manager.clone());
        let process = vm
            .create_process(&Program {
                tokens: vec![
                    Token::Push(Value::Text(String::from("core.value"))),
                    Token::Push(Value::Record(BTreeMap::from([(
                        String::from("count"),
                        Value::Integer(1),
                    )]))),
                    Token::BindCreated {
                        name: String::from("item"),
                        arguments: 2,
                    },
                    Token::LoadIdentity(String::from("item")),
                    Token::Push(Value::Record(BTreeMap::from([(
                        String::from("count"),
                        Value::Integer(9),
                    )]))),
                    Token::ObjectCall {
                        method: String::from("replace"),
                        arguments: 1,
                    },
                    Token::Pop,
                    Token::LoadIdentity(String::from("item")),
                    Token::GetField(String::from("count")),
                    Token::Halt,
                ],
            })
            .expect("create object API process");

        let report = vm.run_slice(process, 32).expect("run object API program");
        assert_eq!(report.status, ProcessStatus::Halted);
        let state = vm.process_state(process).expect("read process state");
        assert_eq!(state.result, Some(Value::Integer(9)));
        let object = state.frames[0].locals["item"];
        assert_eq!(
            manager
                .value(AccessContext::new(SYSTEM_SUBJECT), object)
                .expect("read created object"),
            Value::Record(BTreeMap::from([(String::from("count"), Value::Integer(9))]))
        );
    }

    #[test]
    fn native_vm_calls_praxis_functions_with_value_object_locals() {
        let _ = seed_id_generator(0x4e41_5449_5645_4655);
        let manager = Rc::new(InMemoryObjectManager::new(1).expect("create OMS"));
        let vm = NativeVirtualMachine::new(manager);
        let process = vm
            .create_process(&Program {
                tokens: vec![
                    Token::Push(Value::Integer(21)),
                    Token::CallFunction {
                        name: String::from("twice"),
                        arguments: 1,
                    },
                    Token::Halt,
                    Token::DefineFunction {
                        name: String::from("twice"),
                        parameters: vec![String::from("value")],
                        end: 8,
                    },
                    Token::Load(String::from("value")),
                    Token::Push(Value::Integer(2)),
                    Token::Multiply,
                    Token::Return,
                    Token::Halt,
                ],
            })
            .expect("create function process");
        let report = vm.run_slice(process, 16).expect("run function process");
        assert_eq!(report.status, ProcessStatus::Halted);
        assert_eq!(
            vm.process_state(process).expect("read result").result,
            Some(Value::Integer(42))
        );
    }
}

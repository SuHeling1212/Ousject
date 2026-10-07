#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Default)]
struct PendingWrites {
    values: BTreeMap<ObjectId, Value>,
    encoded_values: BTreeMap<ObjectId, Vec<u8>>,
    expected_versions: BTreeMap<ObjectId, ObjectVersion>,
    creates: BTreeMap<ObjectId, CreateObject>,
}

impl PendingWrites {
    fn is_empty(&self) -> bool {
        self.encoded_values.is_empty()
    }
}

impl VirtualMachine {
    pub(super) fn step(
        &self,
        process: ObjectId,
        mut state: ProcessState,
        instruction_limit: u64,
        lease: WorkerLease,
    ) -> Result<(Option<String>, u64), VmError> {
        const MAX_SLICE_INSTRUCTIONS: u64 = 4096;
        const MAX_SLICE_TIME: Duration = Duration::from_millis(20);

        let process_view = self.manager.read(self.context, process)?;
        if state.status != ProcessStatus::Running
            || state.lease_owner != Some(lease.owner)
            || state.lease_generation != lease.generation
            || state
                .lease_deadline_unix_ms
                .is_none_or(|deadline| deadline <= unix_time_millis())
        {
            return Err(VmError::WorkerLeaseExpired(process));
        }
        let mut process_version = process_view.header().version;
        let program = self.program(state.program)?;
        let instruction_limit = instruction_limit.clamp(1, MAX_SLICE_INSTRUCTIONS);
        let started = Instant::now();
        let mut executed = 0_u64;
        let mut pending = PendingWrites::default();

        loop {
            if executed >= instruction_limit
                || (executed > 0 && started.elapsed() >= MAX_SLICE_TIME)
            {
                self.flush_pending(process, process_version, &state, &mut pending)?;
                return Ok((None, executed));
            }

            match self.execute_pure_step(process, &mut state, &program, &mut pending) {
                Ok(true) => executed += 1,
                Ok(false) => {
                    if !pending.is_empty() {
                        process_version =
                            self.flush_pending(process, process_version, &state, &mut pending)?;
                    }
                    let state_before_barrier = state.clone();
                    match self.execute_step(process, state, process_version) {
                        Ok(output) => return Ok((output, executed.saturating_add(1))),
                        Err(error) => {
                            let current = self.manager.read(self.context, process)?;
                            if current.header().version != process_version
                                && current.links().contains_key("$effect")
                            {
                                // Provider intent commits deliberately advance
                                // the Process before calling the external
                                // operation. If completion persistence fails,
                                // preserve that pending intent and report the
                                // original failure for an idempotent retry.
                                return Err(error);
                            }
                            if current.header().version != process_version {
                                let committed_state = decode_process_state(current.state())?;
                                return self.finish_step_error(
                                    process,
                                    current.header().version,
                                    committed_state,
                                    pending,
                                    error,
                                    executed.saturating_add(1),
                                );
                            }
                            return self.finish_step_error(
                                process,
                                process_version,
                                state_before_barrier,
                                pending,
                                error,
                                executed.saturating_add(1),
                            );
                        }
                    }
                }
                Err(error) => {
                    return self.finish_step_error(
                        process,
                        process_version,
                        state,
                        pending,
                        error,
                        executed.saturating_add(1),
                    );
                }
            }
        }
    }

    fn finish_step_error(
        &self,
        process: ObjectId,
        process_version: oms_types::ObjectVersion,
        mut state: ProcessState,
        mut pending: PendingWrites,
        error: VmError,
        executed: u64,
    ) -> Result<(Option<String>, u64), VmError> {
        let current_version = self.manager.inspect(self.context, process)?.version;
        if current_version != process_version {
            return Err(OmsError::Conflict {
                object: process,
                expected: process_version,
                actual: current_version,
            }
            .into());
        }
        if is_catchable(&error) && !state.handlers.is_empty() {
            self.catch_error_state(process, &mut state, &mut pending, error)?;
            self.flush_pending(process, process_version, &state, &mut pending)?;
            return Ok((None, executed));
        }
        if is_catchable(&error) {
            state.status = ProcessStatus::Failed;
            state.error = Some(error_value(&error));
            state.wait_reason = WaitReason::None;
            state.ended_at_unix_ms = Some(unix_time_millis());
            self.flush_pending(process, process_version, &state, &mut pending)?;
            self.notify_process_ended(process)?;
            return Err(error);
        }
        if executed > 1 {
            self.flush_pending(process, process_version, &state, &mut pending)?;
        }
        Err(error)
    }

    fn flush_pending(
        &self,
        process: ObjectId,
        process_version: oms_types::ObjectVersion,
        state: &ProcessState,
        pending: &mut PendingWrites,
    ) -> Result<oms_types::ObjectVersion, VmError> {
        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, process_version);
        for (object, encoded) in &pending.encoded_values {
            if let Some(mut request) = pending.creates.remove(object) {
                request.state.clone_from(encoded);
                transaction.create(request);
            } else {
                let version = pending
                    .expected_versions
                    .get(object)
                    .copied()
                    .ok_or_else(|| invalid_state("pending Object update has no version"))?;
                transaction
                    .expect(*object, version)
                    .update_state(*object, encoded.clone());
            }
        }
        transaction.update_state(process, encode_process_state(state)?);
        let result = self.manager.commit(transaction)?;
        let version = result
            .versions
            .get(&process)
            .copied()
            .ok_or_else(|| invalid_state("Process update was missing from commit"))?;
        *pending = PendingWrites::default();
        Ok(version)
    }

    fn catch_error_state(
        &self,
        process: ObjectId,
        state: &mut ProcessState,
        pending: &mut PendingWrites,
        error: VmError,
    ) -> Result<(), VmError> {
        let Some(handler) = state.handlers.pop() else {
            return Err(error);
        };
        state.frames.truncate(
            usize::try_from(handler.frame_depth)
                .map_err(|_| invalid_state("invalid exception frame depth"))?,
        );
        state
            .stack
            .truncate(usize::try_from(handler.stack_base).map_err(|_| VmError::StackUnderflow)?);
        state.token_position = handler.catch_position;
        let value = error_value(&error);
        let object = self.stage_store(process, state, handler.error_name, &value, pending)?;
        pending.values.insert(object, value);
        Ok(())
    }

    fn execute_pure_step(
        &self,
        process: ObjectId,
        state: &mut ProcessState,
        program: &Program,
        pending: &mut PendingWrites,
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
            Token::ObjectCall { method, arguments } => {
                return self
                    .execute_ephemeral_object_call(process, state, next, &method, arguments);
            }
            Token::Push(value) => state.stack.push(value),
            Token::Load(name) => {
                let object = binding_id(state, &name)?;
                let value = if let Some(value) = pending.values.get(&object) {
                    value.clone()
                } else {
                    self.variable_from_state(state, &name)?
                };
                state.stack.push(value);
            }
            Token::LoadIdentity(name) => {
                let binding = binding_id(state, &name)?;
                state.stack.push(Value::Text(binding.to_string()));
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
                let object = match self.stage_store(process, state, name, &value, pending) {
                    Ok(object) => object,
                    Err(error) => {
                        state.stack.push(value);
                        return Err(error);
                    }
                };
                pending.values.insert(object, value);
                state.token_position = next;
                return Ok(true);
            }
            Token::MakeArray(_)
            | Token::MakeMap(_)
            | Token::IndexGet
            | Token::IndexSet
            | Token::IndexIncrement
            | Token::IndexDecrement
            | Token::Length => execute_collection_token(&token, &mut state.stack)?,
            Token::GetField(field) => {
                let receiver = state.stack.last().ok_or(VmError::StackUnderflow)?;
                let value = self.get_field(state, receiver, &field, program)?;
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

    fn execute_ephemeral_object_call(
        &self,
        process: ObjectId,
        state: &mut ProcessState,
        next: u32,
        method: &str,
        argument_count: u32,
    ) -> Result<bool, VmError> {
        let argument_count =
            usize::try_from(argument_count).map_err(|_| VmError::StackUnderflow)?;
        let receiver_index = state
            .stack
            .len()
            .checked_sub(argument_count.saturating_add(1))
            .ok_or(VmError::StackUnderflow)?;
        let receiver = &state.stack[receiver_index];
        let object = object_id(receiver)?;
        let type_id = self.manager.inspect(self.context, object)?.type_id;
        let Ok(provider) = self.providers.get(type_id) else {
            return Ok(false);
        };
        if !provider.ephemeral_capabilities().contains(method) {
            return Ok(false);
        }

        self.enforce_package_capability(process, state.subject, type_id, method)?;
        self.manager
            .require_capability(self.context, object, Capability::Invoke)?;
        let target = self.manager.read(self.context, object)?;
        let target_value = Value::decode(target.state())?;
        let arguments = state.stack[receiver_index + 1..].to_vec();
        let outcome = provider.invoke_ephemeral_for_process(
            process,
            object,
            &target_value,
            method,
            &arguments,
        )?;
        if outcome.object_state.is_some() || !outcome.created.is_empty() {
            return Err(VmError::TypeError(
                "ephemeral Provider operations cannot modify persistent Objects",
            ));
        }

        state.stack.truncate(receiver_index);
        state.stack.push(outcome.result);
        state.token_position = next;
        Ok(true)
    }

    fn stage_store(
        &self,
        process: ObjectId,
        state: &mut ProcessState,
        name: String,
        value: &Value,
        pending: &mut PendingWrites,
    ) -> Result<ObjectId, VmError> {
        let existing = state
            .frames
            .last()
            .and_then(|frame| frame.locals.get(&name))
            .or_else(|| state.variables.get(&name))
            .copied();

        let object = if let Some(object) = existing {
            if pending.creates.contains_key(&object) {
                let encoded = value.encode()?;
                pending.encoded_values.insert(object, encoded);
            } else {
                let (version, encoded) =
                    self.manager
                        .prepare_replace_value(self.context, object, value)?;
                if let Some(expected) = pending.expected_versions.get(&object) {
                    if *expected != version {
                        return Err(OmsError::Conflict {
                            object,
                            expected: *expected,
                            actual: version,
                        }
                        .into());
                    }
                }
                pending.expected_versions.insert(object, version);
                pending.encoded_values.insert(object, encoded);
            }
            object
        } else {
            let mut request = self.manager.prepare_create(
                CreateSpec::new("core.value", value.clone()).with_parent(process),
            )?;
            while self.manager.shard_for(request.id) != self.manager.shard_for(process) {
                request.id = ObjectId::new();
            }
            let object = request.id;
            bind_name(state, name, object);
            pending.encoded_values.insert(object, request.state.clone());
            pending.creates.insert(object, request);
            object
        };

        Ok(object)
    }

    // This dispatcher handles operations that may touch Objects, Providers or
    // lifecycle state. Pure tokens are handled by `execute_pure_step` above.
    #[allow(clippy::too_many_lines)]
    pub(super) fn execute_step(
        &self,
        process: ObjectId,
        mut state: ProcessState,
        expected_version: oms_types::ObjectVersion,
    ) -> Result<Option<String>, VmError> {
        let process_view = self.manager.read(self.context, process)?;
        if process_view.header().version != expected_version {
            return Err(OmsError::Conflict {
                object: process,
                expected: expected_version,
                actual: process_view.header().version,
            }
            .into());
        }
        let program = self.program(state.program)?;
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

        match token {
            Token::Push(value) => {
                state.stack.push(value);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Load(name) => {
                let value = self.variable_from_state(&state, &name)?;
                state.stack.push(value);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::LoadIdentity(name) => {
                let binding = binding_id(&state, &name)?;
                state.stack.push(Value::Text(binding.to_string()));
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Store(name) => {
                let value = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.token_position = next;
                self.commit_store(
                    process,
                    process_view.header().version,
                    &mut state,
                    name,
                    &value,
                )?;
                Ok(None)
            }
            Token::Add | Token::Subtract | Token::Multiply | Token::Divide | Token::Modulo => {
                let right = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let left = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.stack.push(arithmetic(&token, left, right)?);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Equal
            | Token::NotEqual
            | Token::Less
            | Token::LessEqual
            | Token::Greater
            | Token::GreaterEqual => {
                let right = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let left = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state
                    .stack
                    .push(Value::Bool(compare(&token, &left, &right)?));
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Not => {
                let value = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.stack.push(Value::Bool(!value.is_truthy()));
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Jump(target) => {
                state.token_position = target;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::JumpIfFalse(target) => {
                let condition = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.token_position = if condition.is_truthy() { next } else { target };
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Pop => {
                state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::MakeArray(_)
            | Token::MakeMap(_)
            | Token::IndexGet
            | Token::IndexSet
            | Token::IndexIncrement
            | Token::IndexDecrement
            | Token::Length => {
                execute_collection_token(&token, &mut state.stack)?;
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::RegistryCall { method, arguments } => self.step_call(
                process,
                process_view.header().version,
                &mut state,
                next,
                &method,
                arguments,
                true,
                &program,
            ),
            Token::BindCreated { name, arguments } => {
                let args = pop_call_arguments(&mut state.stack, arguments)?;
                let mut transaction = self.manager.begin(self.context);
                transaction.expect(process, process_view.header().version);
                let (created, output) =
                    self.registry_call(process, state.program, "create", &args, &mut transaction)?;
                debug_assert!(output.is_none());
                bind_name(&mut state, name, object_id(&created)?);
                state.token_position = next;
                transaction.update_state(process, encode_process_state(&state)?);
                self.manager.commit(transaction)?;
                Ok(None)
            }
            Token::BindFound { name, arguments } => {
                let args = pop_call_arguments(&mut state.stack, arguments)?;
                let mut transaction = self.manager.begin(self.context);
                transaction.expect(process, process_view.header().version);
                let (found, output) =
                    self.registry_call(process, state.program, "find", &args, &mut transaction)?;
                debug_assert!(output.is_none());
                bind_name(&mut state, name, object_id(&found)?);
                state.token_position = next;
                transaction.update_state(process, encode_process_state(&state)?);
                self.manager.commit(transaction)?;
                Ok(None)
            }
            Token::ObjectCall { method, arguments } => self.step_call(
                process,
                process_view.header().version,
                &mut state,
                next,
                &method,
                arguments,
                false,
                &program,
            ),
            Token::DefineFunction { end, .. }
            | Token::DefineClass { end, .. }
            | Token::DefineMethod { end, .. } => {
                state.token_position = end;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::CallFunction { name, arguments } => self.enter_function(
                process,
                process_view.header().version,
                &mut state,
                next,
                &program,
                &name,
                arguments,
            ),
            Token::Return => {
                let result = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let frame = state
                    .frames
                    .pop()
                    .ok_or(VmError::TypeError("return outside function"))?;
                state.stack.truncate(
                    usize::try_from(frame.stack_base).map_err(|_| VmError::StackUnderflow)?,
                );
                state.stack.push(result);
                state.token_position = frame.return_position;
                state.handlers.retain(|handler| {
                    usize::try_from(handler.frame_depth)
                        .is_ok_and(|depth| depth <= state.frames.len())
                });
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::GetField(field) => {
                let receiver = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let value = self.get_field(&state, &receiver, &field, &program)?;
                state.stack.push(value);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::SetField(field) => {
                let value = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let receiver = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.token_position = next;
                self.commit_field(
                    process,
                    process_view.header().version,
                    &state,
                    &receiver,
                    &field,
                    value,
                    &program,
                )?;
                Ok(None)
            }
            Token::BindLink { name, target } => {
                let target = binding_id(&state, &target)?;
                bind_name(&mut state, name, target);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::SuperCall { method, arguments } => self.step_super_call(
                process,
                process_view.header().version,
                &mut state,
                next,
                &program,
                &method,
                arguments,
            ),
            Token::BeginTry { catch, error, .. } => {
                state.handlers.push(ExceptionHandler {
                    catch_position: catch,
                    error_name: error,
                    frame_depth: u32::try_from(state.frames.len())
                        .map_err(|_| invalid_state("too many call frames"))?,
                    stack_base: u32::try_from(state.stack.len())
                        .map_err(|_| invalid_state("stack is too large"))?,
                });
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::EndTry { end } => {
                state
                    .handlers
                    .pop()
                    .ok_or(VmError::TypeError("try handler stack is empty"))?;
                state.token_position = end;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Transaction { end } => self.step_transaction(
                process,
                process_view.header().version,
                &mut state,
                &program,
                next,
                end,
            ),
            Token::CommitTransaction => Err(VmError::TypeError(
                "transaction commit marker cannot execute directly",
            )),
            Token::Halt => {
                state.status = ProcessStatus::Halted;
                state.result = state.stack.last().cloned();
                state.error = None;
                state.wait_reason = WaitReason::None;
                state.ended_at_unix_ms = Some(unix_time_millis());
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                self.notify_process_ended(process)?;
                Ok(None)
            }
        }
    }
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
                    .map_err(|_| invalid_state("too many call frames"))?,
                stack_base: u32::try_from(state.stack.len())
                    .map_err(|_| invalid_state("stack is too large"))?,
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
        _ => return Ok(false),
    }
    Ok(true)
}

fn execute_binary_pure_token(state: &mut ProcessState, token: &Token) -> Result<(), VmError> {
    let start = state
        .stack
        .len()
        .checked_sub(2)
        .ok_or(VmError::StackUnderflow)?;
    if matches!(
        token,
        Token::Add | Token::Subtract | Token::Multiply | Token::Divide | Token::Modulo
    ) {
        let result = arithmetic_ref(token, &state.stack[start], &state.stack[start + 1])?;
        state.stack.truncate(start);
        state.stack.push(result);
    } else {
        let result = compare(token, &state.stack[start], &state.stack[start + 1])?;
        state.stack.truncate(start);
        state.stack.push(Value::Bool(result));
    }
    Ok(())
}

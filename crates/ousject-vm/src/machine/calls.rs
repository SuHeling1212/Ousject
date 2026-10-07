#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn step_call(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        method: &str,
        arguments: u32,
        registry: bool,
        program: &Program,
    ) -> Result<Option<String>, VmError> {
        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, version);
        let args = pop_call_arguments(&mut state.stack, arguments)?;
        let mut ended_processes = Vec::new();
        let (result, output) = if registry {
            self.registry_call(process, state.program, method, &args, &mut transaction)?
        } else {
            let receiver = state.stack.pop().ok_or(VmError::StackUnderflow)?;
            let receiver_id = object_id(&receiver)?;
            let receiver_type = self.manager.inspect(self.context, receiver_id)?.type_id;
            self.enforce_package_capability(process, state.subject, receiver_type, method)?;
            if receiver_type == PROCESS_TYPE && method == "wait" && receiver_id != process {
                if !args.is_empty() {
                    return Err(VmError::TypeError("process.wait expects no arguments"));
                }
                self.manager
                    .require_capability(self.context, receiver_id, Capability::Invoke)?;
                let system = AccessContext::new(SYSTEM_SUBJECT);
                let child = self.manager.read(system, receiver_id)?;
                let child_state = decode_process_state(child.state())?;
                if matches!(
                    child_state.status,
                    ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
                ) {
                    state.stack.push(Value::Text(
                        process_status_name(child_state.status).to_owned(),
                    ));
                    state.token_position = next;
                    transaction.update_state(process, encode_process_state(state)?);
                    self.manager.commit(transaction)?;
                    return Ok(None);
                }

                // Keep the call operands and instruction position intact. The
                // scheduler will retry this call after the child ends.
                state.stack.push(receiver);
                state.stack.extend(args);
                state.status = ProcessStatus::Waiting;
                state.wait_reason = WaitReason::Process(receiver_id);
                state.ended_at_unix_ms = None;
                let mut waiting = self.manager.begin(system);
                waiting
                    .expect(process, version)
                    .expect(receiver_id, child.header().version)
                    .set_link(process, "$waiting_on", receiver_id)
                    .set_link(receiver_id, format!("$wait:{process}"), process)
                    .update_state(process, encode_process_state(state)?);
                self.manager.commit(waiting)?;
                return Ok(None);
            }
            if receiver_type == CORE_TIME_TYPE && method == "sleep" {
                self.manager
                    .require_capability(self.context, receiver_id, Capability::Invoke)?;
                let [Value::Integer(milliseconds)] = args.as_slice() else {
                    return Err(VmError::TypeError("time.sleep requires milliseconds"));
                };
                let milliseconds = u64::try_from(*milliseconds)
                    .map_err(|_| VmError::TypeError("sleep duration must not be negative"))?;
                let deadline = unix_time_millis()
                    .checked_add(milliseconds)
                    .ok_or(VmError::TypeError("sleep deadline is too large"))?;
                let mut timer = self.prepare_sleep_timer(process, deadline)?;
                let timer_id = timer.id;
                timer.links.insert(format!("$wait:{process}"), process);
                state.stack.push(Value::Null);
                state.token_position = next;
                state.status = ProcessStatus::Waiting;
                state.wait_reason = WaitReason::Timer {
                    timer: Some(timer_id),
                    deadline_unix_ms: deadline,
                };
                state.ended_at_unix_ms = None;
                transaction
                    .create(timer)
                    .set_link(process, "$timer", timer_id)
                    .update_state(process, encode_process_state(state)?);
                self.manager.commit(transaction)?;
                return Ok(None);
            }
            if matches!(
                receiver_type,
                CORE_AUTHENTICATION_TYPE | CORE_USER_REGISTRY_TYPE | CORE_TYPE_REGISTRY_TYPE
            ) {
                // Authentication changes and this Process's instruction/state
                // advance must form one commit. Authorization was checked with
                // the real caller above and again by the domain capability.
                transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
                transaction.expect(process, version);
            } else if receiver_type == CORE_CHANNEL_TYPE && method == "send" {
                for capability in [
                    Capability::Invoke,
                    Capability::ViewValue,
                    Capability::ReplaceValue,
                ] {
                    self.manager
                        .require_capability(self.context, receiver_id, capability)?;
                }
                // Waking a registered waiter is a scheduler action. The caller
                // is checked above, then the trusted VM may update Processes
                // owned by other Subjects in the same atomic commit.
                transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
                transaction.expect(process, version);
            } else if (receiver_type == PROCESS_TYPE
                && matches!(method, "start" | "resume" | "suspend" | "terminate"))
                || (receiver_type == CORE_EFFECT_TYPE && matches!(method, "retry" | "resolve"))
                || matches!(method, "grant" | "revoke")
            {
                transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
                transaction.expect(process, version);
            }
            let domain_method = self
                .manager
                .type_by_id(receiver_type)?
                .domain_capabilities
                .contains(method);
            if is_base_object_operation(method) && !domain_method {
                let mut explicit = Vec::with_capacity(args.len() + 1);
                explicit.push(receiver.clone());
                explicit.extend(args);
                let (result, output) = if method == "retire" {
                    self.retire_object(process, state, object_id(&receiver)?, &mut transaction)?;
                    (Value::Null, None)
                } else {
                    (
                        self.object_operation(state.program, method, &explicit, &mut transaction)?,
                        None,
                    )
                };
                if matches!(method, "replace" | "link" | "unlink") {
                    (receiver, output)
                } else {
                    (result, output)
                }
            } else if receiver_type == INSTANCE_TYPE {
                return self.enter_method(
                    process,
                    version,
                    state,
                    next,
                    program,
                    &receiver,
                    method,
                    args,
                    None,
                    transaction,
                );
            } else {
                if receiver_type == PROCESS_TYPE && method == "terminate" {
                    ended_processes.push(receiver_id);
                }
                if receiver_type == CORE_TERMINAL_SESSION_TYPE
                    && matches!(method, "cancel" | "close")
                {
                    if let Ok(process_id) =
                        self.terminal_session_process(receiver_id, state.subject)
                    {
                        ended_processes.push(process_id);
                    }
                }
                let object = object_id(&receiver)?;
                let type_id = self.manager.inspect(self.context, object)?.type_id;
                if self.providers.get(type_id).is_ok() {
                    return self
                        .step_provider_call(process, version, state, next, object, method, &args);
                }
                if receiver_type == CORE_PACKAGE_INSTALLATION_TYPE
                    || domain_method
                        && matches!(
                            receiver_type,
                            CORE_PACKAGE_REGISTRY_TYPE
                                | CORE_PACKAGE_MARKET_TYPE
                                | CORE_PACKAGE_DOWNLOAD_TYPE
                                | CORE_TERMINAL_SESSION_TYPE
                        )
                {
                    self.manager
                        .require_capability(self.context, object, Capability::Invoke)?;
                    // Package and Terminal Session handlers validate Subject
                    // ownership and stage protected Objects in the same
                    // transaction that advances this caller's Process.
                    transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
                    transaction.expect(process, version);
                }
                self.invoke_object(process, state, &receiver, method, &args, &mut transaction)?
            }
        };
        state.stack.push(result);
        state.token_position = next;
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        for ended_process in ended_processes {
            self.notify_process_ended(ended_process)?;
        }
        Ok(output)
    }
}

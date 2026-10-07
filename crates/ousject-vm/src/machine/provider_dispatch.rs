#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn step_provider_call(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        object: ObjectId,
        capability: &str,
        arguments: &[Value],
    ) -> Result<Option<String>, VmError> {
        self.manager
            .require_capability(self.context, object, Capability::Invoke)?;
        let target = self.manager.read(self.context, object)?;
        let provider = self
            .providers
            .get(target.header().type_id)
            .map_err(VmError::from)?;
        if !provider.capabilities().contains(capability) {
            return Err(VmError::from(ProviderError::UnsupportedCapability(
                capability.to_owned(),
            )));
        }

        let system = AccessContext::new(SYSTEM_SUBJECT);
        let existing_effect = self
            .manager
            .read(self.context, process)?
            .links()
            .get("$effect")
            .copied();
        let is_new_effect = existing_effect.is_none();
        let effect = if let Some(effect) = existing_effect {
            let record = EffectRecord::decode(self.manager.read(system, effect)?.state())
                .map_err(VmError::from)?;
            if record.process != process
                || record.target != object
                || record.token_position != state.token_position
                || record.capability != capability
                || record.arguments != EffectRecord::redact_arguments(arguments)
            {
                return Err(VmError::InvalidProcessState(
                    "pending Effect does not match the Process token".to_owned(),
                ));
            }
            if record.status == EffectStatus::Unknown {
                return Err(VmError::Provider(
                    "Effect outcome is unknown; local must resolve or retry it".to_owned(),
                ));
            }
            if record.status == EffectStatus::Failed {
                return Err(VmError::Provider(
                    record
                        .error
                        .unwrap_or_else(|| "Provider operation failed".to_owned()),
                ));
            }
            effect
        } else {
            let effect = ObjectId::new();
            let record = EffectRecord::pending_with_policy(
                process,
                object,
                state.token_position,
                capability,
                arguments.to_vec(),
                provider.effect_recovery_policy(capability),
            );
            let mut waiting = state.clone();
            waiting.stack.push(Value::Text(object.to_string()));
            waiting.stack.extend(arguments.iter().cloned());
            waiting.status = ProcessStatus::Waiting;
            waiting.wait_reason = WaitReason::Effect(effect);
            let request = record
                .create_object(effect)
                .map_err(VmError::from)?
                .with_grant(state.subject, Capability::Inspect)
                .with_grant(state.subject, Capability::ViewValue)
                .with_grant(state.subject, Capability::Invoke);
            let mut intent = self.manager.begin(system);
            intent
                .expect(process, version)
                .create(request)
                .set_link(process, "$effect", effect)
                .update_state(process, encode_process_state(&waiting)?);
            self.stage_audit_event(
                state.subject,
                "effect.intent",
                effect,
                Value::Record(BTreeMap::from([(
                    "capability".to_owned(),
                    Value::Text(capability.to_owned()),
                )])),
                &mut intent,
            )?;
            self.manager.commit(intent)?;
            effect
        };

        let effect_view = self.manager.read(system, effect)?;
        let mut record = EffectRecord::decode(effect_view.state()).map_err(VmError::from)?;
        let target_value = Value::decode(target.state())?;
        let outcome = if record.status == EffectStatus::Completed {
            Ok(ProviderOutcome::result(
                record.result.clone().unwrap_or(Value::Null),
            ))
        } else {
            if record.status != EffectStatus::Running {
                record = record.running();
                let mut running = self.manager.begin(system);
                running
                    .expect(effect, effect_view.header().version)
                    .update_state(effect, record.encode().map_err(VmError::from)?);
                if is_new_effect {
                    self.stage_audit_event(
                        state.subject,
                        "effect.running",
                        effect,
                        Value::Record(BTreeMap::new()),
                        &mut running,
                    )?;
                }
                self.manager.commit(running)?;
            }
            provider.invoke_for_process(
                process,
                object,
                &target_value,
                capability,
                arguments,
                effect,
            )
        };

        let process_view = self.manager.read(system, process)?;
        let persisted_process = decode_process_state(process_view.state())?;
        if persisted_process.lease_owner != state.lease_owner
            || persisted_process.lease_generation != state.lease_generation
            || persisted_process
                .lease_deadline_unix_ms
                .is_none_or(|deadline| deadline <= unix_time_millis())
        {
            return Err(VmError::WorkerLeaseExpired(process));
        }
        let effect_view = self.manager.read(system, effect)?;
        let record = EffectRecord::decode(effect_view.state()).map_err(VmError::from)?;
        let mut completion = self.manager.begin(system);
        completion
            .expect(process, process_view.header().version)
            .expect(effect, effect_view.header().version)
            .remove_link(process, "$effect");
        match outcome {
            Ok(outcome) => {
                completion.update_state(
                    effect,
                    record
                        .complete(outcome.result.clone())
                        .encode()
                        .map_err(VmError::from)?,
                );
                if let Some(object_state) = outcome.object_state {
                    completion
                        .expect(object, target.header().version)
                        .update_state(object, object_state.encode()?);
                }
                for request in outcome.created {
                    if let Some(parent) = request.parent {
                        let parent_version = self.manager.inspect(system, parent)?.version;
                        completion.expect(parent, parent_version);
                    }
                    completion.create(request);
                }
                state.status = ProcessStatus::Running;
                state.wait_reason = WaitReason::None;
                state.stack.push(outcome.result);
                state.token_position = next;
                completion.update_state(process, encode_process_state(state)?);
                self.stage_audit_event(
                    state.subject,
                    "effect.completed",
                    effect,
                    Value::Record(BTreeMap::new()),
                    &mut completion,
                )?;
                self.manager.commit(completion)?;
                Ok(
                    if target.header().type_id == CORE_TERMINAL_TYPE && capability == "println" {
                        arguments.first().map(ToString::to_string)
                    } else {
                        None
                    },
                )
            }
            Err(ProviderError::Pending) => {
                // The call token remains in place. Restore its operands and
                // persist one unified wait reason until input or an external
                // Provider result is available.
                state.stack.push(Value::Text(object.to_string()));
                state.stack.extend(arguments.iter().cloned());
                state.status = ProcessStatus::Waiting;
                state.wait_reason = if target.header().type_id == CORE_TERMINAL_TYPE
                    && matches!(capability, "read_line" | "read_secret")
                {
                    WaitReason::Input(effect)
                } else {
                    WaitReason::Effect(effect)
                };
                let mut pending = self.manager.begin(system);
                pending
                    .expect(process, process_view.header().version)
                    .expect(effect, effect_view.header().version)
                    .update_state(effect, record.retry().encode().map_err(VmError::from)?)
                    .update_state(process, encode_process_state(state)?);
                if is_new_effect {
                    self.stage_audit_event(
                        state.subject,
                        "effect.pending",
                        effect,
                        Value::Record(BTreeMap::new()),
                        &mut pending,
                    )?;
                }
                self.manager.commit(pending)?;
                Ok(None)
            }
            Err(error) => {
                state.stack.push(Value::Text(object.to_string()));
                state.stack.extend(arguments.iter().cloned());
                state.status = ProcessStatus::Running;
                state.wait_reason = WaitReason::None;
                let mut failed = self.manager.begin(system);
                failed
                    .expect(process, process_view.header().version)
                    .expect(effect, effect_view.header().version)
                    .update_state(
                        effect,
                        record
                            .fail("Provider operation failed")
                            .encode()
                            .map_err(VmError::from)?,
                    )
                    .update_state(process, encode_process_state(state)?);
                self.stage_audit_event(
                    state.subject,
                    "effect.failed",
                    effect,
                    Value::Record(BTreeMap::new()),
                    &mut failed,
                )?;
                self.manager.commit(failed)?;
                Err(VmError::from(error))
            }
        }
    }
}

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

        let effect = if let Some(effect) = self
            .manager
            .read(self.context, process)?
            .links()
            .get("$effect")
            .copied()
        {
            let record = EffectRecord::decode(self.manager.read(self.context, effect)?.state())
                .map_err(VmError::from)?;
            if record.process != process
                || record.target != object
                || record.token_position != state.token_position
                || record.capability != capability
                || record.arguments != arguments
            {
                return Err(VmError::InvalidProcessState(
                    "pending Effect does not match the Process token".to_owned(),
                ));
            }
            effect
        } else {
            let effect = ObjectId::new();
            let record = EffectRecord::pending(
                process,
                object,
                state.token_position,
                capability,
                arguments.to_vec(),
            );
            let mut intent = self.manager.begin(self.context);
            intent
                .expect(process, version)
                .create(record.create_object(effect).map_err(VmError::from)?)
                .set_link(process, "$effect", effect);
            self.manager.commit(intent)?;
            effect
        };

        let target_value = Value::decode(target.state())?;
        let outcome = provider.invoke_for_process(
            process,
            object,
            &target_value,
            capability,
            arguments,
            effect,
        );
        let process_view = self.manager.read(self.context, process)?;
        let effect_view = self.manager.read(self.context, effect)?;
        let pending = EffectRecord::decode(effect_view.state()).map_err(VmError::from)?;
        let mut completion = self.manager.begin(self.context);
        completion
            .expect(process, process_view.header().version)
            .expect(effect, effect_view.header().version)
            .remove_link(process, "$effect");
        match outcome {
            Ok(outcome) => {
                completion.update_state(
                    effect,
                    pending
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
                        let parent_version = self.manager.inspect(self.context, parent)?.version;
                        completion.expect(parent, parent_version);
                    }
                    completion.create(request);
                }
                state.stack.push(outcome.result);
                state.token_position = next;
                completion.update_state(process, encode_process_state(state)?);
                self.manager.commit(completion)?;
                Ok(
                    if target.header().type_id == CONSOLE_TYPE && capability == "println" {
                        arguments.first().map(ToString::to_string)
                    } else {
                        None
                    },
                )
            }
            Err(ProviderError::Pending) => {
                // `step_call` already popped the receiver and arguments. The
                // Process remains on this same call token, so its operand
                // stack must be restored exactly for the idempotent retry.
                state.stack.push(Value::Text(object.to_string()));
                state.stack.extend(arguments.iter().cloned());
                state.status = ProcessStatus::Suspended;
                completion = self.manager.begin(self.context);
                completion
                    .expect(process, process_view.header().version)
                    .update_state(process, encode_process_state(state)?);
                self.manager.commit(completion)?;
                Ok(None)
            }
            Err(error) => {
                completion.update_state(
                    effect,
                    pending
                        .fail(error.to_string())
                        .encode()
                        .map_err(VmError::from)?,
                );
                self.manager.commit(completion)?;
                Err(VmError::from(error))
            }
        }
    }
}

#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    pub(super) fn registry_call(
        &self,
        process: ObjectId,
        program_id: ObjectId,
        method: &str,
        args: &[Value],
        transaction: &mut Transaction,
    ) -> Result<(Value, Option<String>), VmError> {
        let output = match (method, args) {
            ("create", [Value::Text(type_name), value]) => {
                let mut request =
                    self.prepare_object_create(program_id, type_name, value, process)?;
                while self.manager.shard_for(request.id) != self.manager.shard_for(process) {
                    request.id = ObjectId::new();
                }
                let process_object = request.type_id == PROCESS_TYPE;
                let id = request.id;
                if process_object {
                    request.links.insert("process".to_owned(), id);
                }
                transaction.create(request);
                Ok(Value::Text(id.to_string()))
            }
            ("create", [Value::Text(type_name), value, parent]) => {
                let parent = object_id(parent)?;
                let parent_version = self.manager.inspect(self.context, parent)?.version;
                let mut request =
                    self.prepare_object_create(program_id, type_name, value, parent)?;
                while self.manager.shard_for(request.id) != self.manager.shard_for(process) {
                    request.id = ObjectId::new();
                }
                let process_object = request.type_id == PROCESS_TYPE;
                let id = request.id;
                if process_object {
                    request.links.insert("process".to_owned(), id);
                }
                transaction.expect(parent, parent_version).create(request);
                Ok(Value::Text(id.to_string()))
            }
            ("find", [Value::Text(identity)]) => {
                let object = if let Ok(object) = identity.parse() {
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
                Ok(Value::Text(object.to_string()))
            }
            ("query", [Value::Text(type_name)] | [Value::Text(type_name), Value::Null]) => {
                self.query_objects(program_id, type_name, None)
            }
            ("query", [Value::Text(type_name), Value::Text(capability)]) => {
                self.query_objects(program_id, type_name, Some(capability))
            }
            _ => Err(VmError::TypeError(
                "object only supports create, find and query",
            )),
        }?;
        Ok((output, None))
    }

    pub(super) fn retire_object(
        &self,
        process: ObjectId,
        state: &mut ProcessState,
        target: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        self.retire_object_as(process, state, target, self.context, transaction)
    }

    pub(super) fn retire_object_as(
        &self,
        process: ObjectId,
        state: &mut ProcessState,
        target: ObjectId,
        context: AccessContext,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        self.retire_objects_as(
            process,
            state,
            BTreeSet::from([target]),
            context,
            transaction,
        )
    }

    /// Retires several roots and all of their children as one atomic Object
    /// lifecycle operation.  It is used by Package cleanup so the root and
    /// automatically removable dependencies can never be observed half gone.
    pub(super) fn retire_objects_as(
        &self,
        process: ObjectId,
        state: &mut ProcessState,
        targets: BTreeSet<ObjectId>,
        context: AccessContext,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let mut retired = BTreeSet::new();
        let mut discovery_order = Vec::new();
        let mut pending = targets.into_iter().collect::<Vec<_>>();
        while let Some(object) = pending.pop() {
            if !retired.insert(object) {
                continue;
            }
            let view = self.manager.read(context, object)?;
            if matches!(
                view.header().type_id,
                CORE_PACKAGE_TYPE
                    | CORE_PACKAGE_REGISTRY_TYPE
                    | CORE_PACKAGE_AUDIT_TYPE
                    | CORE_PACKAGE_MARKET_TYPE
                    | CORE_PACKAGE_MARKET_CONFIG_TYPE
                    | CORE_PACKAGE_DOWNLOAD_TYPE
            ) {
                return Err(VmError::TypeError(
                    "Packages, Package Audit records and the Package Registry are immutable system Objects",
                ));
            }
            if view.header().type_id == CORE_MODULE_TYPE {
                self.ensure_module_links_clear(view.links(), &retired)?;
            }
            if view.header().type_id == CORE_USER_TYPE
                && is_local_user_value(&self.manager.value(context, object)?)
            {
                return Err(VmError::TypeError(
                    "the reserved local User cannot be retired",
                ));
            }
            if view.header().type_id == CORE_EFFECT_TYPE {
                let effect = EffectRecord::decode(view.state())?;
                if effect.status == ousject_provider::EffectStatus::Pending {
                    return Err(VmError::TypeError("a pending Effect cannot be retired"));
                }
            }
            discovery_order.push(object);
            pending.extend(view.children().iter().copied());
        }
        if retired.contains(&process) {
            return Err(VmError::TypeError(
                "a running Process must terminate itself instead of retiring itself",
            ));
        }

        remove_retired_bindings(state, &retired);
        for header in self.manager.list(context)? {
            if header.lifecycle == LifecycleState::Tombstoned {
                continue;
            }
            let view = self.manager.read(context, header.id)?;
            let mut link_names: BTreeSet<String> = view
                .links()
                .iter()
                .filter(|(_, linked)| retired.contains(linked))
                .map(|(name, _)| name.clone())
                .collect();
            if retired.contains(&header.id) {
                link_names.extend(view.links().keys().cloned());
            }
            if !link_names.is_empty() {
                transaction.expect(header.id, header.version);
                for name in link_names {
                    transaction.remove_link(header.id, name);
                }
            }
            if header.type_id == PROCESS_TYPE
                && header.id != process
                && !retired.contains(&header.id)
            {
                let mut other_state = decode_process_state(view.state())?;
                if remove_retired_bindings(&mut other_state, &retired) {
                    transaction
                        .expect(header.id, header.version)
                        .update_state(header.id, encode_process_state(&other_state)?);
                }
            }
        }

        for object in discovery_order.into_iter().rev() {
            let view = self.manager.read(context, object)?;
            transaction.expect(object, view.header().version);
            if let Some(parent) = view.header().parent_id {
                let parent_version = self.manager.inspect(context, parent)?.version;
                transaction.expect(parent, parent_version);
            }
            transaction.tombstone(object);
        }
        Ok(())
    }

    fn ensure_module_links_clear(
        &self,
        links: &BTreeMap<String, ObjectId>,
        retiring: &BTreeSet<ObjectId>,
    ) -> Result<(), VmError> {
        for (link_name, linked) in links {
            if retiring.contains(linked)
                || (!link_name.starts_with("instance:") && !link_name.starts_with("dependent:"))
            {
                continue;
            }
            let Ok(linked_view) = self.manager.read(self.context, *linked) else {
                continue;
            };
            if link_name.starts_with("instance:")
                && linked_view.header().type_id == CORE_MODULE_INSTANCE_TYPE
            {
                let Value::Record(instance) = self.manager.value(self.context, *linked)? else {
                    continue;
                };
                if instance.get("status") == Some(&Value::Text("active".to_owned())) {
                    return Err(VmError::Provider(
                        "Module is loaded in an active Terminal Session".to_owned(),
                    ));
                }
            }
            if link_name.starts_with("dependent:")
                && linked_view.header().type_id == CORE_MODULE_TYPE
            {
                return Err(VmError::Provider(
                    "Module is required by another installed version".to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn wait_for_process(
        &self,
        _waiter: ObjectId,
        process: ObjectId,
    ) -> Result<Value, VmError> {
        let parent = self.manager.inspect(self.context, process)?.parent_id;
        let session = parent.filter(|parent| {
            self.manager
                .inspect(self.context, *parent)
                .is_ok_and(|header| header.type_id == CORE_TERMINAL_SESSION_TYPE)
        });
        let watch_interrupts = session.is_some()
            && self
                .console_driver
                .as_ref()
                .is_some_and(|driver| driver.is_interactive());
        if watch_interrupts {
            self.console_driver
                .as_ref()
                .expect("checked Some above")
                .begin_interrupt_watch(process)
                .map_err(VmError::Provider)?;
        }

        let outcome = (|| loop {
            match self.run(process, if watch_interrupts { 32 } else { 1_000_000 }) {
                Ok(report) => {
                    if watch_interrupts
                        && self
                            .console_driver
                            .as_ref()
                            .expect("checked Some above")
                            .take_interrupt(process)
                            .map_err(VmError::Provider)?
                    {
                        let state = self.process_state(process)?;
                        if let Some(session) = session {
                            self.interrupt_terminal_session(session, state.subject)?;
                        }
                        return Ok(Value::Text("halted".to_owned()));
                    }
                    if report.status == ProcessStatus::Suspended {
                        if self.poll_pending_effect(process)? {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                            continue;
                        }
                        if let Some(delay) = self.time_until_wake(process)? {
                            std::thread::sleep(delay);
                            let _ = self.wake_due_timer(process)?;
                            continue;
                        }
                        return Ok(Value::Text("suspended".to_owned()));
                    }
                    if report.status != ProcessStatus::Running {
                        return Ok(Value::Text(process_status_name(report.status).to_owned()));
                    }
                }
                Err(_error) if self.process_state(process)?.status == ProcessStatus::Failed => {
                    return Ok(Value::Text("failed".to_owned()));
                }
                Err(error) => return Err(error),
            }
        })();

        if watch_interrupts {
            self.console_driver
                .as_ref()
                .expect("checked Some above")
                .end_interrupt_watch(process);
        }
        outcome
    }
}

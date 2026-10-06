#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    pub(super) fn stage_object_value(
        &self,
        object: ObjectId,
        value: &Value,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let view = self.manager.read(self.context, object)?;
        let encoded = self
            .manager
            .prepare_replace_value(self.context, object, value)?
            .1;
        transaction
            .expect(object, view.header().version)
            .update_state(object, encoded);
        Ok(())
    }

    pub(super) fn change_process_status(
        &self,
        current_process: ObjectId,
        current_state: &mut ProcessState,
        target: ObjectId,
        status: ProcessStatus,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let ended_at_unix_ms = matches!(
            status,
            ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
        )
        .then(unix_time_millis);
        if target == current_process {
            current_state.status = status;
            current_state.wake_at_unix_ms = None;
            current_state.ended_at_unix_ms = ended_at_unix_ms;
            return Ok(());
        }
        let view = self.manager.read(self.context, target)?;
        let mut state = decode_process_state(view.state())?;
        state.status = status;
        state.wake_at_unix_ms = None;
        state.ended_at_unix_ms = ended_at_unix_ms;
        transaction
            .expect(target, view.header().version)
            .update_state(target, encode_process_state(&state)?);
        Ok(())
    }

    pub(super) fn stage_process_subject_access(
        &self,
        process: ObjectId,
        state: &ProcessState,
        subject: SubjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let mut objects = BTreeSet::from([process, state.program]);
        objects.extend(state.variables.values().copied());
        for frame in &state.frames {
            objects.extend(frame.locals.values().copied());
            if let Some(receiver) = frame.receiver {
                objects.insert(receiver);
            }
        }
        for object in objects {
            let header = self.manager.inspect(self.context, object)?;
            transaction.expect(object, header.version);
            for capability in [
                Capability::Inspect,
                Capability::ViewValue,
                Capability::ReplaceValue,
                Capability::CreateChild,
                Capability::Invoke,
                Capability::Link,
                Capability::Reparent,
                Capability::Retire,
            ] {
                transaction.grant(object, subject, capability);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn enter_function(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        program: &Program,
        name: &str,
        arguments: u32,
    ) -> Result<Option<String>, VmError> {
        let args = pop_call_arguments(&mut state.stack, arguments)?;
        let (entry, parameters) = find_function(program, name, args.len())?;
        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, version);
        let locals = self.stage_arguments(process, &parameters, args, &mut transaction)?;
        state.frames.push(CallFrame {
            return_position: next,
            stack_base: u32::try_from(state.stack.len())
                .map_err(|_| invalid_state("stack is too large"))?,
            locals,
            receiver: None,
            class: None,
        });
        state.token_position = entry;
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn enter_method(
        &self,
        process: ObjectId,
        _version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        program: &Program,
        receiver: &Value,
        method: &str,
        args: Vec<Value>,
        start_class: Option<&str>,
        mut transaction: Transaction,
    ) -> Result<Option<String>, VmError> {
        let receiver = object_id(receiver)?;
        let class = instance_class(&self.manager.value(self.context, receiver)?)?;
        let lookup = start_class.unwrap_or(&class);
        let definition = find_method(program, lookup, method, args.len())?;
        let internal = state.frames.last().is_some_and(|frame| {
            frame.receiver == Some(receiver) && frame.class.as_deref() == Some(&definition.class)
        });
        if !definition.public && !internal {
            return Err(VmError::TypeError("private method is not visible"));
        }
        let locals =
            self.stage_arguments(process, &definition.parameters, args, &mut transaction)?;
        state.frames.push(CallFrame {
            return_position: next,
            stack_base: u32::try_from(state.stack.len())
                .map_err(|_| invalid_state("stack is too large"))?,
            locals,
            receiver: Some(receiver),
            class: Some(definition.class),
        });
        state.token_position = definition.entry;
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn step_super_call(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        program: &Program,
        method: &str,
        arguments: u32,
    ) -> Result<Option<String>, VmError> {
        let args = pop_call_arguments(&mut state.stack, arguments)?;
        let receiver = state.stack.pop().ok_or(VmError::StackUnderflow)?;
        let class = state
            .frames
            .last()
            .and_then(|frame| frame.class.as_deref())
            .ok_or(VmError::TypeError("super outside method"))?;
        let parent = class_definition(program, class)?
            .parent
            .ok_or(VmError::TypeError("class has no parent"))?;
        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, version);
        self.enter_method(
            process,
            version,
            state,
            next,
            program,
            &receiver,
            method,
            args,
            Some(&parent),
            transaction,
        )
    }

    pub(super) fn stage_arguments(
        &self,
        process: ObjectId,
        parameters: &[String],
        arguments: Vec<Value>,
        transaction: &mut Transaction,
    ) -> Result<BTreeMap<String, ObjectId>, VmError> {
        let mut locals = BTreeMap::new();
        for (parameter, value) in parameters.iter().zip(arguments) {
            let mut request = self
                .manager
                .prepare_create(CreateSpec::new("core.value", value).with_parent(process))?;
            while self.manager.shard_for(request.id) != self.manager.shard_for(process) {
                request.id = ObjectId::new();
            }
            locals.insert(parameter.clone(), request.id);
            transaction.create(request);
        }
        Ok(locals)
    }

    pub(super) fn prepare_object_create(
        &self,
        program_id: ObjectId,
        type_name: &str,
        initial: &Value,
        parent: ObjectId,
    ) -> Result<CreateObject, VmError> {
        if type_name == "core.process" {
            return self.prepare_process_create(program_id, initial, parent);
        }
        match self
            .manager
            .prepare_create(CreateSpec::new(type_name, initial.clone()).with_parent(parent))
        {
            Ok(request) => Ok(request),
            Err(OmsError::TypeCreationDenied(type_id)) => {
                let provider = self.providers.get(type_id).map_err(VmError::from)?;
                if !provider.user_creatable() {
                    return Err(OmsError::TypeCreationDenied(type_id).into());
                }
                let descriptor = self.manager.type_by_id(type_id)?;
                let state = provider.create(initial).map_err(VmError::from)?.encode()?;
                let mut request = CreateObject::new(type_id, state).with_parent(parent);
                request.capabilities = descriptor.capabilities;
                Ok(request)
            }
            Err(OmsError::UnknownTypeName(_)) => {
                let program_view = self.manager.read(self.context, program_id)?;
                let program = Program::decode(program_view.state())?;
                let mut fields = collect_class_fields(&program, type_name)?;
                match initial {
                    Value::Map(values) | Value::Record(values) => {
                        for (name, value) in values {
                            if !fields.contains_key(name) {
                                return Err(VmError::MissingKey(name.clone()));
                            }
                            fields.insert(name.clone(), value.clone());
                        }
                    }
                    Value::Null => {}
                    _ => {
                        return Err(VmError::TypeError(
                            "class initial value must be a Map, Record or null",
                        ));
                    }
                }
                fields.insert("$class".to_owned(), Value::Text(type_name.to_owned()));
                Ok(
                    CreateObject::new(INSTANCE_TYPE, Value::Record(fields).encode()?)
                        .with_parent(parent),
                )
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn prepare_process_create(
        &self,
        program_id: ObjectId,
        initial: &Value,
        parent: ObjectId,
    ) -> Result<CreateObject, VmError> {
        let (Value::Map(options) | Value::Record(options)) = initial else {
            return Err(VmError::TypeError(
                "Process creation requires { entry, start?, links? }",
            ));
        };
        let Some(Value::Text(entry_name)) = options.get("entry") else {
            return Err(VmError::TypeError("Process entry must be a function name"));
        };
        let start = match options.get("start") {
            None | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            Some(_) => return Err(VmError::TypeError("Process start must be Boolean")),
        };
        for key in options.keys() {
            if !matches!(key.as_str(), "entry" | "start" | "links") {
                return Err(VmError::MissingKey(key.clone()));
            }
        }
        let program = Program::decode(self.manager.read(self.context, program_id)?.state())?;
        let (entry, parameters) = find_function(&program, entry_name, 0)?;
        debug_assert!(parameters.is_empty());
        let halt = program
            .tokens
            .iter()
            .position(|token| matches!(token, Token::Halt))
            .and_then(|position| u32::try_from(position).ok())
            .ok_or(VmError::TypeError("Program has no halt token"))?;
        let state = ProcessState {
            program: program_id,
            subject: self.context.subject,
            token_position: entry,
            stack: Vec::new(),
            variables: BTreeMap::new(),
            status: if start {
                ProcessStatus::Running
            } else {
                ProcessStatus::Suspended
            },
            result: None,
            error: None,
            wake_at_unix_ms: None,
            ended_at_unix_ms: None,
            frames: vec![CallFrame {
                return_position: halt,
                stack_base: 0,
                locals: BTreeMap::new(),
                receiver: None,
                class: None,
            }],
            handlers: Vec::new(),
        };
        let mut request = CreateObject::new(PROCESS_TYPE, encode_process_state(&state)?)
            .with_parent(parent)
            .with_link("program", program_id);
        if let Some(console) = self.console_provider {
            request = request.with_link("console", console);
        }
        for (name, service) in &self.kernel_services {
            request = request.with_link(name.clone(), *service);
        }
        if let Some(links) = options.get("links") {
            let (Value::Map(links) | Value::Record(links)) = links else {
                return Err(VmError::TypeError("Process links must be a Map"));
            };
            for (name, target) in links {
                request = request.with_link(name, object_id(target)?);
            }
        }
        Ok(request)
    }

    pub(super) fn prepare_program_execution(
        &self,
        program: ObjectId,
        parent: ObjectId,
        variables: BTreeMap<String, ObjectId>,
    ) -> Result<CreateObject, VmError> {
        Program::decode(self.manager.read(self.context, program)?.state())?;
        let state = ProcessState {
            program,
            subject: self.context.subject,
            token_position: 0,
            stack: Vec::new(),
            variables,
            status: ProcessStatus::Running,
            result: None,
            error: None,
            wake_at_unix_ms: None,
            ended_at_unix_ms: None,
            frames: Vec::new(),
            handlers: Vec::new(),
        };
        let mut request = CreateObject::new(PROCESS_TYPE, encode_process_state(&state)?)
            .with_parent(parent)
            .with_link("program", program);
        if let Some(console) = self.console_provider {
            request = request.with_link("console", console);
        }
        for (name, service) in &self.kernel_services {
            request = request.with_link(name.clone(), *service);
        }
        Ok(request)
    }
}

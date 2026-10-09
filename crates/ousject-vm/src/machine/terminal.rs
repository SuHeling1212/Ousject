#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    /// Opens or reuses the user's shell state on a child Terminal Object.
    pub(super) fn open_terminal_shell(
        &self,
        parent_terminal: ObjectId,
        owner: SubjectId,
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        for terminal in self.manager.query(
            self.context,
            &ObjectQuery::new().with_type(CORE_TERMINAL_TYPE),
        )? {
            if terminal.parent_id != Some(parent_terminal) {
                continue;
            }
            let Value::Record(fields) = self.manager.value(self.context, terminal.id)? else {
                continue;
            };
            if fields.get("owner") == Some(&Value::Text(owner.to_string()))
                && fields.get("active") == Some(&Value::Bool(true))
            {
                return Ok(terminal.id);
            }
        }

        let provider = self.providers.get(CORE_TERMINAL_TYPE)?;
        let parent_state = self.manager.value(self.context, parent_terminal)?;
        let parent_version = self.manager.inspect(self.context, parent_terminal)?.version;
        let created = provider.invoke(
            parent_terminal,
            &parent_state,
            "create",
            &[],
            ObjectId::new(),
        )?;
        let mut terminal_request = created
            .created
            .into_iter()
            .next()
            .ok_or_else(|| invalid_state("Terminal Provider did not create a child"))?;
        let terminal = terminal_request.id;
        let Value::Record(mut fields) = Value::decode(&terminal_request.state)? else {
            return Err(invalid_state("Terminal Provider state is not a Record"));
        };
        let program_id = ObjectId::new();
        let process = ObjectId::new();
        fields.extend([
            ("owner".to_owned(), Value::Text(owner.to_string())),
            ("program".to_owned(), Value::Text(program_id.to_string())),
            ("process".to_owned(), Value::Text(process.to_string())),
            ("history".to_owned(), Value::Array(Vec::new())),
            ("pending_input".to_owned(), Value::Text(String::new())),
            ("active".to_owned(), Value::Bool(true)),
            ("module_instances".to_owned(), Value::Array(Vec::new())),
            (
                "parent_terminal".to_owned(),
                Value::Text(parent_terminal.to_string()),
            ),
        ]);
        terminal_request.state = Value::Record(fields).encode()?;
        terminal_request.capabilities = self.manager.type_by_id(CORE_TERMINAL_TYPE)?.capabilities;
        transaction.expect(parent_terminal, parent_version);
        let program = Program {
            tokens: vec![Token::Halt],
        };
        let program_request = CreateObject::new(PROGRAM_TYPE, program.encode()?)
            .with_id(program_id)
            .with_parent(terminal);
        let mut variables = self.kernel_services.clone();
        if let Some(terminal) = self.terminal_provider {
            variables.insert("terminal".to_owned(), terminal);
        }
        let process_state = ProcessState {
            program: program_id,
            subject: owner,
            token_position: 0,
            stack: Vec::new(),
            variables,
            status: ProcessStatus::Halted,
            wait_reason: WaitReason::None,
            lease_owner: None,
            lease_generation: 0,
            lease_deadline_unix_ms: None,
            result: None,
            error: None,
            ended_at_unix_ms: Some(unix_time_millis()),
            frames: Vec::new(),
            handlers: Vec::new(),
        };
        let mut process_request =
            CreateObject::new(PROCESS_TYPE, encode_process_state(&process_state)?)
                .with_id(process)
                .with_parent(terminal)
                .with_link("program", program_id)
                .with_link("process", process);
        if let Some(terminal) = self.terminal_provider {
            process_request = process_request.with_link("terminal", terminal);
        }
        for (name, service) in &self.kernel_services {
            process_request = process_request.with_link(name.clone(), *service);
        }
        transaction
            .create(terminal_request)
            .create(program_request)
            .create(process_request);
        Ok(terminal)
    }

    pub(super) fn terminal_process(
        &self,
        terminal: ObjectId,
        owner: SubjectId,
    ) -> Result<ObjectId, VmError> {
        let fields = self.terminal_fields(terminal)?;
        Self::validate_terminal_owner(&fields, owner)?;
        terminal_field_id(&fields, "process")
    }

    pub(super) fn terminal_history(
        &self,
        terminal: ObjectId,
        owner: SubjectId,
    ) -> Result<Value, VmError> {
        let fields = self.terminal_fields(terminal)?;
        Self::validate_terminal_owner(&fields, owner)?;
        fields
            .get("history")
            .cloned()
            .ok_or_else(|| invalid_state("Terminal has no history"))
    }

    pub(super) fn terminal_submit(
        &self,
        terminal: ObjectId,
        owner: SubjectId,
        source: &str,
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        let terminal_view = self.manager.read(self.context, terminal)?;
        let mut fields = terminal_record(terminal_view.state())?;
        Self::validate_terminal_owner(&fields, owner)?;
        if fields.get("active") != Some(&Value::Bool(true)) {
            return Err(VmError::TypeError("Terminal is closed"));
        }
        let process = terminal_field_id(&fields, "process")?;
        let program_id = terminal_field_id(&fields, "program")?;
        let process_view = self.manager.read(self.context, process)?;
        let mut process_state = decode_process_state(process_view.state())?;
        if process_state.subject != owner {
            return Err(VmError::TypeError("Terminal Process owner mismatch"));
        }
        if matches!(
            process_state.status,
            ProcessStatus::Running
                | ProcessStatus::Ready
                | ProcessStatus::Waiting
                | ProcessStatus::Suspended
        ) {
            return Err(VmError::TypeError("Terminal Process is still active"));
        }
        if process_state.status == ProcessStatus::Failed {
            process_state.stack.clear();
            process_state.frames.clear();
            process_state.handlers.clear();
        }
        let program_view = self.manager.read(self.context, program_id)?;
        let mut program = Program::decode(program_view.state())?;
        compact_terminal_program(&mut program)?;
        let mut imported_modules = BTreeMap::new();
        let expanded = expand_interactive_with_contextual_loader(source, |name, importer| {
            let (source, identity) =
                self.package_module_source_for_subject(owner, name, importer)?;
            let module = identity
                .parse::<ObjectId>()
                .map_err(|_| "invalid loaded Module Object id".to_owned())?;
            imported_modules.insert(module, super::modules::source_sha256(&source));
            Ok((source, identity))
        })
        .map_err(|error| VmError::Provider(error.to_string()))?;
        let cache_key = super::compilation_cache::compilation_cache_key(
            super::compilation_cache::CompilationMode::Interactive,
            owner,
            source,
            &expanded,
            &imported_modules,
        );
        let submitted = self.compile_cached(cache_key, || {
            compile_interactive_expanded(&expanded)
                .map_err(|error| VmError::Provider(error.to_string()))
        })?;
        let start = append_interactive_program(&mut program, submitted.as_ref().clone())?;
        process_state.token_position = start;
        process_state.status = ProcessStatus::Ready;
        process_state.wait_reason = WaitReason::None;
        process_state.result = None;
        process_state.error = None;
        process_state.ended_at_unix_ms = None;

        let Some(Value::Array(mut history)) = fields.remove("history") else {
            return Err(invalid_state("Terminal history is malformed"));
        };
        history.push(Value::Text(sanitize_terminal_history_source(source)));
        if history.len() > 100 {
            history.remove(0);
        }
        fields.insert("history".to_owned(), Value::Array(history));
        fields.insert("pending_input".to_owned(), Value::Text(String::new()));
        self.stage_terminal_module_instances(
            terminal,
            process,
            owner,
            &mut fields,
            &imported_modules,
            transaction,
        )?;
        transaction
            .expect(terminal, terminal_view.header().version)
            .expect(process, process_view.header().version)
            .expect(program_id, program_view.header().version)
            .update_state(terminal, Value::Record(fields).encode()?)
            .update_state(process, encode_process_state(&process_state)?)
            .update_state(program_id, program.encode()?);
        Ok(process)
    }

    fn stage_terminal_module_instances(
        &self,
        terminal: ObjectId,
        process: ObjectId,
        owner: SubjectId,
        fields: &mut BTreeMap<String, Value>,
        imported_modules: &BTreeMap<ObjectId, String>,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let existing_instances = match fields.get("module_instances") {
            Some(Value::Array(instances)) => instances
                .iter()
                .map(|value| match value {
                    Value::Text(instance) => instance
                        .parse::<ObjectId>()
                        .map_err(|_| invalid_state("Terminal Module Instance id is malformed")),
                    _ => Err(invalid_state("Terminal Module Instances are malformed")),
                })
                .collect::<Result<BTreeSet<_>, _>>()?,
            None => BTreeSet::new(),
            _ => return Err(invalid_state("Terminal Module Instances are malformed")),
        };
        let mut existing_modules = BTreeSet::new();
        let mut instance_ids = BTreeSet::new();
        let system = AccessContext::new(SYSTEM_SUBJECT);
        for instance in &existing_instances {
            let Value::Record(instance_fields) = self.manager.value(self.context, *instance)?
            else {
                return Err(invalid_state("Module Instance state is not a Record"));
            };
            if instance_fields.get("status") == Some(&Value::Text("active".to_owned())) {
                existing_modules.insert(terminal_field_id(&instance_fields, "module")?);
                instance_ids.insert(*instance);
            }
        }
        for (module, compiled_source_hash) in imported_modules {
            let module_view = self.manager.read(system, *module)?;
            let module_type = module_view.header().type_id;
            let Value::Record(module_fields) = Value::decode(module_view.state())? else {
                return Err(invalid_state("Module state is not a Record"));
            };
            let checked_source = match module_type {
                CORE_MODULE_TYPE => super::modules::checked_module_source(&module_fields)
                    .map_err(VmError::Provider)?,
                CORE_PACKAGE_MODULE_TYPE => self.package_module_source(owner, *module)?,
                _ => return Err(VmError::TypeError("loaded Object is not a Praxis Module")),
            };
            if super::modules::source_sha256(&checked_source) != *compiled_source_hash {
                return Err(VmError::Provider(
                    "Module changed while compiling import".to_owned(),
                ));
            }
            transaction.expect(*module, module_view.header().version);
            if existing_modules.contains(module) {
                continue;
            }
            let instance = ObjectId::new();
            let instance_fields = BTreeMap::from([
                ("module".to_owned(), Value::Text(module.to_string())),
                ("terminal".to_owned(), Value::Text(terminal.to_string())),
                ("process".to_owned(), Value::Text(process.to_string())),
                ("owner".to_owned(), Value::Text(owner.to_string())),
                ("status".to_owned(), Value::Text("active".to_owned())),
            ]);
            transaction
                .create(
                    CreateObject::new(
                        CORE_MODULE_INSTANCE_TYPE,
                        Value::Record(instance_fields).encode()?,
                    )
                    .with_id(instance)
                    .with_parent(terminal)
                    .with_grant(owner, Capability::Inspect)
                    .with_grant(owner, Capability::ViewValue),
                )
                .set_link(*module, format!("instance:{terminal}"), instance);
            instance_ids.insert(instance);
        }
        fields.insert(
            "module_instances".to_owned(),
            Value::Array(
                instance_ids
                    .iter()
                    .map(|instance| Value::Text(instance.to_string()))
                    .collect(),
            ),
        );
        Ok(())
    }

    pub(super) fn terminal_close(
        &self,
        current_process: ObjectId,
        current_state: &mut ProcessState,
        terminal: ObjectId,
        owner: SubjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let terminal_view = self.manager.read(self.context, terminal)?;
        if terminal_view.header().type_id != CORE_TERMINAL_TYPE {
            return Err(VmError::TypeError("Object is not a Terminal"));
        }
        let mut fields = terminal_record(terminal_view.state())?;
        Self::validate_terminal_owner(&fields, owner)?;
        if fields.get("active") != Some(&Value::Bool(true)) {
            return Ok(());
        }

        let process = terminal_field_id(&fields, "process")?;
        let program = terminal_field_id(&fields, "program")?;
        let process_view = self.manager.read(self.context, process)?;
        let mut process_state = if process == current_process {
            current_state.clone()
        } else {
            decode_process_state(process_view.state())?
        };
        if process_state.subject != owner {
            return Err(VmError::TypeError("Terminal Process owner mismatch"));
        }
        if process != current_process
            && matches!(
                process_state.status,
                ProcessStatus::Running
                    | ProcessStatus::Ready
                    | ProcessStatus::Waiting
                    | ProcessStatus::Suspended
            )
        {
            return Err(VmError::TypeError(
                "cannot close a Terminal while its Process is active",
            ));
        }

        let instances = match fields.get("module_instances") {
            Some(Value::Array(instances)) => instances
                .iter()
                .map(|value| match value {
                    Value::Text(id) => id
                        .parse::<ObjectId>()
                        .map_err(|_| invalid_state("Terminal Module Instance id is malformed")),
                    _ => Err(invalid_state("Terminal Module Instances are malformed")),
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(Value::Null) | None => Vec::new(),
            _ => return Err(invalid_state("Terminal Module Instances are malformed")),
        };
        for instance_id in instances {
            let instance_view = self.manager.read(self.context, instance_id)?;
            let Value::Record(mut instance_fields) = Value::decode(instance_view.state())? else {
                return Err(invalid_state("Module Instance state is not a Record"));
            };
            if instance_fields.get("status") != Some(&Value::Text("active".to_owned())) {
                continue;
            }
            let module = terminal_field_id(&instance_fields, "module")?;
            let module_view = self
                .manager
                .read(AccessContext::new(SYSTEM_SUBJECT), module)?;
            transaction
                .expect(module, module_view.header().version)
                .remove_link(module, format!("instance:{terminal}"));
            instance_fields.insert("status".to_owned(), Value::Text("unloaded".to_owned()));
            transaction
                .expect(instance_id, instance_view.header().version)
                .update_state(instance_id, Value::Record(instance_fields).encode()?);
        }

        fields.insert("active".to_owned(), Value::Bool(false));
        fields.insert("pending_input".to_owned(), Value::Text(String::new()));
        fields.insert("module_instances".to_owned(), Value::Array(Vec::new()));
        transaction
            .expect(terminal, terminal_view.header().version)
            .expect(process, process_view.header().version)
            .update_state(terminal, Value::Record(fields).encode()?);

        process_state.status = ProcessStatus::Terminated;
        process_state.stack.clear();
        process_state.frames.clear();
        process_state.handlers.clear();
        process_state.result = None;
        process_state.error = None;
        process_state.wait_reason = WaitReason::None;
        process_state.ended_at_unix_ms = Some(unix_time_millis());
        if process == current_process {
            *current_state = process_state;
        } else {
            transaction.update_state(process, encode_process_state(&process_state)?);
        }

        self.retire_object_as(
            current_process,
            current_state,
            program,
            AccessContext::new(SYSTEM_SUBJECT),
            transaction,
        )?;
        Ok(())
    }

    pub(super) fn terminal_save_input(
        &self,
        terminal: ObjectId,
        owner: SubjectId,
        source: &str,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let view = self.manager.read(self.context, terminal)?;
        let mut fields = terminal_record(view.state())?;
        Self::validate_terminal_owner(&fields, owner)?;
        fields.insert(
            "pending_input".to_owned(),
            Value::Text(sanitize_terminal_history_source(source)),
        );
        transaction
            .expect(terminal, view.header().version)
            .update_state(terminal, Value::Record(fields).encode()?);
        Ok(())
    }

    pub(super) fn terminal_pending_input(
        &self,
        terminal: ObjectId,
        owner: SubjectId,
    ) -> Result<Value, VmError> {
        let fields = self.terminal_fields(terminal)?;
        Self::validate_terminal_owner(&fields, owner)?;
        fields
            .get("pending_input")
            .cloned()
            .ok_or_else(|| invalid_state("Terminal has no pending input"))
    }

    pub(super) fn terminal_update_size(
        &self,
        terminal: ObjectId,
        owner: SubjectId,
        columns: i64,
        rows: i64,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let columns = u16::try_from(columns)
            .ok()
            .filter(|value| *value > 0)
            .ok_or(VmError::TypeError(
                "terminal columns must be from 1 to 65535",
            ))?;
        let rows = u16::try_from(rows)
            .ok()
            .filter(|value| *value > 0)
            .ok_or(VmError::TypeError("terminal rows must be from 1 to 65535"))?;
        let view = self.manager.read(self.context, terminal)?;
        let mut fields = terminal_record(view.state())?;
        Self::validate_terminal_owner(&fields, owner)?;
        fields.insert("columns".to_owned(), Value::Integer(i64::from(columns)));
        fields.insert("rows".to_owned(), Value::Integer(i64::from(rows)));
        transaction
            .expect(terminal, view.header().version)
            .update_state(terminal, Value::Record(fields).encode()?);
        Ok(())
    }

    pub(super) fn terminal_cancel(
        &self,
        terminal: ObjectId,
        owner: SubjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let view = self.manager.read(self.context, terminal)?;
        let mut fields = terminal_record(view.state())?;
        Self::validate_terminal_owner(&fields, owner)?;
        let process = terminal_field_id(&fields, "process")?;
        let process_view = self.manager.read(self.context, process)?;
        let mut process_state = decode_process_state(process_view.state())?;
        process_state.status = ProcessStatus::Halted;
        process_state.stack.clear();
        process_state.frames.clear();
        process_state.handlers.clear();
        process_state.result = None;
        process_state.error = None;
        process_state.wait_reason = WaitReason::None;
        process_state.ended_at_unix_ms = Some(unix_time_millis());
        fields.insert("pending_input".to_owned(), Value::Text(String::new()));
        transaction
            .expect(terminal, view.header().version)
            .expect(process, process_view.header().version)
            .update_state(terminal, Value::Record(fields).encode()?)
            .update_state(process, encode_process_state(&process_state)?);
        Ok(())
    }

    pub(super) fn interrupt_terminal(
        &self,
        terminal: ObjectId,
        owner: SubjectId,
    ) -> Result<(), VmError> {
        let terminal_view = self.manager.read(self.context, terminal)?;
        let mut fields = terminal_record(terminal_view.state())?;
        Self::validate_terminal_owner(&fields, owner)?;
        let process = terminal_field_id(&fields, "process")?;
        let process_view = self.manager.read(self.context, process)?;
        let mut process_state = decode_process_state(process_view.state())?;
        if process_state.subject != owner {
            return Err(VmError::TypeError("Terminal Process owner mismatch"));
        }
        process_state.status = ProcessStatus::Halted;
        process_state.stack.clear();
        process_state.frames.clear();
        process_state.handlers.clear();
        process_state.result = None;
        process_state.error = None;
        process_state.wait_reason = WaitReason::None;
        process_state.ended_at_unix_ms = Some(unix_time_millis());
        fields.insert("pending_input".to_owned(), Value::Text(String::new()));
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(terminal, terminal_view.header().version)
            .expect(process, process_view.header().version)
            .update_state(terminal, Value::Record(fields).encode()?)
            .update_state(process, encode_process_state(&process_state)?);
        self.manager.commit(transaction)?;
        self.notify_process_ended(process)?;
        Ok(())
    }

    fn terminal_fields(&self, terminal: ObjectId) -> Result<BTreeMap<String, Value>, VmError> {
        let view = self.manager.read(self.context, terminal)?;
        if !matches!(view.header().type_id, CORE_TERMINAL_TYPE) {
            return Err(VmError::TypeError("Object is not an interactive Terminal"));
        }
        let fields = terminal_record(view.state())?;
        Ok(fields)
    }

    fn validate_terminal_owner(
        fields: &BTreeMap<String, Value>,
        owner: SubjectId,
    ) -> Result<(), VmError> {
        if fields.get("owner") == Some(&Value::Text(owner.to_string())) {
            Ok(())
        } else {
            Err(VmError::TypeError("Terminal belongs to another user"))
        }
    }
}

fn terminal_record(bytes: &[u8]) -> Result<BTreeMap<String, Value>, VmError> {
    match Value::decode(bytes)? {
        Value::Record(fields) => Ok(fields),
        _ => Err(invalid_state("Terminal state is not a Record")),
    }
}

fn terminal_field_id(fields: &BTreeMap<String, Value>, name: &str) -> Result<ObjectId, VmError> {
    match fields.get(name) {
        Some(Value::Text(value)) => value
            .parse()
            .map_err(|_| invalid_state("Terminal contains an invalid ObjectId")),
        _ => Err(invalid_state("Terminal is missing an ObjectId")),
    }
}

fn append_interactive_program(program: &mut Program, submitted: Program) -> Result<u32, VmError> {
    if !matches!(program.tokens.last(), Some(Token::Halt)) {
        return Err(invalid_state("Terminal Program does not end in Halt"));
    }
    let base = u32::try_from(program.tokens.len().saturating_sub(1))
        .map_err(|_| VmError::TypeError("Terminal Program is too large"))?;
    program.tokens.pop();
    for mut token in submitted.tokens {
        match &mut token {
            Token::Jump(target)
            | Token::JumpIfFalse(target)
            | Token::DefineFunction { end: target, .. }
            | Token::DefineClass { end: target, .. }
            | Token::DefineMethod { end: target, .. }
            | Token::EndTry { end: target }
            | Token::Transaction { end: target } => {
                *target = target.checked_add(base).ok_or(VmError::TypeError(
                    "Terminal Program control-flow target is too large",
                ))?;
            }
            Token::BeginTry { catch, end, .. } => {
                *catch = catch.checked_add(base).ok_or(VmError::TypeError(
                    "Terminal Program control-flow target is too large",
                ))?;
                *end = end.checked_add(base).ok_or(VmError::TypeError(
                    "Terminal Program control-flow target is too large",
                ))?;
            }
            _ => {}
        }
        program.tokens.push(token);
    }
    program.validate()?;
    Ok(base)
}

fn compact_terminal_program(program: &mut Program) -> Result<(), VmError> {
    if !matches!(program.tokens.last(), Some(Token::Halt)) {
        return Err(invalid_state("Terminal Program does not end in Halt"));
    }
    let mut latest = BTreeMap::<(u8, String), (usize, usize)>::new();
    let mut position = 0;
    while position + 1 < program.tokens.len() {
        match &program.tokens[position] {
            Token::DefineFunction { name, end, .. } => {
                let end = usize::try_from(*end)
                    .map_err(|_| invalid_state("Terminal function boundary is invalid"))?;
                if end <= position || end > program.tokens.len() - 1 {
                    return Err(invalid_state("Terminal function boundary is out of range"));
                }
                latest.insert((0, name.clone()), (position, end));
                position = end;
            }
            Token::DefineClass { name, end, .. } => {
                let end = usize::try_from(*end)
                    .map_err(|_| invalid_state("Terminal class boundary is invalid"))?;
                if end <= position || end > program.tokens.len() - 1 {
                    return Err(invalid_state("Terminal class boundary is out of range"));
                }
                latest.insert((1, name.clone()), (position, end));
                position = end;
            }
            _ => position += 1,
        }
    }

    let mut blocks: Vec<_> = latest.into_values().collect();
    blocks.sort_unstable_by_key(|(start, _)| *start);
    let mut remap = vec![None; program.tokens.len()];
    let mut boundaries = BTreeMap::new();
    let mut next = 0_u32;
    for (start, end) in &blocks {
        for position in remap.iter_mut().take(*end).skip(*start) {
            *position = Some(next);
            next = next
                .checked_add(1)
                .ok_or(VmError::TypeError("Terminal Program is too large"))?;
        }
        boundaries.insert(*end, next);
    }
    let capacity = usize::try_from(next)
        .map_err(|_| VmError::TypeError("Terminal Program is too large"))?
        .saturating_add(1);
    let mut compacted = Vec::with_capacity(capacity);
    for (start, end) in blocks {
        for old in start..end {
            let mut token = program.tokens[old].clone();
            relocate_terminal_token(&mut token, &remap, &boundaries)?;
            compacted.push(token);
        }
    }
    compacted.push(Token::Halt);
    program.tokens = compacted;
    program.validate()?;
    Ok(())
}

fn relocate_terminal_token(
    token: &mut Token,
    remap: &[Option<u32>],
    boundaries: &BTreeMap<usize, u32>,
) -> Result<(), VmError> {
    let relocate = |target: &mut u32| -> Result<(), VmError> {
        let old = usize::try_from(*target)
            .map_err(|_| invalid_state("Terminal Program target is invalid"))?;
        *target = remap
            .get(old)
            .and_then(|position| *position)
            .or_else(|| boundaries.get(&old).copied())
            .ok_or_else(|| invalid_state("Terminal Program target leaves a retained definition"))?;
        Ok(())
    };
    match token {
        Token::Jump(target)
        | Token::JumpIfFalse(target)
        | Token::DefineFunction { end: target, .. }
        | Token::DefineClass { end: target, .. }
        | Token::DefineMethod { end: target, .. }
        | Token::EndTry { end: target }
        | Token::Transaction { end: target } => relocate(target)?,
        Token::BeginTry { catch, end, .. } => {
            relocate(catch)?;
            relocate(end)?;
        }
        _ => {}
    }
    Ok(())
}

fn sanitize_terminal_history_source(source: &str) -> String {
    let normalized = source.to_ascii_lowercase();
    let sensitive_markers = [
        "password",
        "passphrase",
        "credential",
        "read_secret",
        "initialize_local(",
        "create_user(",
        "login(",
        "change_password(",
        "private_key",
        "secret:",
    ];
    if sensitive_markers
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        "<redacted sensitive input>".to_owned()
    } else {
        source.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_terminal_history_source;

    #[test]
    fn terminal_history_redacts_secret_bearing_submissions() {
        let sanitized = sanitize_terminal_history_source(
            "authentication.login(\"alice\", \"private-password\")",
        );
        assert_eq!(sanitized, "<redacted sensitive input>");
        assert!(!sanitized.contains("private-password"));
        assert_eq!(
            sanitize_terminal_history_source("terminal.println(\"safe\")"),
            "terminal.println(\"safe\")"
        );
    }
}

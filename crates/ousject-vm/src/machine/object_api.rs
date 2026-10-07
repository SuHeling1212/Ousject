#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    pub(super) fn get_field(
        &self,
        state: &ProcessState,
        receiver: &Value,
        field: &str,
        program: &Program,
    ) -> Result<Value, VmError> {
        if let Value::Map(fields) | Value::Record(fields) = receiver {
            return fields
                .get(field)
                .cloned()
                .ok_or_else(|| VmError::MissingKey(field.to_owned()));
        }

        let object = object_id(receiver)?;
        let view = self.manager.read(self.context, object)?;
        if let Ok(value) = Value::decode(view.state()) {
            Self::check_field_visibility(state, object, field, program, &value)?;
            if let Value::Map(fields) | Value::Record(fields) = &value {
                if let Some(value) = fields.get(field) {
                    return Ok(value.clone());
                }
            }
        }

        if let Some(value) = self.object_property(object, &view, field, program)? {
            return Ok(value);
        }

        if view.header().type_id == PROCESS_TYPE {
            let process = decode_process_state(view.state())?;
            if let Some(variable) = process.variables.get(field) {
                return self
                    .manager
                    .value(self.context, *variable)
                    .map_err(Into::into);
            }
        }

        Err(VmError::MissingKey(field.to_owned()))
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn object_property(
        &self,
        object: ObjectId,
        view: &oms_runtime::ObjectView,
        field: &str,
        program: &Program,
    ) -> Result<Option<Value>, VmError> {
        let header = view.header();
        let descriptor = self.manager.type_by_id(header.type_id)?;
        let type_name = if header.type_id == INSTANCE_TYPE {
            instance_class(&self.manager.value(self.context, object)?)?
        } else {
            descriptor.name.clone()
        };
        let value = match field {
            "id" => Value::Text(object.to_string()),
            "type" => Value::Text(type_name.clone()),
            "parent" => header
                .parent_id
                .map_or(Value::Null, |id| Value::Text(id.to_string())),
            "status" => {
                if header.type_id == PROCESS_TYPE {
                    Value::Text(
                        process_status_name(decode_process_state(view.state())?.status).to_owned(),
                    )
                } else {
                    Value::Text(format!("{:?}", header.lifecycle))
                }
            }
            "result" if header.type_id == PROCESS_TYPE => decode_process_state(view.state())?
                .result
                .unwrap_or(Value::Null),
            "error" if header.type_id == PROCESS_TYPE => decode_process_state(view.state())?
                .error
                .unwrap_or(Value::Null),
            "version" => Value::Text(header.version.get().to_string()),
            "owner" => Value::Text(view.owner().to_string()),
            "permissions" => {
                self.manager
                    .require_capability(self.context, object, Capability::ManagePolicy)?;
                Value::Record(BTreeMap::from([
                    ("owner".to_owned(), Value::Text(view.owner().to_string())),
                    (
                        "grants".to_owned(),
                        Value::Record(
                            view.grants()
                                .iter()
                                .map(|(subject, capabilities)| {
                                    (
                                        subject.to_string(),
                                        Value::Array(
                                            capabilities
                                                .iter()
                                                .map(|capability| {
                                                    Value::Text(format!("{capability:?}"))
                                                })
                                                .collect(),
                                        ),
                                    )
                                })
                                .collect(),
                        ),
                    ),
                ]))
            }
            "inspect" => Value::Record(BTreeMap::from([
                ("id".to_owned(), Value::Text(object.to_string())),
                ("type".to_owned(), Value::Text(type_name)),
                (
                    "parent".to_owned(),
                    header
                        .parent_id
                        .map_or(Value::Null, |id| Value::Text(id.to_string())),
                ),
                (
                    "version".to_owned(),
                    Value::Text(header.version.get().to_string()),
                ),
                (
                    "status".to_owned(),
                    if header.type_id == PROCESS_TYPE {
                        Value::Text(
                            process_status_name(decode_process_state(view.state())?.status)
                                .to_owned(),
                        )
                    } else {
                        Value::Text(format!("{:?}", header.lifecycle))
                    },
                ),
            ])),
            "capabilities" => {
                let mut capabilities: Vec<Value> = view
                    .capabilities()
                    .iter()
                    .map(|capability| Value::Text(format!("{capability:?}")))
                    .collect();
                capabilities.extend(
                    self.effective_domain_capabilities(header.type_id, &descriptor)
                        .into_iter()
                        .map(Value::Text),
                );
                if header.type_id == INSTANCE_TYPE {
                    for method in public_methods(program, &type_name)? {
                        let value = Value::Text(method);
                        if !capabilities.contains(&value) {
                            capabilities.push(value);
                        }
                    }
                }
                Value::Array(capabilities)
            }
            "value" => object_essential_value(view)?,
            "children" => Value::Array(
                view.children()
                    .iter()
                    .map(|id| Value::Text(id.to_string()))
                    .collect(),
            ),
            "links" => Value::Map(
                view.links()
                    .iter()
                    .map(|(name, target)| (name.clone(), Value::Text(target.to_string())))
                    .collect(),
            ),
            "variables" if header.type_id == PROCESS_TYPE => {
                let process = decode_process_state(view.state())?;
                let mut variables = BTreeMap::new();
                for (name, object) in process.variables {
                    variables.insert(name, self.manager.value(self.context, object)?);
                }
                Value::Record(variables)
            }
            "program" if header.type_id == PROCESS_TYPE => {
                Value::Text(decode_process_state(view.state())?.program.to_string())
            }
            "user" | "subject" if header.type_id == PROCESS_TYPE => {
                Value::Text(decode_process_state(view.state())?.subject.to_string())
            }
            "position" if header.type_id == PROCESS_TYPE => Value::Integer(i64::from(
                decode_process_state(view.state())?.token_position,
            )),
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    pub(super) fn effective_domain_capabilities(
        &self,
        type_id: TypeId,
        descriptor: &TypeDescriptor,
    ) -> BTreeSet<String> {
        let provider_backed_device = matches!(
            type_id,
            CORE_TERMINAL_TYPE
                | NET_ENDPOINT_TYPE
                | NET_RESOLVER_TYPE
                | DEVICE_DISPLAY_TYPE
                | DEVICE_SENSOR_TYPE
                | DEVICE_KEYBOARD_TYPE
                | DEVICE_BLOCK_STORAGE_TYPE
        );
        if !provider_backed_device {
            return descriptor.domain_capabilities.clone();
        }

        let mut supported = BTreeSet::new();
        if let Ok(provider) = self.providers.get(type_id) {
            supported.extend(provider.capabilities());
            supported.extend(provider.ephemeral_capabilities());
        }
        if type_id == CORE_TERMINAL_TYPE {
            // Shell state methods are VM intrinsics stored on Terminal
            // Objects; they are not host Provider operations.
            supported.extend(
                [
                    "shell",
                    "submit",
                    "process",
                    "history",
                    "pending_input",
                    "save_input",
                    "update_size",
                    "cancel",
                    "close",
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        supported.retain(|capability| descriptor.domain_capabilities.contains(capability));
        supported
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn commit_field(
        &self,
        process: ObjectId,
        process_version: oms_types::ObjectVersion,
        state: &ProcessState,
        receiver: &Value,
        field: &str,
        value: Value,
        program: &Program,
    ) -> Result<(), VmError> {
        let object = object_id(receiver)?;
        let view = self.manager.read(self.context, object)?;
        let current = self.manager.value(self.context, object)?;
        Self::check_field_visibility(state, object, field, program, &current)?;
        let (Value::Map(mut fields) | Value::Record(mut fields)) = current else {
            return Err(VmError::TypeError("Object value has no fields"));
        };
        if !fields.contains_key(field) {
            return Err(VmError::MissingKey(field.to_owned()));
        }
        fields.insert(field.to_owned(), value);
        let replacement = Value::Record(fields).encode()?;
        if view.header().type_id == CORE_PACKAGE_DATA_TYPE
            && replacement.len() > self.package_data_quota_for_object(object)?
        {
            return Err(VmError::TypeError(
                "Package Data exceeds this Package's quota",
            ));
        }
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, process_version)
            .expect(object, view.header().version)
            .update_state(object, replacement)
            .update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(())
    }

    pub(super) fn check_field_visibility(
        state: &ProcessState,
        object: ObjectId,
        field: &str,
        program: &Program,
        value: &Value,
    ) -> Result<(), VmError> {
        let Ok(class) = instance_class(value) else {
            return Ok(());
        };
        if let Some(owner) = private_field_owner(program, &class, field)? {
            let internal = state.frames.last().is_some_and(|frame| {
                frame.receiver == Some(object) && frame.class.as_deref() == Some(&owner)
            });
            if !internal {
                return Err(VmError::TypeError("private field is not visible"));
            }
        }
        Ok(())
    }

    pub(super) fn query_objects(
        &self,
        program_id: ObjectId,
        type_name: &str,
        capability: Option<&str>,
    ) -> Result<Value, VmError> {
        let (type_id, class_filter) = match self.manager.type_by_name(type_name) {
            Ok(descriptor) => (descriptor.id, None),
            Err(OmsError::UnknownTypeName(_)) => {
                let program =
                    Program::decode(self.manager.read(self.context, program_id)?.state())?;
                class_definition(&program, type_name)?;
                (INSTANCE_TYPE, Some(type_name))
            }
            Err(error) => return Err(error.into()),
        };
        let mut query = ObjectQuery::new().with_type(type_id);
        if class_filter.is_none() {
            if let Some(capability) = capability {
                query = query.with_domain_capability(capability);
            }
        } else if let (Some(class), Some(capability)) = (class_filter, capability) {
            let program = Program::decode(self.manager.read(self.context, program_id)?.state())?;
            if !public_methods(&program, class)?
                .iter()
                .any(|method| method == capability)
            {
                return Ok(Value::Array(Vec::new()));
            }
        }
        let headers = self.manager.query(self.context, &query)?;
        let mut objects = Vec::new();
        for header in headers {
            if let Some(class) = class_filter {
                let value = self.manager.value(self.context, header.id)?;
                if instance_class(&value)?.as_str() != class {
                    continue;
                }
            }
            objects.push(Value::Text(header.id.to_string()));
        }
        Ok(Value::Array(objects))
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn object_operation(
        &self,
        program_id: ObjectId,
        method: &str,
        args: &[Value],
        transaction: &mut Transaction,
    ) -> Result<Value, VmError> {
        let (identity, args) = args
            .split_first()
            .ok_or(VmError::TypeError("Object operation requires an ID"))?;
        let object = object_id(identity)?;
        let view = self.manager.read(self.context, object)?;
        let descriptor = self.manager.type_by_id(view.header().type_id)?;
        let no_args = args.is_empty();
        match method {
            "id" if no_args => Ok(Value::Text(object.to_string())),
            "type" if no_args => {
                if view.header().type_id == INSTANCE_TYPE {
                    Ok(Value::Text(instance_class(
                        &self.manager.value(self.context, object)?,
                    )?))
                } else {
                    Ok(Value::Text(descriptor.name.clone()))
                }
            }
            "parent" if no_args => Ok(view
                .header()
                .parent_id
                .map_or(Value::Null, |id| Value::Text(id.to_string()))),
            "status" if no_args => {
                if view.header().type_id == PROCESS_TYPE {
                    Ok(Value::Text(
                        process_status_name(decode_process_state(view.state())?.status).to_owned(),
                    ))
                } else {
                    Ok(Value::Text(format!("{:?}", view.header().lifecycle)))
                }
            }
            "inspect" if no_args => {
                let header = view.header();
                let type_name = if header.type_id == INSTANCE_TYPE {
                    instance_class(&self.manager.value(self.context, object)?)?
                } else {
                    descriptor.name.clone()
                };
                Ok(Value::Record(BTreeMap::from([
                    ("id".to_owned(), Value::Text(object.to_string())),
                    ("type".to_owned(), Value::Text(type_name)),
                    (
                        "parent".to_owned(),
                        header
                            .parent_id
                            .map_or(Value::Null, |id| Value::Text(id.to_string())),
                    ),
                    (
                        "version".to_owned(),
                        Value::Text(header.version.get().to_string()),
                    ),
                    (
                        "status".to_owned(),
                        if header.type_id == PROCESS_TYPE {
                            Value::Text(
                                process_status_name(decode_process_state(view.state())?.status)
                                    .to_owned(),
                            )
                        } else {
                            Value::Text(format!("{:?}", header.lifecycle))
                        },
                    ),
                ])))
            }
            "capabilities" if no_args => {
                let mut capabilities: Vec<Value> = view
                    .capabilities()
                    .iter()
                    .map(|capability| Value::Text(format!("{capability:?}")))
                    .collect();
                capabilities.extend(
                    descriptor
                        .domain_capabilities
                        .iter()
                        .cloned()
                        .map(Value::Text),
                );
                if view.header().type_id == INSTANCE_TYPE {
                    let class = instance_class(&self.manager.value(self.context, object)?)?;
                    let program =
                        Program::decode(self.manager.read(self.context, program_id)?.state())?;
                    for method in public_methods(&program, &class)? {
                        let value = Value::Text(method);
                        if !capabilities.contains(&value) {
                            capabilities.push(value);
                        }
                    }
                }
                Ok(Value::Array(capabilities))
            }
            "value" if no_args => self.manager.value(self.context, object).map_err(Into::into),
            "children" if no_args => Ok(Value::Array(
                view.children()
                    .iter()
                    .map(|id| Value::Text(id.to_string()))
                    .collect(),
            )),
            "links" if no_args => Ok(Value::Map(
                view.links()
                    .iter()
                    .map(|(name, target)| (name.clone(), Value::Text(target.to_string())))
                    .collect(),
            )),
            "replace" if args.len() == 1 => {
                if view.header().type_id == CORE_USER_TYPE
                    && is_local_user_value(&self.manager.value(self.context, object)?)
                {
                    return Err(VmError::TypeError(
                        "the reserved local User can only change through authentication",
                    ));
                }
                let (version, encoded) =
                    self.manager
                        .prepare_replace_value(self.context, object, &args[0])?;
                if view.header().type_id == CORE_PACKAGE_DATA_TYPE
                    && encoded.len() > self.package_data_quota_for_object(object)?
                {
                    return Err(VmError::TypeError(
                        "Package Data exceeds this Package's quota",
                    ));
                }
                transaction
                    .expect(object, version)
                    .update_state(object, encoded);
                Ok(Value::Null)
            }
            "link" if args.len() == 2 => {
                let Value::Text(name) = &args[0] else {
                    return Err(VmError::TypeError("link name must be text"));
                };
                let target = object_id(&args[1])?;
                transaction.expect(object, view.header().version).set_link(
                    object,
                    name.clone(),
                    target,
                );
                Ok(Value::Null)
            }
            "unlink" if args.len() == 1 => {
                let Value::Text(name) = &args[0] else {
                    return Err(VmError::TypeError("link name must be text"));
                };
                transaction
                    .expect(object, view.header().version)
                    .remove_link(object, name.clone());
                Ok(Value::Null)
            }
            "grant" | "revoke" if args.len() == 2 => {
                let Value::Text(subject) = &args[0] else {
                    return Err(VmError::TypeError("subject must be hexadecimal text"));
                };
                let subject = subject
                    .parse::<SubjectId>()
                    .map_err(|_| VmError::TypeError("invalid hexadecimal SubjectId"))?;
                let Value::Text(capability_name) = &args[1] else {
                    return Err(VmError::TypeError("capability name must be text"));
                };
                let capability = capability_from_name(capability_name)?;
                transaction.expect(object, view.header().version);
                if method == "grant" {
                    transaction.grant(object, subject, capability);
                } else {
                    transaction.revoke(object, subject, capability);
                }
                self.stage_audit_event(
                    self.context.subject,
                    if method == "grant" {
                        "permission.grant"
                    } else {
                        "permission.revoke"
                    },
                    object,
                    Value::Record(BTreeMap::from([
                        ("subject".to_owned(), Value::Text(subject.to_string())),
                        (
                            "capability".to_owned(),
                            Value::Text(capability_name.clone()),
                        ),
                    ])),
                    transaction,
                )?;
                Ok(Value::Null)
            }
            _ => Err(VmError::TypeError("invalid Object method or arguments")),
        }
    }

    pub(super) fn variable_from_state(
        &self,
        state: &ProcessState,
        name: &str,
    ) -> Result<Value, VmError> {
        let object = binding_id(state, name)?;
        let view = self.manager.read(self.context, object)?;
        decode_value_state(view.state())
    }

    pub(super) fn commit_process(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &ProcessState,
    ) -> Result<(), VmError> {
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, version)
            .update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(())
    }

    pub(super) fn commit_store(
        &self,
        process: ObjectId,
        process_version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        name: String,
        value: &Value,
    ) -> Result<(), VmError> {
        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, process_version);
        let existing = state
            .frames
            .last()
            .and_then(|frame| frame.locals.get(&name))
            .or_else(|| state.variables.get(&name))
            .copied();
        if let Some(variable) = existing {
            let variable_view = self.manager.read(self.context, variable)?;
            let encoded = self
                .manager
                .prepare_replace_value(self.context, variable, value)?
                .1;
            transaction
                .expect(variable, variable_view.header().version)
                .update_state(variable, encoded);
        } else {
            let mut request = self.manager.prepare_create(
                CreateSpec::new("core.value", value.clone()).with_parent(process),
            )?;
            while self.manager.shard_for(request.id) != self.manager.shard_for(process) {
                request.id = ObjectId::new();
            }
            request = request.with_parent(process);
            bind_name(state, name, request.id);
            transaction.create(request);
        }
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(())
    }
}

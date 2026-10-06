#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    #[allow(clippy::too_many_lines)]
    pub(super) fn step_transaction(
        &self,
        process: ObjectId,
        process_version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        program: &Program,
        start: u32,
        end: u32,
    ) -> Result<Option<String>, VmError> {
        let start = usize::try_from(start)
            .map_err(|_| VmError::TypeError("transaction position is too large"))?;
        let commit = usize::try_from(end)
            .map_err(|_| VmError::TypeError("transaction position is too large"))?
            .checked_sub(1)
            .ok_or(VmError::TypeError("invalid transaction range"))?;
        if !matches!(program.tokens.get(commit), Some(Token::CommitTransaction)) {
            return Err(VmError::TypeError("invalid transaction commit marker"));
        }

        let mut stack = Vec::new();
        let mut staged = BTreeMap::<ObjectId, Value>::new();
        let mut created = BTreeMap::<ObjectId, Value>::new();
        for token in program
            .tokens
            .get(start..commit)
            .ok_or(VmError::TypeError("invalid transaction range"))?
        {
            match token {
                Token::Push(value) => stack.push(value.clone()),
                Token::Load(name) => {
                    let object = binding_id(state, name)?;
                    stack.push(self.staged_value(object, &staged, &created)?);
                }
                Token::LoadIdentity(name) => {
                    stack.push(Value::Text(binding_id(state, name)?.to_string()));
                }
                Token::Store(name) => {
                    let value = stack.pop().ok_or(VmError::StackUnderflow)?;
                    let existing = state
                        .frames
                        .last()
                        .and_then(|frame| frame.locals.get(name))
                        .or_else(|| state.variables.get(name))
                        .copied();
                    if let Some(object) = existing {
                        if let Some(current) = created.get_mut(&object) {
                            *current = value;
                        } else {
                            staged.insert(object, value);
                        }
                    } else {
                        let mut request = self.manager.prepare_create(
                            CreateSpec::new("core.value", value.clone()).with_parent(process),
                        )?;
                        while self.manager.shard_for(request.id) != self.manager.shard_for(process)
                        {
                            request.id = ObjectId::new();
                        }
                        let object = request.id;
                        bind_name(state, name.clone(), object);
                        created.insert(object, value);
                    }
                }
                Token::Add | Token::Subtract | Token::Multiply | Token::Divide | Token::Modulo => {
                    let right = stack.pop().ok_or(VmError::StackUnderflow)?;
                    let left = stack.pop().ok_or(VmError::StackUnderflow)?;
                    stack.push(arithmetic(token, left, right)?);
                }
                Token::Equal
                | Token::NotEqual
                | Token::Less
                | Token::LessEqual
                | Token::Greater
                | Token::GreaterEqual => {
                    let right = stack.pop().ok_or(VmError::StackUnderflow)?;
                    let left = stack.pop().ok_or(VmError::StackUnderflow)?;
                    stack.push(Value::Bool(compare(token, &left, &right)?));
                }
                Token::Not => {
                    let value = stack.pop().ok_or(VmError::StackUnderflow)?;
                    stack.push(Value::Bool(!value.is_truthy()));
                }
                Token::MakeArray(_)
                | Token::MakeMap(_)
                | Token::IndexGet
                | Token::IndexSet
                | Token::IndexIncrement
                | Token::IndexDecrement
                | Token::Length => execute_collection_token(token, &mut stack)?,
                Token::GetField(field) => {
                    let receiver = object_id(&stack.pop().ok_or(VmError::StackUnderflow)?)?;
                    let value = self.staged_value(receiver, &staged, &created)?;
                    Self::check_field_visibility(state, receiver, field, program, &value)?;
                    let (Value::Map(fields) | Value::Record(fields)) = value else {
                        return Err(VmError::TypeError("Object value has no fields"));
                    };
                    stack.push(
                        fields
                            .get(field)
                            .cloned()
                            .ok_or_else(|| VmError::MissingKey(field.clone()))?,
                    );
                }
                Token::SetField(field) => {
                    let value = stack.pop().ok_or(VmError::StackUnderflow)?;
                    let receiver = object_id(&stack.pop().ok_or(VmError::StackUnderflow)?)?;
                    let current = self.staged_value(receiver, &staged, &created)?;
                    Self::check_field_visibility(state, receiver, field, program, &current)?;
                    let (Value::Map(mut fields) | Value::Record(mut fields)) = current else {
                        return Err(VmError::TypeError("Object value has no fields"));
                    };
                    if !fields.contains_key(field) {
                        return Err(VmError::MissingKey(field.clone()));
                    }
                    fields.insert(field.clone(), value);
                    let replacement = Value::Record(fields);
                    if let Some(current) = created.get_mut(&receiver) {
                        *current = replacement;
                    } else {
                        staged.insert(receiver, replacement);
                    }
                }
                Token::BindLink { name, target } => {
                    let target = binding_id(state, target)?;
                    bind_name(state, name.clone(), target);
                }
                _ => {
                    return Err(VmError::TypeError(
                        "token is not allowed in an atomic transaction",
                    ));
                }
            }
        }
        if !stack.is_empty() {
            return Err(VmError::TypeError(
                "transaction statements left temporary values",
            ));
        }

        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, process_version);
        for (object, value) in created {
            let mut request = self
                .manager
                .prepare_create(CreateSpec::new("core.value", value).with_parent(process))?;
            request.id = object;
            transaction.create(request);
        }
        for (object, value) in staged {
            let view = self.manager.read(self.context, object)?;
            let encoded = if view.header().type_id == INSTANCE_TYPE {
                value.encode()?
            } else {
                self.manager
                    .prepare_replace_value(self.context, object, &value)?
                    .1
            };
            if view.header().type_id == CORE_PACKAGE_DATA_TYPE
                && encoded.len() > self.package_data_quota_for_object(object)?
            {
                return Err(VmError::TypeError(
                    "Package Data exceeds this Package's quota",
                ));
            }
            transaction
                .expect(object, view.header().version)
                .update_state(object, encoded);
        }
        state.token_position = end;
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(None)
    }

    pub(super) fn staged_value(
        &self,
        object: ObjectId,
        staged: &BTreeMap<ObjectId, Value>,
        created: &BTreeMap<ObjectId, Value>,
    ) -> Result<Value, VmError> {
        staged
            .get(&object)
            .or_else(|| created.get(&object))
            .cloned()
            .map_or_else(
                || self.manager.value(self.context, object).map_err(Into::into),
                Ok,
            )
    }
}

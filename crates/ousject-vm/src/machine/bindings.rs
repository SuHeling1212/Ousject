fn pop_call_arguments(stack: &mut Vec<Value>, count: u32) -> Result<Vec<Value>, VmError> {
    let count = usize::try_from(count).map_err(|_| VmError::StackUnderflow)?;
    let start = stack
        .len()
        .checked_sub(count)
        .ok_or(VmError::StackUnderflow)?;
    Ok(stack.split_off(start))
}

#[derive(Debug, Clone)]
struct ClassDefinition {
    parent: Option<String>,
    fields: BTreeMap<String, Value>,
    private_fields: Vec<String>,
}

#[derive(Debug, Clone)]
struct MethodDefinition {
    class: String,
    parameters: Vec<String>,
    public: bool,
    entry: u32,
}

fn binding_id(state: &ProcessState, name: &str) -> Result<ObjectId, VmError> {
    if matches!(name, "this" | "super") {
        return state
            .frames
            .last()
            .and_then(|frame| frame.receiver)
            .ok_or(VmError::TypeError("this or super outside method"));
    }
    state
        .frames
        .last()
        .and_then(|frame| frame.locals.get(name))
        .or_else(|| state.variables.get(name))
        .copied()
        .ok_or_else(|| VmError::UndefinedVariable(name.to_owned()))
}

fn bind_name(state: &mut ProcessState, name: String, object: ObjectId) {
    if let Some(frame) = state.frames.last_mut() {
        frame.locals.insert(name, object);
    } else {
        state.variables.insert(name, object);
    }
}

fn remove_retired_bindings(state: &mut ProcessState, retired: &BTreeSet<ObjectId>) -> bool {
    let mut changed = false;
    state.variables.retain(|_, object| {
        let keep = !retired.contains(object);
        changed |= !keep;
        keep
    });
    for frame in &mut state.frames {
        frame.locals.retain(|_, object| {
            let keep = !retired.contains(object);
            changed |= !keep;
            keep
        });
    }
    changed
}

fn find_function(
    program: &Program,
    name: &str,
    arity: usize,
) -> Result<(u32, Vec<String>), VmError> {
    let found = program
        .tokens
        .iter()
        .enumerate()
        .rev()
        .find_map(|(position, token)| match token {
            Token::DefineFunction {
                name: candidate,
                parameters,
                ..
            } if candidate == name && parameters.len() == arity => Some((position, parameters)),
            _ => None,
        });
    let (position, parameters) =
        found.ok_or(VmError::TypeError("unknown function or argument count"))?;
    let entry = u32::try_from(position + 1)
        .map_err(|_| VmError::TypeError("function position is too large"))?;
    Ok((entry, parameters.clone()))
}

fn class_definition(program: &Program, name: &str) -> Result<ClassDefinition, VmError> {
    program
        .tokens
        .iter()
        .rev()
        .find_map(|token| match token {
            Token::DefineClass {
                name: candidate,
                parent,
                fields,
                private_fields,
                ..
            } if candidate == name => Some(ClassDefinition {
                parent: parent.clone(),
                fields: fields.clone(),
                private_fields: private_fields.clone(),
            }),
            _ => None,
        })
        .ok_or(VmError::TypeError("unknown class"))
}

fn collect_class_fields(program: &Program, name: &str) -> Result<BTreeMap<String, Value>, VmError> {
    fn collect(
        program: &Program,
        name: &str,
        depth: usize,
        output: &mut BTreeMap<String, Value>,
    ) -> Result<(), VmError> {
        if depth > 64 {
            return Err(VmError::TypeError("class inheritance is too deep"));
        }
        let definition = class_definition(program, name)?;
        if let Some(parent) = definition.parent {
            collect(program, &parent, depth + 1, output)?;
        }
        output.extend(definition.fields);
        Ok(())
    }
    let mut fields = BTreeMap::new();
    collect(program, name, 0, &mut fields)?;
    Ok(fields)
}

fn find_method(
    program: &Program,
    class: &str,
    method: &str,
    arity: usize,
) -> Result<MethodDefinition, VmError> {
    let mut current = Some(class.to_owned());
    for _ in 0..=64 {
        let class_name = current
            .take()
            .ok_or(VmError::TypeError("unknown method or argument count"))?;
        if let Some((position, parameters, public)) = program
            .tokens
            .iter()
            .enumerate()
            .rev()
            .find_map(|(position, token)| match token {
                Token::DefineMethod {
                    class,
                    name,
                    parameters,
                    public,
                    ..
                } if class == &class_name && name == method && parameters.len() == arity => {
                    Some((position, parameters.clone(), *public))
                }
                _ => None,
            })
        {
            return Ok(MethodDefinition {
                class: class_name,
                parameters,
                public,
                entry: u32::try_from(position + 1)
                    .map_err(|_| VmError::TypeError("method position is too large"))?,
            });
        }
        current = class_definition(program, &class_name)?.parent;
    }
    Err(VmError::TypeError("class inheritance is too deep"))
}

fn public_methods(program: &Program, class: &str) -> Result<Vec<String>, VmError> {
    let mut current = Some(class.to_owned());
    let mut seen = BTreeSet::new();
    let mut methods = Vec::new();
    for _ in 0..=64 {
        let Some(class_name) = current.take() else {
            return Ok(methods);
        };
        for token in program.tokens.iter().rev() {
            if let Token::DefineMethod {
                class,
                name,
                public,
                ..
            } = token
            {
                if class == &class_name && seen.insert(name.clone()) && *public {
                    methods.push(name.clone());
                }
            }
        }
        current = class_definition(program, &class_name)?.parent;
    }
    Err(VmError::TypeError("class inheritance is too deep"))
}

fn private_field_owner(
    program: &Program,
    class: &str,
    field: &str,
) -> Result<Option<String>, VmError> {
    let mut current = Some(class.to_owned());
    for _ in 0..=64 {
        let Some(class_name) = current.take() else {
            return Ok(None);
        };
        let definition = class_definition(program, &class_name)?;
        if definition.private_fields.iter().any(|name| name == field) {
            return Ok(Some(class_name));
        }
        current = definition.parent;
    }
    Err(VmError::TypeError("class inheritance is too deep"))
}

fn instance_class(value: &Value) -> Result<String, VmError> {
    let Value::Record(fields) = value else {
        return Err(VmError::TypeError("invalid class instance state"));
    };
    match fields.get("$class") {
        Some(Value::Text(class)) => Ok(class.clone()),
        _ => Err(VmError::TypeError("class instance has no class")),
    }
}

fn object_id(value: &Value) -> Result<ObjectId, VmError> {
    match value {
        Value::Text(id) => id
            .parse()
            .map_err(|_| VmError::TypeError("invalid hexadecimal ObjectId")),
        _ => Err(VmError::TypeError("expected hexadecimal ObjectId text")),
    }
}

fn validate_namespace_name(name: &str) -> Result<(), VmError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        Err(VmError::TypeError("invalid Namespace name"))
    } else {
        Ok(())
    }
}

fn is_base_object_operation(method: &str) -> bool {
    matches!(
        method,
        "replace" | "link" | "unlink" | "grant" | "revoke" | "retire"
    )
}

fn capability_from_name(value: &str) -> Result<Capability, VmError> {
    match value {
        "view_value" => Ok(Capability::ViewValue),
        "replace_value" => Ok(Capability::ReplaceValue),
        "create_child" => Ok(Capability::CreateChild),
        "invoke" => Ok(Capability::Invoke),
        "link" => Ok(Capability::Link),
        "reparent" => Ok(Capability::Reparent),
        "retire" => Ok(Capability::Retire),
        "inspect" => Ok(Capability::Inspect),
        "manage_policy" => Ok(Capability::ManagePolicy),
        _ => Err(VmError::TypeError("unknown capability name")),
    }
}

const fn process_status_name(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Running => "running",
        ProcessStatus::Suspended => "suspended",
        ProcessStatus::Halted => "halted",
        ProcessStatus::Terminated => "terminated",
        ProcessStatus::Failed => "failed",
    }
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn count_integer(value: usize) -> Result<i64, VmError> {
    i64::try_from(value).map_err(|_| VmError::TypeError("count exceeds Praxis Integer"))
}

fn counter_integer(value: u64) -> Result<i64, VmError> {
    i64::try_from(value).map_err(|_| VmError::TypeError("performance counter exceeds Integer"))
}

fn type_descriptor_value(descriptor: &TypeDescriptor) -> Value {
    Value::Record(BTreeMap::from([
        ("id".to_owned(), Value::Text(descriptor.id.to_string())),
        ("name".to_owned(), Value::Text(descriptor.name.clone())),
        (
            "schema".to_owned(),
            Value::Text(format!("{:?}", descriptor.schema).to_lowercase()),
        ),
        (
            "creation".to_owned(),
            Value::Text(format!("{:?}", descriptor.creation).to_lowercase()),
        ),
        (
            "capabilities".to_owned(),
            Value::Array(
                descriptor
                    .domain_capabilities
                    .iter()
                    .cloned()
                    .map(Value::Text)
                    .collect(),
            ),
        ),
    ]))
}

fn user_identity_value(identity: &UserIdentity) -> Value {
    Value::Record(BTreeMap::from([
        (
            "object".to_owned(),
            Value::Text(identity.object.to_string()),
        ),
        (
            "subject".to_owned(),
            Value::Text(identity.subject.to_string()),
        ),
        ("name".to_owned(), Value::Text(identity.name.clone())),
    ]))
}

fn is_local_user_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Record(fields)
            if fields.get("name") == Some(&Value::Text(LOCAL_USER_NAME.to_owned()))
                && fields.get("subject")
                    == Some(&Value::Text(SYSTEM_SUBJECT.to_string()))
    )
}

use super::super::{
    AccessContext, BTreeSet, Capability, CreateSpec, CreationPolicy, ObjectQuery, Program,
    SYSTEM_SUBJECT, SubjectId, Value, ValueSchema, error_text, open_manager, option_subject,
    parse_object, parse_options,
};

pub(crate) fn command_inspect(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 1 {
        return Err("usage: ousject inspect <object-id> [options]".to_owned());
    }
    let object = parse_object(&positional[0])?;
    let manager = open_manager(options.state.as_deref())?;
    let context = AccessContext::new(option_subject(&manager, &options)?);
    let header = manager.inspect(context, object).map_err(error_text)?;
    println!(
        "id={} type={} version={} lifecycle={:?} parent={}",
        header.id,
        header.type_id,
        header.version.get(),
        header.lifecycle,
        header
            .parent_id
            .map_or_else(|| "none".to_owned(), |id| id.to_string())
    );
    Ok(())
}

pub(crate) fn command_list(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if !positional.is_empty() {
        return Err("usage: ousject list [options]".to_owned());
    }
    let manager = open_manager(options.state.as_deref())?;
    let context = AccessContext::new(option_subject(&manager, &options)?);
    for header in manager.list(context).map_err(error_text)? {
        println!(
            "{} type={} version={} lifecycle={:?}",
            header.id,
            header.type_id,
            header.version.get(),
            header.lifecycle
        );
    }
    Ok(())
}

pub(crate) fn command_check(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if !positional.is_empty() {
        return Err("usage: ousject check [options]".to_owned());
    }
    let manager = open_manager(options.state.as_deref())?;
    manager.health_check().map_err(error_text)?;
    let stats = manager.stats().map_err(error_text)?;
    println!(
        "ok: objects={} active={} tombstoned={} shards={}",
        stats.object_count, stats.active_count, stats.tombstoned_count, stats.shard_count
    );
    Ok(())
}

pub(crate) fn command_types(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if !positional.is_empty() {
        return Err("usage: ousject types [options]".to_owned());
    }
    let manager = open_manager(options.state.as_deref())?;
    for descriptor in manager.types().map_err(error_text)? {
        println!(
            "{} id={} schema={:?} creation={:?} capabilities={}",
            descriptor.name,
            descriptor.id,
            descriptor.schema,
            descriptor.creation,
            descriptor
                .domain_capabilities
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    Ok(())
}

pub(crate) fn command_type_register(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if !(3..=4).contains(&positional.len()) || options.session.is_some() {
        return Err(
            "usage: ousject type-register <name> <schema> <public|provider-only> [capability,...] [--state <path>]"
                .to_owned(),
        );
    }
    let schema = match positional[1].as_str() {
        "any" => ValueSchema::Any,
        "text" => ValueSchema::Text,
        "bytes" => ValueSchema::Bytes,
        "collection" => ValueSchema::Collection,
        "record" => ValueSchema::Record,
        _ => return Err("schema must be any, text, bytes, collection or record".to_owned()),
    };
    let creation = match positional[2].as_str() {
        "public" => CreationPolicy::Public,
        "provider-only" => CreationPolicy::ProviderOnly,
        _ => return Err("creation must be public or provider-only".to_owned()),
    };
    let capabilities = positional.get(3).map_or_else(BTreeSet::new, |value| {
        value
            .split(',')
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    });
    let manager = open_manager(options.state.as_deref())?;
    let descriptor = manager
        .register_type(
            AccessContext::new(SYSTEM_SUBJECT),
            &positional[0],
            schema,
            creation,
            capabilities,
        )
        .map_err(error_text)?;
    println!("{} id={}", descriptor.name, descriptor.id);
    Ok(())
}

pub(crate) fn command_object_create(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 2 {
        return Err(
            "usage: ousject object-create <type-name> <initial-value> [options]".to_owned(),
        );
    }
    let manager = open_manager(options.state.as_deref())?;
    let descriptor = manager.type_by_name(&positional[0]).map_err(error_text)?;
    let value = parse_cli_value(descriptor.schema, &positional[1])?;
    let context = AccessContext::new(option_subject(&manager, &options)?);
    let object = manager
        .create_object(context, CreateSpec::new(&positional[0], value))
        .map_err(error_text)?;
    println!("{object}");
    Ok(())
}

pub(crate) fn command_object_value(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 1 {
        return Err("usage: ousject object-value <object-id> [options]".to_owned());
    }
    let manager = open_manager(options.state.as_deref())?;
    let context = AccessContext::new(option_subject(&manager, &options)?);
    let value = manager
        .value(context, parse_object(&positional[0])?)
        .map_err(error_text)?;
    println!("{value}");
    Ok(())
}

pub(crate) fn command_object_query(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 1 {
        return Err(
            "usage: ousject object-query <type-name> [--capability <name>] [options]".to_owned(),
        );
    }
    let manager = open_manager(options.state.as_deref())?;
    let descriptor = manager.type_by_name(&positional[0]).map_err(error_text)?;
    let mut query = ObjectQuery::new().with_type(descriptor.id);
    let subject = option_subject(&manager, &options)?;
    if let Some(capability) = options.capability {
        query = query.with_domain_capability(capability);
    }
    for header in manager
        .query(AccessContext::new(subject), &query)
        .map_err(error_text)?
    {
        println!(
            "{} type={} version={}",
            header.id,
            descriptor.name,
            header.version.get()
        );
    }
    Ok(())
}

pub(crate) fn command_object_bind(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 3 {
        return Err(
            "usage: ousject object-bind <namespace-id> <name> <target-id> [options]".to_owned(),
        );
    }
    let manager = open_manager(options.state.as_deref())?;
    let context = AccessContext::new(option_subject(&manager, &options)?);
    let version = manager
        .bind_name(
            context,
            parse_object(&positional[0])?,
            &positional[1],
            parse_object(&positional[2])?,
        )
        .map_err(error_text)?;
    println!("version={}", version.get());
    Ok(())
}

pub(crate) fn command_object_unbind(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 2 {
        return Err("usage: ousject object-unbind <namespace-id> <name> [options]".to_owned());
    }
    let manager = open_manager(options.state.as_deref())?;
    let context = AccessContext::new(option_subject(&manager, &options)?);
    let version = manager
        .unbind_name(context, parse_object(&positional[0])?, &positional[1])
        .map_err(error_text)?;
    println!("version={}", version.get());
    Ok(())
}

pub(crate) fn command_object_resolve(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 2 {
        return Err("usage: ousject object-resolve <root-id> <path> [options]".to_owned());
    }
    let manager = open_manager(options.state.as_deref())?;
    let context = AccessContext::new(option_subject(&manager, &options)?);
    let object = manager
        .resolve(context, parse_object(&positional[0])?, &positional[1])
        .map_err(error_text)?;
    println!("{object}");
    Ok(())
}

pub(crate) fn command_object_policy(arguments: &[String], grant: bool) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 3 {
        let action = if grant { "grant" } else { "revoke" };
        return Err(format!(
            "usage: ousject object-{action} <object-id> <subject-id> <capability> [options]"
        ));
    }
    let object = parse_object(&positional[0])?;
    let subject = positional[1]
        .parse::<SubjectId>()
        .map_err(|error| format!("invalid SubjectId: {error}"))?;
    let capability = parse_capability(&positional[2])?;
    let manager = open_manager(options.state.as_deref())?;
    let context = AccessContext::new(option_subject(&manager, &options)?);
    let version = manager
        .inspect(context, object)
        .map_err(error_text)?
        .version;
    let mut transaction = manager.begin(context);
    transaction.expect(object, version);
    if grant {
        transaction.grant(object, subject, capability);
    } else {
        transaction.revoke(object, subject, capability);
    }
    let result = manager.commit(transaction).map_err(error_text)?;
    println!("version={}", result.versions[&object].get());
    Ok(())
}

fn parse_capability(value: &str) -> Result<Capability, String> {
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
        _ => Err("unknown capability; use view_value, replace_value, create_child, invoke, link, reparent, retire, inspect or manage_policy".to_owned()),
    }
}

fn parse_cli_value(schema: ValueSchema, input: &str) -> Result<Value, String> {
    match schema {
        ValueSchema::Any | ValueSchema::Text => Ok(Value::Text(input.to_owned())),
        ValueSchema::Bytes => Ok(Value::Bytes(input.as_bytes().to_vec())),
        ValueSchema::Collection if input == "[]" => Ok(Value::Array(Vec::new())),
        ValueSchema::Collection if input == "{}" => {
            Ok(Value::Map(std::collections::BTreeMap::new()))
        }
        ValueSchema::Record if input == "{}" => {
            Ok(Value::Record(std::collections::BTreeMap::new()))
        }
        ValueSchema::Collection => {
            Err("collection CLI value must currently be [] or {}".to_owned())
        }
        ValueSchema::Record => Err("record CLI value must currently be {}".to_owned()),
    }
}

pub(crate) fn command_tf_dump(arguments: &[String]) -> Result<(), String> {
    if arguments.len() != 1 {
        return Err("usage: ousject tf-dump <program.tf>".to_owned());
    }
    let program =
        Program::decode(&std::fs::read(&arguments[0]).map_err(error_text)?).map_err(error_text)?;
    for (position, token) in program.tokens.iter().enumerate() {
        println!("{position:06} {token:?}");
    }
    Ok(())
}

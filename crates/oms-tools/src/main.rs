use oms_runtime::{AccessContext, CreateObject, InMemoryObjectManager};
use oms_types::{Capability, ObjectHeader, ObjectId, OmsError, SubjectId, TypeId};
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::sync::Arc;

const TEXT_TYPE: TypeId = TypeId::from_u128(1);

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None | Some("demo") => run_demo()?,
        Some("shell") => {
            let shard_count = arguments
                .next()
                .map_or(Ok(1), |value| value.parse::<u32>())?;
            run_shell(shard_count)?;
        }
        Some("help" | "--help" | "-h") => print_usage(),
        Some(command) => {
            eprintln!("unknown command: {command}");
            print_usage();
            std::process::exit(2);
        }
    }
    Ok(())
}

fn print_usage() {
    println!(
        "Ousject OMS M1\n\n\
         Usage:\n  oms-tools demo\n  oms-tools shell [shard-count]\n\n\
         The M1 runtime is in-memory only; state ends when the process exits."
    );
}

fn run_demo() -> Result<(), OmsError> {
    let manager = InMemoryObjectManager::new(1)?;
    let owner = SubjectId::new();
    let owner_context = AccessContext::new(owner);

    let process = create_object(&manager, owner_context, "Process", None)?;
    let counter = create_object(&manager, owner_context, "0", Some(process))?;

    let process_view = manager.read(owner_context, process)?;
    let mut link = manager.begin(owner_context);
    link.expect(process, process_view.header().version)
        .set_link(process, "displayed", counter);
    manager.commit(link)?;

    let old_counter = manager.read(owner_context, counter)?;
    let mut winner = manager.begin(owner_context);
    winner
        .expect(counter, old_counter.header().version)
        .update_state(counter, b"1");
    let mut loser = manager.begin(owner_context);
    loser
        .expect(counter, old_counter.header().version)
        .update_state(counter, b"2");
    manager.commit(winner)?;
    let conflict_detected = matches!(manager.commit(loser), Err(OmsError::Conflict { .. }));
    if !conflict_detected {
        return Err(OmsError::InvalidOperation(
            "demo expected an optimistic write conflict",
        ));
    }

    let reader = SubjectId::new();
    let current = manager.read(owner_context, counter)?;
    let mut grant = manager.begin(owner_context);
    grant
        .expect(counter, current.header().version)
        .grant(counter, reader, Capability::ViewValue);
    manager.commit(grant)?;
    let reader_view = manager.read(AccessContext::new(reader), counter)?;
    if reader_view.state() != b"1" {
        return Err(OmsError::InvalidOperation(
            "demo counter has an unexpected state",
        ));
    }
    manager.health_check()?;

    let stats = manager.stats()?;
    println!("OMS M1 demo completed");
    println!("owner:   {owner}");
    println!("process: {process}");
    println!("counter: {counter}");
    println!(
        "counter state: {}",
        String::from_utf8_lossy(reader_view.state())
    );
    println!("optimistic conflict detected: {conflict_detected}");
    println!(
        "objects: {} active, {} tombstoned across {} shard",
        stats.active_count, stats.tombstoned_count, stats.shard_count
    );
    println!("health check: ok");
    Ok(())
}

fn run_shell(shard_count: u32) -> Result<(), Box<dyn Error>> {
    let manager = Arc::new(InMemoryObjectManager::new(shard_count)?);
    let owner = SubjectId::new();
    let mut context = AccessContext::new(owner);
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut line = String::new();

    println!("Ousject OMS M1 shell");
    println!("initial subject: {owner}");
    println!("in-memory runtime with {shard_count} shard(s); type 'help' for commands");

    loop {
        print!("oms> ");
        io::stdout().flush()?;
        line.clear();
        if input.read_line(&mut line)? == 0 {
            break;
        }
        let command = line.trim();
        if command.is_empty() {
            continue;
        }
        match execute_command(&manager, &mut context, command) {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => eprintln!("error: {error}"),
        }
    }
    Ok(())
}

fn execute_command(
    manager: &InMemoryObjectManager,
    context: &mut AccessContext,
    command: &str,
) -> Result<bool, String> {
    let mut words = command.split_whitespace();
    let verb = words.next().unwrap_or_default();
    match verb {
        "help" => print_shell_help(),
        "quit" | "exit" => return Ok(false),
        "whoami" => println!("{}", context.subject),
        "new-subject" => println!("{}", SubjectId::new()),
        "as" => context.subject = parse_subject(required(&mut words, "subject")?)?,
        "create" => command_create(manager, *context, command)?,
        "create-child" => command_create_child(manager, *context, command)?,
        "read" => command_read(manager, *context, required_id(&mut words)?)?,
        "inspect" => command_inspect(manager, *context, required_id(&mut words)?)?,
        "update" => command_update(manager, *context, command)?,
        "link" => command_link(manager, *context, &mut words)?,
        "unlink" => command_unlink(manager, *context, &mut words)?,
        "reparent" => command_reparent(manager, *context, &mut words)?,
        "grant" => command_policy(manager, *context, &mut words, true)?,
        "revoke" => command_policy(manager, *context, &mut words, false)?,
        "tombstone" => command_tombstone(manager, *context, required_id(&mut words)?)?,
        "list" => command_list(manager, *context)?,
        "stats" => command_stats(manager)?,
        "health" => {
            manager.health_check().map_err(error_text)?;
            println!("ok");
        }
        _ => return Err(format!("unknown command '{verb}'; type 'help'")),
    }
    Ok(true)
}

fn command_create(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    command: &str,
) -> Result<(), String> {
    let state = command
        .strip_prefix("create")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "usage: create <state>".to_owned())?;
    let id = create_object(manager, context, state, None).map_err(error_text)?;
    println!("{id}");
    Ok(())
}

fn command_create_child(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    command: &str,
) -> Result<(), String> {
    let rest = command
        .strip_prefix("create-child")
        .map(str::trim)
        .ok_or_else(|| "usage: create-child <parent-id> <state>".to_owned())?;
    let (parent, state) = rest
        .split_once(char::is_whitespace)
        .ok_or_else(|| "usage: create-child <parent-id> <state>".to_owned())?;
    let parent = parse_object(parent)?;
    let state = state.trim();
    if state.is_empty() {
        return Err("usage: create-child <parent-id> <state>".to_owned());
    }
    let id = create_object(manager, context, state, Some(parent)).map_err(error_text)?;
    println!("{id}");
    Ok(())
}

fn create_object(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    state: &str,
    parent: Option<ObjectId>,
) -> Result<ObjectId, OmsError> {
    let mut request = CreateObject::new(TEXT_TYPE, state.as_bytes());
    if let Some(parent) = parent {
        while manager.shard_for(request.id) != manager.shard_for(parent) {
            request.id = ObjectId::new();
        }
        request = request.with_parent(parent);
    }
    let id = request.id;
    let mut transaction = manager.begin(context);
    if let Some(parent) = parent {
        let parent_version = manager.inspect(context, parent)?.version;
        transaction.expect(parent, parent_version);
    }
    transaction.create(request);
    manager.commit(transaction)?;
    Ok(id)
}

fn command_read(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    object: ObjectId,
) -> Result<(), String> {
    let view = manager.read(context, object).map_err(error_text)?;
    println!("{}", String::from_utf8_lossy(view.state()));
    Ok(())
}

fn command_inspect(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    object: ObjectId,
) -> Result<(), String> {
    let header = manager.inspect(context, object).map_err(error_text)?;
    print_header(&header);
    Ok(())
}

fn command_update(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    command: &str,
) -> Result<(), String> {
    let rest = command
        .strip_prefix("update")
        .map(str::trim)
        .ok_or_else(|| "usage: update <object-id> <state>".to_owned())?;
    let (object, state) = rest
        .split_once(char::is_whitespace)
        .ok_or_else(|| "usage: update <object-id> <state>".to_owned())?;
    let object = parse_object(object)?;
    let view = manager.read(context, object).map_err(error_text)?;
    let mut transaction = manager.begin(context);
    transaction
        .expect(object, view.header().version)
        .update_state(object, state.trim().as_bytes());
    manager.commit(transaction).map_err(error_text)?;
    println!("updated {object}");
    Ok(())
}

fn command_link(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    words: &mut dyn Iterator<Item = &str>,
) -> Result<(), String> {
    let source = parse_object(required(words, "source")?)?;
    let name = required(words, "name")?;
    let target = parse_object(required(words, "target")?)?;
    let version = manager
        .inspect(context, source)
        .map_err(error_text)?
        .version;
    let mut transaction = manager.begin(context);
    transaction
        .expect(source, version)
        .set_link(source, name, target);
    manager.commit(transaction).map_err(error_text)?;
    println!("linked {source}.{name} -> {target}");
    Ok(())
}

fn command_unlink(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    words: &mut dyn Iterator<Item = &str>,
) -> Result<(), String> {
    let source = parse_object(required(words, "source")?)?;
    let name = required(words, "name")?;
    let version = manager
        .inspect(context, source)
        .map_err(error_text)?
        .version;
    let mut transaction = manager.begin(context);
    transaction
        .expect(source, version)
        .remove_link(source, name);
    manager.commit(transaction).map_err(error_text)?;
    println!("removed {source}.{name}");
    Ok(())
}

fn command_reparent(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    words: &mut dyn Iterator<Item = &str>,
) -> Result<(), String> {
    let child = parse_object(required(words, "child")?)?;
    let parent_text = required(words, "parent or none")?;
    let new_parent = (parent_text != "none")
        .then(|| parse_object(parent_text))
        .transpose()?;
    let child_header = manager.inspect(context, child).map_err(error_text)?;
    let mut transaction = manager.begin(context);
    transaction.expect(child, child_header.version);
    if let Some(old_parent) = child_header.parent_id {
        let version = manager
            .inspect(context, old_parent)
            .map_err(error_text)?
            .version;
        transaction.expect(old_parent, version);
    }
    if let Some(parent) = new_parent {
        let version = manager
            .inspect(context, parent)
            .map_err(error_text)?
            .version;
        transaction.expect(parent, version);
    }
    transaction.reparent(child, new_parent);
    manager.commit(transaction).map_err(error_text)?;
    println!("reparented {child}");
    Ok(())
}

fn command_policy(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    words: &mut dyn Iterator<Item = &str>,
    grant: bool,
) -> Result<(), String> {
    let object = parse_object(required(words, "object")?)?;
    let subject = parse_subject(required(words, "subject")?)?;
    let capability = parse_capability(required(words, "capability")?)?;
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
    manager.commit(transaction).map_err(error_text)?;
    println!("policy updated for {object}");
    Ok(())
}

fn command_tombstone(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    object: ObjectId,
) -> Result<(), String> {
    let header = manager.inspect(context, object).map_err(error_text)?;
    let mut transaction = manager.begin(context);
    transaction.expect(object, header.version);
    if let Some(parent) = header.parent_id {
        let version = manager
            .inspect(context, parent)
            .map_err(error_text)?
            .version;
        transaction.expect(parent, version);
    }
    transaction.tombstone(object);
    manager.commit(transaction).map_err(error_text)?;
    println!("tombstoned {object}");
    Ok(())
}

fn command_list(manager: &InMemoryObjectManager, context: AccessContext) -> Result<(), String> {
    for header in manager.list(context).map_err(error_text)? {
        print_header(&header);
    }
    Ok(())
}

fn command_stats(manager: &InMemoryObjectManager) -> Result<(), String> {
    let stats = manager.stats().map_err(error_text)?;
    println!(
        "shards={} objects={} active={} tombstoned={}",
        stats.shard_count, stats.object_count, stats.active_count, stats.tombstoned_count
    );
    Ok(())
}

fn print_header(header: &ObjectHeader) {
    let parent = header
        .parent_id
        .map_or_else(|| "none".to_owned(), |id| id.to_string());
    println!(
        "id={} type={} version={} lifecycle={:?} parent={parent}",
        header.id,
        header.type_id,
        header.version.get(),
        header.lifecycle
    );
}

fn required<'a>(words: &mut dyn Iterator<Item = &'a str>, name: &str) -> Result<&'a str, String> {
    words.next().ok_or_else(|| format!("missing {name}"))
}

fn required_id(words: &mut dyn Iterator<Item = &str>) -> Result<ObjectId, String> {
    parse_object(required(words, "object id")?)
}

fn parse_object(value: &str) -> Result<ObjectId, String> {
    value
        .parse()
        .map_err(|error| format!("invalid ObjectId: {error}"))
}

fn parse_subject(value: &str) -> Result<SubjectId, String> {
    value
        .parse()
        .map_err(|error| format!("invalid SubjectId: {error}"))
}

fn parse_capability(value: &str) -> Result<Capability, String> {
    match value.to_ascii_lowercase().as_str() {
        "view-value" => Ok(Capability::ViewValue),
        "replace-value" => Ok(Capability::ReplaceValue),
        "create-child" => Ok(Capability::CreateChild),
        "invoke" => Ok(Capability::Invoke),
        "link" => Ok(Capability::Link),
        "reparent" => Ok(Capability::Reparent),
        "retire" => Ok(Capability::Retire),
        "inspect" => Ok(Capability::Inspect),
        "manage-policy" => Ok(Capability::ManagePolicy),
        _ => Err(format!("unknown capability '{value}'")),
    }
}

// `Result::map_err` supplies the owned error to this adapter.
#[allow(clippy::needless_pass_by_value)]
fn error_text(error: OmsError) -> String {
    error.to_string()
}

fn print_shell_help() {
    println!(
        "help\n\
         create <state>\n\
         create-child <parent-id> <state>\n\
         read <object-id>\n\
         inspect <object-id>\n\
         update <object-id> <state>\n\
         link <source-id> <name> <target-id>\n\
         unlink <source-id> <name>\n\
         reparent <child-id> <parent-id|none>\n\
         tombstone <object-id>\n\
         list | stats | health\n\
         new-subject | whoami | as <subject-id>\n\
         grant <object-id> <subject-id> <capability>\n\
         revoke <object-id> <subject-id> <capability>\n\
         quit"
    );
}

use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use oms_runtime::{
    AccessContext, CreateObject, CreateSpec, CreationPolicy, InMemoryObjectManager, ObjectQuery,
    ValueSchema,
};
use oms_types::{
    CORE_CONSOLE_TYPE, CORE_NAMESPACE_TYPE, CORE_PROGRAM_TYPE, CORE_SYSTEM_TYPE, Capability,
    DEVICE_BLOCK_STORAGE_TYPE, DEVICE_DISPLAY_TYPE, DEVICE_KEYBOARD_TYPE, DEVICE_SENSOR_TYPE,
    ObjectId, SubjectId, Value,
};
use ousject_auth::AuthService;
use ousject_provider::{ObjectProvider, ProviderError, ProviderOutcome};
use ousject_vm::{
    ConsoleProvider, CooperativeScheduler, ProcessStatus, RunReport, SYSTEM_SUBJECT, VirtualMachine,
};
use praxis_compiler::compile_with_loader;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::OpenOptions;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tf_format::Program;

const DEFAULT_STEP_LIMIT: u64 = 1_000_000;

fn main() {
    if let Err(error) = run_cli() {
        eprintln!("ousject: {error}");
        std::process::exit(1);
    }
}

fn run_cli() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "help".to_owned());
    let rest = arguments.collect::<Vec<_>>();
    match command.as_str() {
        "compile" => command_compile(&rest),
        "system-install" => command_system_install(&rest),
        "boot" => command_boot(&rest),
        "run" => command_run(&rest, false),
        "run-tf" => command_run(&rest, true),
        "resume" => command_resume(&rest),
        "schedule" => command_schedule(&rest),
        "inspect" => command_inspect(&rest),
        "list" => command_list(&rest),
        "check" => command_check(&rest),
        "types" => command_types(&rest),
        "type-register" => command_type_register(&rest),
        "object-create" => command_object_create(&rest),
        "object-value" => command_object_value(&rest),
        "object-query" => command_object_query(&rest),
        "object-bind" => command_object_bind(&rest),
        "object-unbind" => command_object_unbind(&rest),
        "object-resolve" => command_object_resolve(&rest),
        "object-grant" => command_object_policy(&rest, true),
        "object-revoke" => command_object_policy(&rest, false),
        "tf-dump" => command_tf_dump(&rest),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => Err(format!("unknown command '{command}'; use 'ousject help'")),
    }
}

fn command_compile(arguments: &[String]) -> Result<(), String> {
    if arguments.len() != 2 {
        return Err("usage: ousject compile <source.px> <output.tf>".to_owned());
    }
    let program = compile_source_file(Path::new(&arguments[0]))?;
    let bytes = program.encode().map_err(error_text)?;
    std::fs::write(&arguments[1], bytes).map_err(error_text)?;
    println!(
        "compiled {} tokens to {}",
        program.tokens.len(),
        arguments[1]
    );
    Ok(())
}

fn command_system_install(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 1 || !options.local || options.session.is_some() {
        return Err(
            "usage: ousject system-install <system-directory> --local [--state <path>]".to_owned(),
        );
    }
    let manager = open_manager(options.state.as_deref())?;
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let installed = manager
        .query(context, &ObjectQuery::new().with_type(CORE_NAMESPACE_TYPE))
        .map_err(error_text)?
        .into_iter()
        .find(|header| {
            matches!(
                manager.value(context, header.id),
                Ok(Value::Record(fields))
                    if fields.get("name") == Some(&Value::Text("system".to_owned()))
            )
        });
    let directory = Path::new(&positional[0]);
    let mut entries = std::fs::read_dir(directory)
        .map_err(error_text)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error_text)?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let namespace = installed
        .as_ref()
        .map_or_else(ObjectId::new, |item| item.id);
    let mut namespace_request = CreateObject::new(
        CORE_NAMESPACE_TYPE,
        Value::Record(BTreeMap::from([(
            "name".to_owned(),
            Value::Text("system".to_owned()),
        )]))
        .encode()
        .map_err(error_text)?,
    )
    .with_id(namespace);
    let mut programs = Vec::new();
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("px") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| "system program name is not UTF-8".to_owned())?
            .to_owned();
        let program = compile_source_file(&path)?;
        let request = CreateObject::new(CORE_PROGRAM_TYPE, program.encode().map_err(error_text)?);
        namespace_request.links.insert(name, request.id);
        programs.push(request);
    }
    if !namespace_request.links.contains_key("init") {
        return Err("system directory has no init.px".to_owned());
    }
    let mut transaction = manager.begin(context);
    for program in programs {
        transaction.create(program);
    }
    if let Some(installed) = installed {
        let current = manager.read(context, installed.id).map_err(error_text)?;
        transaction.expect(installed.id, installed.version);
        for name in current.links().keys() {
            transaction.remove_link(installed.id, name.clone());
        }
        for (name, program) in namespace_request.links {
            transaction.set_link(installed.id, name, program);
        }
    } else {
        transaction.create(namespace_request);
    }
    manager.commit(transaction).map_err(error_text)?;
    println!("installed system programs in namespace={namespace}");
    Ok(())
}

fn command_boot(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if !positional.is_empty() || options.session.is_some() || options.local {
        return Err("usage: ousject boot [--state <path>]".to_owned());
    }
    let block_path = host_block_path(&options);
    let manager = open_manager(options.state.as_deref())?;
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let namespace = manager
        .query(context, &ObjectQuery::new().with_type(CORE_NAMESPACE_TYPE))
        .map_err(error_text)?
        .into_iter()
        .find(|header| {
            matches!(
                manager.value(context, header.id),
                Ok(Value::Record(fields))
                    if fields.get("name") == Some(&Value::Text("system".to_owned()))
            )
        })
        .ok_or_else(|| "system programs are not installed".to_owned())?;
    let init = manager
        .resolve(context, namespace.id, "init")
        .map_err(error_text)?;
    let program = Program::decode(manager.read(context, init).map_err(error_text)?.state())
        .map_err(error_text)?;
    let vm = discover_host_hardware(manager, block_path)?;
    loop {
        let process = vm.create_process(&program).map_err(error_text)?;
        let report = run_hosted_process(&vm, process, options.steps)?;
        print_report(&report);
        if report.status != ProcessStatus::Halted {
            return Err(format!("init stopped with status {:?}", report.status));
        }
        let system = vm
            .manager()
            .query(context, &ObjectQuery::new().with_type(CORE_SYSTEM_TYPE))
            .map_err(error_text)?
            .into_iter()
            .next()
            .ok_or_else(|| "core.system Object is missing".to_owned())?;
        let request = match vm.manager().value(context, system.id).map_err(error_text)? {
            Value::Record(fields) => fields.get("request").cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
        if request != Value::Text("restart".to_owned()) {
            return Ok(());
        }
        let mut transaction = vm.manager().begin(context);
        transaction.expect(system.id, system.version).update_state(
            system.id,
            Value::Record(BTreeMap::from([
                ("name".to_owned(), Value::Text("system".to_owned())),
                ("request".to_owned(), Value::Null),
            ]))
            .encode()
            .map_err(error_text)?,
        );
        vm.manager().commit(transaction).map_err(error_text)?;
    }
}

fn command_run(arguments: &[String], tf_input: bool) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 1 {
        let input = if tf_input { "program.tf" } else { "source.px" };
        return Err(format!(
            "usage: ousject {} <{input}> [options]",
            if tf_input { "run-tf" } else { "run" }
        ));
    }
    let program = if tf_input {
        Program::decode(&std::fs::read(&positional[0]).map_err(error_text)?).map_err(error_text)?
    } else {
        compile_source_file(Path::new(&positional[0]))?
    };
    let block_path = host_block_path(&options);
    let manager = open_manager(options.state.as_deref())?;
    let subject = option_subject(&manager, &options)?;
    let vm = discover_host_hardware(manager, block_path)?;
    grant_console_access(vm.manager(), subject)?;
    let process = vm
        .create_process_as(&program, subject)
        .map_err(error_text)?;
    let report = run_hosted_process(&vm, process, options.steps)?;
    print_report(&report);
    eprintln!(
        "process={process} status={:?} steps={}",
        report.status, report.steps
    );
    Ok(())
}

fn compile_source_file(path: &Path) -> Result<Program, String> {
    let source = std::fs::read_to_string(path).map_err(error_text)?;
    let root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    compile_with_loader(&source, |specifier| {
        let mut candidate = root.join(specifier);
        if candidate.extension().is_none() {
            candidate.set_extension("px");
        }
        std::fs::read_to_string(&candidate).map_err(|error| error.to_string())
    })
    .map_err(error_text)
}

fn command_resume(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 1 {
        return Err("usage: ousject resume <process-id> [options]".to_owned());
    }
    let process = parse_object(&positional[0])?;
    let block_path = host_block_path(&options);
    let manager = open_manager(options.state.as_deref())?;
    let vm = discover_host_hardware(manager, block_path)?;
    if options.session.is_some() {
        let subject = option_subject(vm.manager(), &options)?;
        if vm.process_state(process).map_err(error_text)?.subject != subject {
            return Err("the Session does not own this Process".to_owned());
        }
    }
    grant_console_access(
        vm.manager(),
        vm.process_state(process).map_err(error_text)?.subject,
    )?;
    vm.reconnect_hardware(process).map_err(error_text)?;
    let report = run_hosted_process(&vm, process, options.steps)?;
    print_report(&report);
    eprintln!(
        "process={process} status={:?} steps={}",
        report.status, report.steps
    );
    Ok(())
}

fn command_schedule(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    let block_path = host_block_path(&options);
    let manager = open_manager(options.state.as_deref())?;
    let subject = option_subject(&manager, &options)?;
    let vm = discover_host_hardware(manager, block_path)?;
    grant_console_access(vm.manager(), subject)?;
    let processes = if positional.is_empty() {
        vm.manager()
            .query(
                AccessContext::new(subject),
                &ObjectQuery::new().with_type(oms_types::CORE_PROCESS_TYPE),
            )
            .map_err(error_text)?
            .into_iter()
            .map(|header| header.id)
            .collect::<Vec<_>>()
    } else {
        positional
            .iter()
            .map(|value| parse_object(value))
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut scheduler = CooperativeScheduler::new(&vm);
    for process in processes {
        let state = vm.process_state(process).map_err(error_text)?;
        if subject != SYSTEM_SUBJECT && state.subject != subject {
            return Err(format!("Session does not own Process {process}"));
        }
        vm.reconnect_hardware(process).map_err(error_text)?;
        scheduler.enqueue(process);
    }
    let report = scheduler.run(options.steps).map_err(error_text)?;
    println!("steps={}", report.total_steps);
    for process in report.processes {
        println!(
            "process={} status={:?} steps={}",
            process.process, process.status, process.steps
        );
    }
    Ok(())
}

fn command_inspect(arguments: &[String]) -> Result<(), String> {
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

fn command_list(arguments: &[String]) -> Result<(), String> {
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

fn command_check(arguments: &[String]) -> Result<(), String> {
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

fn command_types(arguments: &[String]) -> Result<(), String> {
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

fn command_type_register(arguments: &[String]) -> Result<(), String> {
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

fn command_object_create(arguments: &[String]) -> Result<(), String> {
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

fn command_object_value(arguments: &[String]) -> Result<(), String> {
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

fn command_object_query(arguments: &[String]) -> Result<(), String> {
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

fn command_object_bind(arguments: &[String]) -> Result<(), String> {
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

fn command_object_unbind(arguments: &[String]) -> Result<(), String> {
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

fn command_object_resolve(arguments: &[String]) -> Result<(), String> {
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

fn command_object_policy(arguments: &[String], grant: bool) -> Result<(), String> {
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

fn command_tf_dump(arguments: &[String]) -> Result<(), String> {
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

#[derive(Debug)]
struct RuntimeOptions {
    state: Option<PathBuf>,
    steps: u64,
    capability: Option<String>,
    session: Option<String>,
    local: bool,
}

fn parse_options(arguments: &[String]) -> Result<(Vec<String>, RuntimeOptions), String> {
    let mut positional = Vec::new();
    let mut state = Some(PathBuf::from(".ousject/objects.oms"));
    let mut steps = DEFAULT_STEP_LIMIT;
    let mut capability = None;
    let mut session = None;
    let mut local = false;
    let mut position = 0;
    while position < arguments.len() {
        match arguments[position].as_str() {
            "--state" => {
                position += 1;
                let value = arguments
                    .get(position)
                    .ok_or_else(|| "--state requires a path".to_owned())?;
                state = Some(PathBuf::from(value));
            }
            "--memory" => state = None,
            "--steps" => {
                position += 1;
                let value = arguments
                    .get(position)
                    .ok_or_else(|| "--steps requires a number".to_owned())?;
                steps = value
                    .parse::<u64>()
                    .map_err(|_| "--steps must be an unsigned integer".to_owned())?;
            }
            "--capability" => {
                position += 1;
                capability = Some(
                    arguments
                        .get(position)
                        .ok_or_else(|| "--capability requires a name".to_owned())?
                        .to_owned(),
                );
            }
            "--session" => {
                position += 1;
                session = Some(
                    arguments
                        .get(position)
                        .ok_or_else(|| "--session requires a token".to_owned())?
                        .to_owned(),
                );
            }
            "--local" => local = true,
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            value => positional.push(value.to_owned()),
        }
        position += 1;
    }
    Ok((
        positional,
        RuntimeOptions {
            state,
            steps,
            capability,
            session,
            local,
        },
    ))
}

fn option_subject(
    manager: &Arc<InMemoryObjectManager>,
    options: &RuntimeOptions,
) -> Result<SubjectId, String> {
    match (&options.session, options.local) {
        (Some(_), true) => Err("use either --session or --local, not both".to_owned()),
        (Some(token), false) => AuthService::new(Arc::clone(manager))
            .authenticate(token)
            .map_err(error_text),
        (None, true) => Ok(SYSTEM_SUBJECT),
        (None, false) => Err(
            "authentication required: use --session <token>; --local is only for explicit development/recovery work"
                .to_owned(),
        ),
    }
}

fn open_manager(path: Option<&Path>) -> Result<Arc<InMemoryObjectManager>, String> {
    let manager = match path {
        Some(path) => InMemoryObjectManager::open_persistent(path),
        None => InMemoryObjectManager::new(1),
    }
    .map_err(error_text)?;
    Ok(Arc::new(manager))
}

fn host_block_path(options: &RuntimeOptions) -> PathBuf {
    options.state.as_ref().map_or_else(
        || {
            std::env::temp_dir().join(format!(
                "ousject-memory-{}-device-blocks.bin",
                std::process::id()
            ))
        },
        |path| path.with_extension("blocks"),
    )
}

/// Linux is currently only the hardware adapter. The runtime receives a
/// discovered provider Object and does not discover or fabricate devices.
fn discover_host_hardware(
    manager: Arc<InMemoryObjectManager>,
    block_path: PathBuf,
) -> Result<VirtualMachine, String> {
    let state = Value::Record(BTreeMap::from([
        (
            "provider".to_owned(),
            Value::Text("linux.stdout".to_owned()),
        ),
        (
            "interactive".to_owned(),
            Value::Bool(std::io::stdout().is_terminal()),
        ),
    ]));
    let console = VirtualMachine::publish_console(&manager, &state).map_err(error_text)?;
    if std::io::stdout().is_terminal() {
        VirtualMachine::publish_provider_object(
            &manager,
            DEVICE_DISPLAY_TYPE,
            &provider_state("linux.terminal.display"),
        )
        .map_err(error_text)?;
    }
    if std::io::stdin().is_terminal() {
        VirtualMachine::publish_provider_object(
            &manager,
            DEVICE_KEYBOARD_TYPE,
            &provider_state("linux.terminal.keyboard"),
        )
        .map_err(error_text)?;
    }
    VirtualMachine::publish_provider_object(
        &manager,
        DEVICE_SENSOR_TYPE,
        &Value::Record(BTreeMap::from([
            ("provider".to_owned(), Value::Text("linux.clock".to_owned())),
            ("kind".to_owned(), Value::Text("clock".to_owned())),
        ])),
    )
    .map_err(error_text)?;
    VirtualMachine::publish_provider_object(
        &manager,
        DEVICE_BLOCK_STORAGE_TYPE,
        &Value::Record(BTreeMap::from([
            (
                "provider".to_owned(),
                Value::Text("linux.block_file_adapter".to_owned()),
            ),
            ("block_size".to_owned(), Value::Integer(4096)),
        ])),
    )
    .map_err(error_text)?;
    let vm = VirtualMachine::with_console(manager, console, Arc::new(LinuxConsole::new()))
        .map_err(error_text)?;
    vm.register_provider(Arc::new(HostNetworkProvider::default()))
        .map_err(error_text)?;
    vm.register_provider(Arc::new(CachedProvider::new(HostDisplayProvider)))
        .map_err(error_text)?;
    vm.register_provider(Arc::new(CachedProvider::new(HostKeyboardProvider)))
        .map_err(error_text)?;
    vm.register_provider(Arc::new(CachedProvider::new(HostClockSensorProvider)))
        .map_err(error_text)?;
    vm.register_provider(Arc::new(CachedProvider::new(
        HostBlockStorageProvider::open(block_path).map_err(error_text)?,
    )))
    .map_err(error_text)?;
    Ok(vm)
}

fn provider_state(provider: &str) -> Value {
    Value::Record(BTreeMap::from([(
        "provider".to_owned(),
        Value::Text(provider.to_owned()),
    )]))
}

fn grant_console_access(
    manager: &Arc<InMemoryObjectManager>,
    subject: SubjectId,
) -> Result<(), String> {
    if subject == SYSTEM_SUBJECT {
        return Ok(());
    }
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let mut transaction = manager.begin(context);
    for type_id in [
        CORE_CONSOLE_TYPE,
        DEVICE_DISPLAY_TYPE,
        DEVICE_KEYBOARD_TYPE,
        DEVICE_SENSOR_TYPE,
        DEVICE_BLOCK_STORAGE_TYPE,
    ] {
        for object in manager
            .query(context, &ObjectQuery::new().with_type(type_id))
            .map_err(error_text)?
        {
            transaction
                .expect(object.id, object.version)
                .grant(object.id, subject, Capability::Inspect)
                .grant(object.id, subject, Capability::Invoke)
                .grant(object.id, subject, Capability::ViewValue)
                .grant(object.id, subject, Capability::ReplaceValue);
        }
    }
    manager.commit(transaction).map_err(error_text)?;
    Ok(())
}

#[derive(Debug)]
struct LinuxConsole {
    input: Mutex<LinuxInputState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Line,
    Secret,
}

#[derive(Debug)]
struct LinuxInputState {
    requests: mpsc::Sender<InputMode>,
    responses: mpsc::Receiver<(InputMode, Result<String, String>)>,
    outstanding: Option<InputMode>,
    lines: VecDeque<Result<String, String>>,
    secrets: VecDeque<Result<String, String>>,
}

impl LinuxConsole {
    fn new() -> Self {
        let (request_sender, request_receiver) = mpsc::channel();
        let (response_sender, response_receiver) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(mode) = request_receiver.recv() {
                if response_sender.send((mode, read_host_line(mode))).is_err() {
                    break;
                }
            }
        });
        Self {
            input: Mutex::new(LinuxInputState {
                requests: request_sender,
                responses: response_receiver,
                outstanding: None,
                lines: VecDeque::new(),
                secrets: VecDeque::new(),
            }),
        }
    }

    fn poll_input(&self, mode: InputMode) -> Result<Option<String>, String> {
        let mut input = self.input.lock().map_err(error_text)?;
        while let Ok((completed_mode, result)) = input.responses.try_recv() {
            input.outstanding = None;
            match completed_mode {
                InputMode::Line => input.lines.push_back(result),
                InputMode::Secret => input.secrets.push_back(result),
            }
        }
        let ready = match mode {
            InputMode::Line => input.lines.pop_front(),
            InputMode::Secret => input.secrets.pop_front(),
        };
        if let Some(result) = ready {
            return result.map(Some);
        }
        if input.outstanding.is_none() {
            input.requests.send(mode).map_err(error_text)?;
            input.outstanding = Some(mode);
        }
        Ok(None)
    }
}

impl ConsoleProvider for LinuxConsole {
    fn println(&self, text: &str) -> Result<(), String> {
        writeln!(std::io::stdout().lock(), "{text}").map_err(error_text)
    }

    fn try_read_line(&self) -> Result<Option<String>, String> {
        self.poll_input(InputMode::Line)
    }

    fn try_read_secret(&self) -> Result<Option<String>, String> {
        self.poll_input(InputMode::Secret)
    }
}

fn read_host_line(mode: InputMode) -> Result<String, String> {
    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    let original = if mode == InputMode::Secret && terminal {
        let original = tcgetattr(&stdin).map_err(error_text)?;
        let mut hidden = original.clone();
        hidden.local_flags.remove(LocalFlags::ECHO);
        tcsetattr(&stdin, SetArg::TCSANOW, &hidden).map_err(error_text)?;
        Some(original)
    } else {
        None
    };
    let mut line = String::new();
    let read_result = stdin.read_line(&mut line).map_err(error_text);
    let restore_result = original
        .as_ref()
        .map(|settings| tcsetattr(&stdin, SetArg::TCSANOW, settings).map_err(error_text))
        .transpose();
    if original.is_some() {
        writeln!(std::io::stdout().lock()).map_err(error_text)?;
    }
    restore_result?;
    if read_result? == 0 {
        Err("console input reached EOF".to_owned())
    } else {
        while matches!(line.as_bytes().last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        Ok(line)
    }
}

#[derive(Debug)]
struct CachedProvider<P> {
    inner: P,
    completed: Mutex<BTreeMap<ObjectId, ProviderOutcome>>,
}

impl<P> CachedProvider<P> {
    fn new(inner: P) -> Self {
        Self {
            inner,
            completed: Mutex::new(BTreeMap::new()),
        }
    }
}

impl<P: ObjectProvider> ObjectProvider for CachedProvider<P> {
    fn type_id(&self) -> oms_types::TypeId {
        self.inner.type_id()
    }

    fn user_creatable(&self) -> bool {
        self.inner.user_creatable()
    }

    fn create(&self, initial: &Value) -> Result<Value, ProviderError> {
        self.inner.create(initial)
    }

    fn invoke(
        &self,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if let Some(outcome) = self
            .completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&effect)
            .cloned()
        {
            return Ok(outcome);
        }
        let outcome = self
            .inner
            .invoke(object, state, capability, arguments, effect)?;
        self.completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .insert(effect, outcome.clone());
        Ok(outcome)
    }

    fn capabilities(&self) -> BTreeSet<String> {
        self.inner.capabilities()
    }
}

#[derive(Debug)]
struct HostDisplayProvider;

impl ObjectProvider for HostDisplayProvider {
    fn type_id(&self) -> oms_types::TypeId {
        DEVICE_DISPLAY_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Display Objects are published by hardware discovery",
        ))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        let outcome = match (capability, arguments) {
            ("present", [Value::Text(frame)]) => {
                write!(std::io::stdout().lock(), "{frame}").map_err(adapter_error)?;
                std::io::stdout().lock().flush().map_err(adapter_error)?;
                ProviderOutcome::result(Value::Null)
            }
            ("present", [Value::Bytes(frame)]) => {
                std::io::stdout()
                    .lock()
                    .write_all(frame)
                    .and_then(|()| std::io::stdout().lock().flush())
                    .map_err(adapter_error)?;
                ProviderOutcome::result(Value::Null)
            }
            ("configure", [configuration]) => {
                return Ok(
                    ProviderOutcome::result(Value::Null).with_state(Value::Record(BTreeMap::from(
                        [
                            (
                                "provider".to_owned(),
                                Value::Text("linux.terminal.display".to_owned()),
                            ),
                            ("configuration".to_owned(), configuration.clone()),
                        ],
                    ))),
                );
            }
            _ => return Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        };
        Ok(outcome.with_state(state.clone()))
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["present", "configure"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

#[derive(Debug)]
struct HostKeyboardProvider;

impl ObjectProvider for HostKeyboardProvider {
    fn type_id(&self) -> oms_types::TypeId {
        DEVICE_KEYBOARD_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Keyboard Objects are published by hardware discovery",
        ))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if capability != "next_event" || !arguments.is_empty() {
            return Err(ProviderError::UnsupportedCapability(capability.to_owned()));
        }
        let mut event = String::new();
        std::io::stdin()
            .read_line(&mut event)
            .map_err(adapter_error)?;
        while matches!(event.as_bytes().last(), Some(b'\n' | b'\r')) {
            event.pop();
        }
        Ok(ProviderOutcome::result(Value::Text(event)))
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["next_event".to_owned()].into_iter().collect()
    }
}

#[derive(Debug)]
struct HostClockSensorProvider;

impl ObjectProvider for HostClockSensorProvider {
    fn type_id(&self) -> oms_types::TypeId {
        DEVICE_SENSOR_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Sensor Objects are published by hardware discovery",
        ))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        match (capability, arguments) {
            ("sample", []) => {
                let millis = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|error| ProviderError::Adapter(error.to_string()))?
                    .as_millis();
                Ok(ProviderOutcome::result(Value::Integer(
                    i64::try_from(millis).map_err(|_| {
                        ProviderError::Adapter("clock value is too large".to_owned())
                    })?,
                )))
            }
            ("calibrate", []) => Ok(ProviderOutcome::result(Value::Null)),
            _ => Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        }
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["sample", "calibrate"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

#[derive(Debug)]
struct HostBlockStorageProvider {
    file: Mutex<std::fs::File>,
}

impl HostBlockStorageProvider {
    fn open(path: PathBuf) -> Result<Self, std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl ObjectProvider for HostBlockStorageProvider {
    fn type_id(&self) -> oms_types::TypeId {
        DEVICE_BLOCK_STORAGE_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Block Storage Objects are published by hardware discovery",
        ))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        const BLOCK_SIZE: usize = 4096;
        match (capability, arguments) {
            ("load_block", [Value::Integer(index)]) => {
                let offset = block_offset(*index, BLOCK_SIZE)?;
                let mut file = self.file.lock().map_err(|_| ProviderError::Unavailable)?;
                file.seek(SeekFrom::Start(offset)).map_err(adapter_error)?;
                let mut block = vec![0_u8; BLOCK_SIZE];
                let mut count = 0;
                while count < block.len() {
                    match file.read(&mut block[count..]).map_err(adapter_error)? {
                        0 => break,
                        read => count += read,
                    }
                }
                Ok(ProviderOutcome::result(Value::Bytes(block)))
            }
            ("store_block", [Value::Integer(index), Value::Bytes(bytes)]) => {
                if bytes.len() != BLOCK_SIZE {
                    return Err(ProviderError::InvalidArguments(
                        "store_block requires exactly 4096 Bytes",
                    ));
                }
                let offset = block_offset(*index, BLOCK_SIZE)?;
                let mut file = self.file.lock().map_err(|_| ProviderError::Unavailable)?;
                file.seek(SeekFrom::Start(offset)).map_err(adapter_error)?;
                file.write_all(bytes).map_err(adapter_error)?;
                file.sync_all().map_err(adapter_error)?;
                Ok(ProviderOutcome::result(Value::Integer(
                    i64::try_from(bytes.len()).expect("block size fits i64"),
                )))
            }
            _ => Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        }
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["load_block", "store_block"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

fn block_offset(index: i64, block_size: usize) -> Result<u64, ProviderError> {
    let index = u64::try_from(index)
        .map_err(|_| ProviderError::InvalidArguments("block index must be non-negative"))?;
    index
        .checked_mul(u64::try_from(block_size).expect("block size fits u64"))
        .ok_or_else(|| ProviderError::Adapter("block offset overflow".to_owned()))
}

#[derive(Debug)]
enum HostEndpoint {
    Stream(TcpStream),
    Listener(TcpListener),
}

#[derive(Debug, Default)]
struct HostNetworkProvider {
    endpoints: Mutex<BTreeMap<ObjectId, HostEndpoint>>,
    completed: Mutex<BTreeMap<ObjectId, ProviderOutcome>>,
}

impl ObjectProvider for HostNetworkProvider {
    fn type_id(&self) -> oms_types::TypeId {
        oms_types::NET_ENDPOINT_TYPE
    }

    fn user_creatable(&self) -> bool {
        true
    }

    fn create(&self, initial: &Value) -> Result<Value, ProviderError> {
        let (Value::Map(fields) | Value::Record(fields)) = initial else {
            return Err(ProviderError::InvalidArguments(
                "Network Endpoint requires a configuration Map",
            ));
        };
        if !match fields.get("transport") {
            None => true,
            Some(Value::Text(value)) => value == "tcp",
            Some(_) => false,
        } {
            return Err(ProviderError::InvalidArguments(
                "only the tcp transport is currently available",
            ));
        }
        Ok(network_state("tcp", "new", None))
    }

    #[allow(clippy::too_many_lines)]
    fn invoke(
        &self,
        object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if let Some(outcome) = self
            .completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&effect)
            .cloned()
        {
            return Ok(outcome);
        }
        let outcome = match (capability, arguments) {
            ("connect", [Value::Text(host), Value::Integer(port)]) => {
                let address = socket_address(host, *port)?;
                let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
                    .map_err(adapter_error)?;
                configure_stream(&stream)?;
                self.endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .insert(object, HostEndpoint::Stream(stream));
                ProviderOutcome::result(Value::Null).with_state(network_state(
                    "tcp",
                    "connected",
                    Some(address.to_string()),
                ))
            }
            ("listen", [Value::Text(host), Value::Integer(port)]) => {
                let address = socket_address(host, *port)?;
                let listener = TcpListener::bind(address).map_err(adapter_error)?;
                listener.set_nonblocking(true).map_err(adapter_error)?;
                let local = listener.local_addr().map_err(adapter_error)?.to_string();
                self.endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .insert(object, HostEndpoint::Listener(listener));
                ProviderOutcome::result(Value::Text(local.clone())).with_state(network_state(
                    "tcp",
                    "listening",
                    Some(local),
                ))
            }
            ("accept", []) => {
                let (stream, peer) = {
                    let endpoints = self
                        .endpoints
                        .lock()
                        .map_err(|_| ProviderError::Unavailable)?;
                    let Some(HostEndpoint::Listener(listener)) = endpoints.get(&object) else {
                        return Err(ProviderError::Adapter(
                            "Endpoint is not listening".to_owned(),
                        ));
                    };
                    listener.accept().map_err(adapter_error)?
                };
                configure_stream(&stream)?;
                let child = ObjectId::new();
                self.endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .insert(child, HostEndpoint::Stream(stream));
                let child_state = network_state("tcp", "connected", Some(peer.to_string()));
                let request = oms_runtime::CreateObject::new(
                    oms_types::NET_ENDPOINT_TYPE,
                    child_state.encode()?,
                )
                .with_id(child)
                .with_parent(object);
                ProviderOutcome::result(Value::Text(child.to_string())).with_created(request)
            }
            ("send", [data]) => {
                let bytes = match data {
                    Value::Bytes(bytes) => bytes.as_slice(),
                    Value::Text(text) => text.as_bytes(),
                    _ => {
                        return Err(ProviderError::InvalidArguments(
                            "send requires Text or Bytes",
                        ));
                    }
                };
                let mut endpoints = self
                    .endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?;
                let Some(HostEndpoint::Stream(stream)) = endpoints.get_mut(&object) else {
                    return Err(ProviderError::Adapter(
                        "Endpoint is not connected".to_owned(),
                    ));
                };
                stream.write_all(bytes).map_err(adapter_error)?;
                ProviderOutcome::result(Value::Integer(i64::try_from(bytes.len()).map_err(
                    |_| ProviderError::Adapter("sent byte count is too large".to_owned()),
                )?))
            }
            ("receive", [] | [Value::Null]) => receive_from(&self.endpoints, object, 65_536)?,
            ("receive", [Value::Integer(maximum)]) => {
                let maximum = usize::try_from(*maximum)
                    .ok()
                    .filter(|value| *value > 0 && *value <= 16 * 1024 * 1024)
                    .ok_or(ProviderError::InvalidArguments(
                        "receive size must be between 1 and 16777216",
                    ))?;
                receive_from(&self.endpoints, object, maximum)?
            }
            ("close", []) => {
                if let Some(HostEndpoint::Stream(stream)) = self
                    .endpoints
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .remove(&object)
                {
                    stream.shutdown(Shutdown::Both).map_err(adapter_error)?;
                }
                ProviderOutcome::result(Value::Null)
                    .with_state(network_state("tcp", "closed", None))
            }
            _ => {
                return Err(ProviderError::UnsupportedCapability(capability.to_owned()));
            }
        };
        self.completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .insert(effect, outcome.clone());
        Ok(outcome)
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["connect", "listen", "accept", "send", "receive", "close"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

fn socket_address(host: &str, port: i64) -> Result<std::net::SocketAddr, ProviderError> {
    let port = u16::try_from(port)
        .map_err(|_| ProviderError::InvalidArguments("port must be between 0 and 65535"))?;
    (host, port)
        .to_socket_addrs()
        .map_err(adapter_error)?
        .next()
        .ok_or_else(|| ProviderError::Adapter("address did not resolve".to_owned()))
}

fn configure_stream(stream: &TcpStream) -> Result<(), ProviderError> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(5))))
        .map_err(adapter_error)
}

fn receive_from(
    endpoints: &Mutex<BTreeMap<ObjectId, HostEndpoint>>,
    object: ObjectId,
    maximum: usize,
) -> Result<ProviderOutcome, ProviderError> {
    let mut endpoints = endpoints.lock().map_err(|_| ProviderError::Unavailable)?;
    let Some(HostEndpoint::Stream(stream)) = endpoints.get_mut(&object) else {
        return Err(ProviderError::Adapter(
            "Endpoint is not connected".to_owned(),
        ));
    };
    let mut bytes = vec![0_u8; maximum];
    let count = stream.read(&mut bytes).map_err(adapter_error)?;
    bytes.truncate(count);
    Ok(ProviderOutcome::result(Value::Bytes(bytes)))
}

fn network_state(transport: &str, status: &str, peer: Option<String>) -> Value {
    Value::Record(BTreeMap::from([
        ("transport".to_owned(), Value::Text(transport.to_owned())),
        ("status".to_owned(), Value::Text(status.to_owned())),
        ("peer".to_owned(), peer.map_or(Value::Null, Value::Text)),
    ]))
}

#[allow(clippy::needless_pass_by_value)]
fn adapter_error(error: std::io::Error) -> ProviderError {
    ProviderError::Adapter(error.to_string())
}

fn run_hosted_process(
    vm: &VirtualMachine,
    process: ObjectId,
    step_limit: u64,
) -> Result<RunReport, String> {
    let mut total = 0_u64;
    let mut output = Vec::new();
    loop {
        let remaining = step_limit.saturating_sub(total);
        if remaining == 0 {
            return Ok(RunReport {
                process,
                steps: total,
                status: ProcessStatus::Running,
                output,
            });
        }
        let report = vm.run(process, remaining).map_err(error_text)?;
        total = total.saturating_add(report.steps);
        output.extend(report.output);
        if report.status != ProcessStatus::Suspended
            || !vm.poll_pending_effect(process).map_err(error_text)?
        {
            return Ok(RunReport {
                process,
                steps: total,
                status: report.status,
                output,
            });
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn print_report(report: &RunReport) {
    if report.status != ProcessStatus::Halted {
        eprintln!("process did not halt");
    }
}

fn parse_object(value: &str) -> Result<ObjectId, String> {
    value
        .parse()
        .map_err(|error| format!("invalid ObjectId: {error}"))
}

fn error_text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn print_help() {
    println!(
        "Ousject system MVP\n\n\
         Commands:\n\
           ousject system-install <system-directory> --local [--state <path>]\n\
           ousject boot [--state <path>]\n\
           ousject compile <source.px> <output.tf>\n\
           ousject run <source.px> [--state <path>] [--steps <count>]\n\
           ousject run-tf <program.tf> [--state <path>] [--steps <count>]\n\
           ousject resume <process-id> [--state <path>] [--steps <count>]\n\
           ousject schedule [process-id ...] [--state <path>] [--steps <count>]\n\
           ousject inspect <object-id> [--state <path>]\n\
           ousject list [--state <path>]\n\
           ousject check [--state <path>]\n\
           ousject types [--state <path>]\n\
           ousject type-register <name> <schema> <public|provider-only> [capability,...] [--state <path>]\n\
           ousject object-create <type-name> <initial-value> [--state <path>]\n\
           ousject object-value <object-id> [--state <path>]\n\
           ousject object-query <type-name> [--capability <name>] [--state <path>]\n\
           ousject object-bind <namespace-id> <name> <target-id> [--state <path>]\n\
           ousject object-unbind <namespace-id> <name> [--state <path>]\n\
           ousject object-resolve <root-id> <path> [--state <path>]\n\
           ousject object-grant <object-id> <subject-id> <capability> [--state <path>]\n\
           ousject object-revoke <object-id> <subject-id> <capability> [--state <path>]\n\
           ousject tf-dump <program.tf>\n\n\
         Use --memory to run without persistent state. Normal commands require --session <token>;\n\
         --local is an explicit development/recovery authority and is never implied."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_network_provider_connects_sends_receives_and_closes() {
        let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = server.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut bytes = [0_u8; 5];
            stream.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"hello");
            stream.write_all(b"world").unwrap();
        });

        let provider = HostNetworkProvider::default();
        let object = ObjectId::new();
        let mut state = provider
            .create(&Value::Map(BTreeMap::from([(
                "transport".to_owned(),
                Value::Text("tcp".to_owned()),
            )])))
            .unwrap();
        let connected = provider
            .invoke(
                object,
                &state,
                "connect",
                &[
                    Value::Text("127.0.0.1".to_owned()),
                    Value::Integer(i64::from(address.port())),
                ],
                ObjectId::new(),
            )
            .unwrap();
        state = connected.object_state.unwrap();
        assert_eq!(
            provider
                .invoke(
                    object,
                    &state,
                    "send",
                    &[Value::Text("hello".to_owned())],
                    ObjectId::new(),
                )
                .unwrap()
                .result,
            Value::Integer(5)
        );
        assert_eq!(
            provider
                .invoke(object, &state, "receive", &[], ObjectId::new())
                .unwrap()
                .result,
            Value::Bytes(b"world".to_vec())
        );
        provider
            .invoke(object, &state, "close", &[], ObjectId::new())
            .unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn host_block_storage_provider_round_trips_a_synced_block() {
        let directory =
            std::env::temp_dir().join(format!("ousject-block-test-{}", ObjectId::new()));
        let path = directory.join("blocks.bin");
        let provider = HostBlockStorageProvider::open(path).unwrap();
        let object = ObjectId::new();
        let bytes = vec![7_u8; 4096];

        assert_eq!(
            provider
                .invoke(
                    object,
                    &Value::Null,
                    "store_block",
                    &[Value::Integer(2), Value::Bytes(bytes.clone())],
                    ObjectId::new(),
                )
                .unwrap()
                .result,
            Value::Integer(4096)
        );
        assert_eq!(
            provider
                .invoke(
                    object,
                    &Value::Null,
                    "load_block",
                    &[Value::Integer(2)],
                    ObjectId::new(),
                )
                .unwrap()
                .result,
            Value::Bytes(bytes)
        );

        drop(provider);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

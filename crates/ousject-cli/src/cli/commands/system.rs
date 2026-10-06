use super::super::{
    AccessContext, BTreeMap, CORE_NAMESPACE_TYPE, CORE_PROGRAM_TYPE, CORE_SYSTEM_TYPE,
    CreateObject, Duration, ObjectId, ObjectQuery, Path, ProcessReaper, ProcessStatus, Program,
    SYSTEM_SUBJECT, TombstoneReaper, Value, compile_source_file, discover_host_hardware,
    error_text, host_block_path, open_manager, parse_options, print_report, run_hosted_process,
};

pub(crate) fn command_system_install(arguments: &[String]) -> Result<(), String> {
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

pub(crate) fn command_boot(arguments: &[String]) -> Result<(), String> {
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
    let _tombstone_reaper = TombstoneReaper::start(vm.manager()).map_err(error_text)?;
    let _process_reaper = ProcessReaper::start(&vm, Duration::from_secs(60)).map_err(error_text)?;
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

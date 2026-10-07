use super::{
    AccessContext, Arc, BTreeMap, CORE_TERMINAL_TYPE, CachedProvider, Capability,
    DEVICE_BLOCK_STORAGE_TYPE, DEVICE_KEYBOARD_TYPE, Duration, HostBlockStorageProvider,
    HostKeyboardProvider, HostNetworkProvider, HostResolverProvider, HostTerminalProvider,
    InMemoryObjectManager, IsTerminal, LinuxTerminal, NET_RESOLVER_TYPE, ObjectId, ObjectQuery,
    Path, PathBuf, ProcessStatus, Program, RunReport, RuntimeOptions, SYSTEM_SUBJECT, SubjectId,
    Value, VirtualMachine, compile_program_with_loader,
};

pub(crate) fn host_block_path(options: &RuntimeOptions) -> PathBuf {
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
pub(crate) fn discover_host_hardware(
    manager: Arc<InMemoryObjectManager>,
    block_path: PathBuf,
) -> Result<VirtualMachine, String> {
    let terminal_state = Value::Record(BTreeMap::from([
        (
            "provider".to_owned(),
            Value::Text("linux.terminal".to_owned()),
        ),
        (
            "interactive".to_owned(),
            Value::Bool(std::io::stdout().is_terminal()),
        ),
    ]));
    let terminal_id =
        VirtualMachine::publish_terminal(&manager, &terminal_state).map_err(error_text)?;
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
        NET_RESOLVER_TYPE,
        &provider_state("linux.dns"),
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
    let terminal = LinuxTerminal::new()?;
    let vm = VirtualMachine::with_terminal_backend(manager.clone(), terminal_id, terminal.clone())
        .map_err(error_text)?;
    let terminal_object = vm
        .manager()
        .query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_TERMINAL_TYPE),
        )
        .map_err(error_text)?
        .into_iter()
        .find(|object| object.parent_id.is_none())
        .map(|object| object.id)
        .ok_or_else(|| "kernel did not publish a Terminal Object".to_owned())?;
    vm.register_provider(Arc::new(HostNetworkProvider::default()))
        .map_err(error_text)?;
    vm.register_provider(Arc::new(HostTerminalProvider::with_manager(
        terminal_object,
        terminal.clone(),
        manager,
    )))
    .map_err(error_text)?;
    vm.register_provider(Arc::new(CachedProvider::new(HostKeyboardProvider {
        terminal,
    })))
    .map_err(error_text)?;
    vm.register_provider(Arc::new(HostResolverProvider))
        .map_err(error_text)?;
    vm.register_provider(Arc::new(CachedProvider::new(
        HostBlockStorageProvider::open(block_path).map_err(error_text)?,
    )))
    .map_err(error_text)?;
    Ok(vm)
}

pub(crate) fn provider_state(provider: &str) -> Value {
    Value::Record(BTreeMap::from([(
        "provider".to_owned(),
        Value::Text(provider.to_owned()),
    )]))
}

pub(crate) fn grant_terminal_access(
    manager: &Arc<InMemoryObjectManager>,
    subject: SubjectId,
) -> Result<(), String> {
    if subject == SYSTEM_SUBJECT {
        return Ok(());
    }
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let mut transaction = manager.begin(context);
    for type_id in [CORE_TERMINAL_TYPE, DEVICE_KEYBOARD_TYPE] {
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

pub(crate) fn run_hosted_process(
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
                status: ProcessStatus::Ready,
                output,
            });
        }
        let report = vm.run(process, remaining).map_err(error_text)?;
        total = total.saturating_add(report.steps);
        output.extend(report.output);
        if report.status == ProcessStatus::Ready {
            continue;
        }
        if !matches!(
            report.status,
            ProcessStatus::Waiting | ProcessStatus::Suspended
        ) {
            return Ok(RunReport {
                process,
                steps: total,
                status: report.status,
                output,
            });
        }
        if vm.poll_pending_effect(process).map_err(error_text)? {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if let Some(delay) = vm.time_until_wake(process).map_err(error_text)? {
            std::thread::sleep(delay);
            let _ = vm.wake_due_timer(process).map_err(error_text)?;
            continue;
        }
        return Ok(RunReport {
            process,
            steps: total,
            status: report.status,
            output,
        });
    }
}

pub(crate) fn print_report(report: &RunReport) {
    if report.status != ProcessStatus::Halted {
        eprintln!("process did not halt");
    }
}

pub(crate) fn parse_object(value: &str) -> Result<ObjectId, String> {
    value
        .parse()
        .map_err(|error| format!("invalid ObjectId: {error}"))
}

pub(crate) fn error_text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub(crate) fn compile_source_file(path: &Path) -> Result<Program, String> {
    let source = std::fs::read_to_string(path).map_err(error_text)?;
    let root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    compile_program_with_loader(&source, |specifier| {
        let mut candidate = root.join(specifier);
        if candidate.extension().is_none() {
            candidate.set_extension("px");
        }
        std::fs::read_to_string(&candidate).map_err(|error| error.to_string())
    })
    .map_err(error_text)
}

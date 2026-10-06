#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Debug)]
struct ModuleTestConsole(Arc<Mutex<Vec<String>>>);

impl ConsoleProvider for ModuleTestConsole {
    fn println(&self, text: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

#[test]
fn praxis_discovers_and_uses_kernel_service_objects() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let vm = vm_with_console(Arc::clone(&manager));
    let program = compile(
        r#"
console = object.find("console")
system = object.find("system")
store = object.find("store")
types = object.find("types")
processes = object.query("core.process")
console.println(system.status())
console.println(store.health_check())
console.println(types.types())
console.println(processes)
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 1_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output.len(), 4);
    assert!(report.output[0].contains("running"), "{:?}", report.output);
    assert!(report.output[1].contains("healthy"));
    assert!(report.output[2].contains("core.system"));
    assert!(report.output[3].contains(&process.to_string()));
}

#[test]
#[allow(clippy::too_many_lines)]
fn local_installs_praxis_module_source_and_program_atomically() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let vm = VirtualMachine::with_console(
        manager.clone(),
        console,
        Arc::new(ModuleTestConsole(Arc::clone(&output))),
    )
    .unwrap();
    let source = r#"
modules = object.find("modules")
module_id = modules.install("greeter", "0.1.0", "func greeting() { return \"hello\" }", ["console.println"])
module_list = modules.modules()
modules.enable(module_id)
terminal = object.find("terminal")
terminal_id = terminal.open()
session = object.find(terminal_id)
terminal_process_id = session.process()
terminal_process = object.find(terminal_process_id)
session.submit("import \"greeter\"\ngreeting()")
terminal_process.wait()
session.submit("answer = greeting()")
terminal_process.wait()
modules.disable(module_id)
session.submit("answer = greeting()")
terminal_process.wait()
upgraded_module_id = modules.upgrade(module_id, "0.2.0", "func greeting() { return \"bonjour\" }", ["console.println"])
modules.enable(upgraded_module_id)
session.submit("import \"greeter\"\ngreeting()")
terminal_process.wait()
session.submit("answer2 = greeting()")
terminal_process.wait()
modules.disable(upgraded_module_id)
modules.rollback(upgraded_module_id, module_id)
modules.enable(module_id)
session.submit("import \"greeter\"\ngreeting()")
terminal_process.wait()
session.submit("answer3 = greeting()")
terminal_process.wait()
modules.disable(module_id)
module_history = modules.modules()
"#;
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 10_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
    let Value::Text(module_id) = vm.variable(process, "module_id").unwrap() else {
        panic!("expected Module Object id");
    };
    let Value::Text(upgraded_module_id) = vm.variable(process, "upgraded_module_id").unwrap()
    else {
        panic!("expected upgraded Module Object id");
    };
    let Value::Array(module_history) = vm.variable(process, "module_history").unwrap() else {
        panic!("expected Module version history");
    };
    assert_eq!(
        vm.variable(process, "module_list"),
        Ok(Value::Array(vec![Value::Text(module_id.clone())]))
    );
    let Value::Text(terminal_process) = vm.variable(process, "terminal_process_id").unwrap() else {
        panic!("expected persistent terminal Process id");
    };
    assert_eq!(
        vm.variable(terminal_process.parse().unwrap(), "answer"),
        Ok(Value::Text("hello".to_owned()))
    );
    assert_eq!(
        vm.variable(terminal_process.parse().unwrap(), "answer2"),
        Ok(Value::Text("bonjour".to_owned()))
    );
    assert_eq!(
        vm.variable(terminal_process.parse().unwrap(), "answer3"),
        Ok(Value::Text("hello".to_owned()))
    );
    let output = output.lock().unwrap();
    assert!(output.iter().any(|line| line == "hello"));
    assert!(output.iter().any(|line| line == "bonjour"));
    assert_eq!(
        output
            .iter()
            .filter(|line| line.as_str() == "hello")
            .count(),
        2
    );
    drop(output);
    assert_eq!(module_history.len(), 2);
    assert!(module_history.contains(&Value::Text(module_id.clone())));
    assert!(module_history.contains(&Value::Text(upgraded_module_id.clone())));
    let old_module_id: ObjectId = module_id.parse().unwrap();
    let old_module = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), old_module_id)
        .unwrap();
    let Value::Record(old_fields) = old_module else {
        panic!("expected prior Module version metadata");
    };
    assert_eq!(old_fields["version"], Value::Text("0.1.0".to_owned()));
    assert_eq!(old_fields["status"], Value::Text("disabled".to_owned()));
    assert_eq!(
        old_fields["source"],
        Value::Text("func greeting() { return \"hello\" }".to_owned())
    );
    assert_eq!(
        old_fields["source_sha256"],
        Value::Text("9a3becc6f9ef40fc41dab28f878b27136b8a9ecfa4e086c889a0fb53cc6920fe".to_owned())
    );

    let upgraded_module_id: ObjectId = upgraded_module_id.parse().unwrap();
    let upgraded_module = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), upgraded_module_id)
        .unwrap();
    let Value::Record(fields) = upgraded_module else {
        panic!("expected upgraded Module metadata");
    };
    assert_eq!(fields["name"], Value::Text("greeter".to_owned()));
    assert_eq!(fields["version"], Value::Text("0.2.0".to_owned()));
    assert_eq!(fields["status"], Value::Text("superseded".to_owned()));
    assert_eq!(
        fields["capabilities"],
        Value::Array(vec![Value::Text("console.println".to_owned())])
    );
    let program_id: ObjectId = match &fields["program"] {
        Value::Text(program) => program.parse().unwrap(),
        _ => panic!("expected linked Program id"),
    };
    assert_eq!(
        manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), program_id)
            .unwrap()
            .parent_id,
        Some(upgraded_module_id)
    );
    assert!(
        tf_format::Program::decode(
            manager
                .read(AccessContext::new(SYSTEM_SUBJECT), program_id)
                .unwrap()
                .state()
        )
        .is_ok()
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn praxis_modules_lock_dependencies_and_verify_source_hashes() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let vm = VirtualMachine::with_console(
        manager.clone(),
        console,
        Arc::new(ModuleTestConsole(output)),
    )
    .unwrap();
    let source = r#"
modules = object.find("modules")
dependency_id = modules.install("greeter", "1.0.0", "func greeting() { return \"hello\" }", [])
modules.enable(dependency_id)
consumer_id = modules.install("wrapper", "1.0.0", "import \"greeter\"\nfunc wrapped_greeting() { return greeting() }", [], [dependency_id])
modules.enable(consumer_id)
terminal = object.find("terminal")
terminal_id = terminal.open()
session = object.find(terminal_id)
session_process_id = session.process()
session_process = object.find(session_process_id)
session.submit("import \"wrapper\"\nanswer = wrapped_greeting()")
session_process.wait()
dependency_v2 = modules.upgrade(dependency_id, "2.0.0", "func greeting() { return \"new\" }", [])
modules.enable(dependency_v2)
session.submit("import \"wrapper\"\nanswer2 = wrapped_greeting()")
session_process.wait()
module_instances = modules.instances(consumer_id)
"#;
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 10_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
    let Value::Text(terminal_process) = vm.variable(process, "session_process_id").unwrap() else {
        panic!("expected terminal Process id");
    };
    let terminal_process = terminal_process.parse().unwrap();
    assert_eq!(
        vm.variable(terminal_process, "answer"),
        Ok(Value::Text("hello".to_owned()))
    );
    assert_eq!(
        vm.variable(terminal_process, "answer2"),
        Ok(Value::Text("hello".to_owned()))
    );
    assert!(matches!(
        vm.variable(process, "module_instances"),
        Ok(Value::Array(ref instances)) if instances.len() == 1
    ));
    assert_eq!(
        manager
            .query(
                AccessContext::new(SYSTEM_SUBJECT),
                &ObjectQuery::new().with_type(CORE_MODULE_INSTANCE_TYPE),
            )
            .unwrap()
            .len(),
        2,
        "the terminal session has one persistent instance for each imported Module"
    );

    let invalid = vm
        .create_process(
            &compile(
                r#"modules = object.find("modules")
bad = modules.install("bad-wrapper", "1.0.0", "import \"greeter\"", [])"#,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(vm.run(invalid, 1_000).is_err());
    assert_eq!(
        manager
            .query(
                AccessContext::new(SYSTEM_SUBJECT),
                &ObjectQuery::new().with_type(CORE_MODULE_TYPE),
            )
            .unwrap()
            .len(),
        3,
        "failed module installation must not leave a partial Module"
    );

    let dependency_id = match vm.variable(process, "dependency_id").unwrap() {
        Value::Text(id) => id.parse().unwrap(),
        _ => panic!("expected dependency Module id"),
    };
    let dependency_view = manager
        .read(AccessContext::new(SYSTEM_SUBJECT), dependency_id)
        .unwrap();
    let Value::Record(mut dependency_fields) = Value::decode(dependency_view.state()).unwrap()
    else {
        panic!("expected dependency Module metadata");
    };
    dependency_fields.insert(
        "source".to_owned(),
        Value::Text("func greeting() { return \"tampered\" }".to_owned()),
    );
    let mut transaction = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
    transaction
        .expect(dependency_id, dependency_view.header().version)
        .update_state(
            dependency_id,
            Value::Record(dependency_fields).encode().unwrap(),
        );
    manager.commit(transaction).unwrap();

    let tampered_import = vm
        .create_process(
            &compile(
                r#"terminal = object.find("terminal")
session = object.find(terminal.open())
session.submit("import \"wrapper\"")"#,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(vm.run(tampered_import, 1_000).is_err());
}

#[test]
#[allow(clippy::too_many_lines)]
fn module_uninstall_respects_dependencies_and_active_instances() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let vm = vm_with_console(Arc::clone(&manager));
    let setup = compile(
        r#"
modules = object.find("modules")
base_id = modules.install("base", "1.0.0", "func value() { return 1 }", [])
modules.enable(base_id)
consumer_id = modules.install("consumer", "1.0.0", "import \"base\"\nfunc read_value() { return value() }", [], [base_id])
unused_id = modules.install("unused", "1.0.0", "func spare() { return 0 }", [])
modules.enable(consumer_id)
"#,
    )
    .unwrap();
    let setup_process = vm.create_process(&setup).unwrap();
    assert_eq!(
        vm.run(setup_process, 2_000).unwrap().status,
        ProcessStatus::Halted
    );
    let Value::Text(base_id) = vm.variable(setup_process, "base_id").unwrap() else {
        panic!("expected base Module id");
    };
    let Value::Text(consumer_id) = vm.variable(setup_process, "consumer_id").unwrap() else {
        panic!("expected consumer Module id");
    };
    let Value::Text(unused_id) = vm.variable(setup_process, "unused_id").unwrap() else {
        panic!("expected unused Module id");
    };
    let consumer: ObjectId = consumer_id.parse().unwrap();
    let Value::Record(consumer_fields) = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), consumer)
        .unwrap()
    else {
        panic!("expected consumer metadata");
    };
    let consumer_program: ObjectId = match &consumer_fields["program"] {
        Value::Text(id) => id.parse().unwrap(),
        _ => panic!("expected Program id"),
    };

    let remove_dependency = vm
        .create_process(
            &compile(&format!(
                "modules = object.find(\"modules\")\nmodules.uninstall(\"{base_id}\")"
            ))
            .unwrap(),
        )
        .unwrap();
    assert!(vm.run(remove_dependency, 1_000).is_err());

    let remove_consumer = vm
        .create_process(
            &compile(&format!(
                "modules = object.find(\"modules\")\nmodules.uninstall(\"{consumer_id}\")"
            ))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        vm.run(remove_consumer, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    assert_eq!(
        manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), consumer)
            .unwrap()
            .lifecycle,
        LifecycleState::Tombstoned
    );
    assert_eq!(
        manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), consumer_program)
            .unwrap()
            .lifecycle,
        LifecycleState::Tombstoned,
        "uninstall retires the Module and its Program in one transaction"
    );

    let import_base = compile(
        r#"
terminal = object.find("terminal")
session = object.find(terminal.open())
session_process = object.find(session.process())
session.submit("import \"base\"")
session_process.wait()
"#,
    )
    .unwrap();
    let import_process = vm.create_process(&import_base).unwrap();
    assert_eq!(
        vm.run(import_process, 2_000).unwrap().status,
        ProcessStatus::Halted
    );
    let remove_loaded = vm
        .create_process(
            &compile(&format!(
                "modules = object.find(\"modules\")\nmodules.uninstall(\"{base_id}\")"
            ))
            .unwrap(),
        )
        .unwrap();
    assert!(vm.run(remove_loaded, 1_000).is_err());

    let unused: ObjectId = unused_id.parse().unwrap();
    let remove_unused = vm
        .create_process(
            &compile(&format!(
                "modules = object.find(\"modules\")\nmodules.uninstall(\"{unused_id}\")"
            ))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        vm.run(remove_unused, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    assert_eq!(
        manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), unused)
            .unwrap()
            .lifecycle,
        LifecycleState::Tombstoned
    );
    assert_eq!(
        manager
            .query(
                AccessContext::new(SYSTEM_SUBJECT),
                &ObjectQuery::new().with_type(CORE_MODULE_TYPE),
            )
            .unwrap()
            .len(),
        1,
        "only the loaded base Module remains active"
    );
}

#[test]
fn enabled_modules_and_locked_dependencies_survive_store_reopen() {
    let directory =
        std::env::temp_dir().join(format!("ousject-module-recovery-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let dependency_id;
    let wrapper_id;
    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_console(Arc::clone(&manager));
        let install = compile(
            r#"
modules = object.find("modules")
dependency_id = modules.install("greeter", "1.0.0", "func greeting() { return \"hello\" }", [])
modules.enable(dependency_id)
wrapper_id = modules.install("wrapper", "1.0.0", "import \"greeter\"\nfunc wrapped_greeting() { return greeting() }", [], [dependency_id])
modules.enable(wrapper_id)
"#,
        )
        .unwrap();
        let process = vm.create_process(&install).unwrap();
        assert_eq!(
            vm.run(process, 1_000).unwrap().status,
            ProcessStatus::Halted
        );
        dependency_id = match vm.variable(process, "dependency_id").unwrap() {
            Value::Text(id) => id,
            _ => panic!("expected dependency Module id"),
        };
        wrapper_id = match vm.variable(process, "wrapper_id").unwrap() {
            Value::Text(id) => id,
            _ => panic!("expected wrapper Module id"),
        };
    }

    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_console(Arc::clone(&manager));
        let shell = compile(
            r#"
terminal = object.find("terminal")
session = object.find(terminal.open())
session_process_id = session.process()
session_process = object.find(session_process_id)
session.submit("import \"wrapper\"\nanswer = wrapped_greeting()")
session_process.wait()
"#,
        )
        .unwrap();
        let process = vm.create_process(&shell).unwrap();
        let report = vm.run(process, 2_000).unwrap();
        assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
        let Value::Text(session_process_id) = vm.variable(process, "session_process_id").unwrap()
        else {
            panic!("expected terminal Process id");
        };
        assert_eq!(
            vm.variable(session_process_id.parse().unwrap(), "answer"),
            Ok(Value::Text("hello".to_owned()))
        );
        let instance_query = compile(&format!(
            "modules = object.find(\"modules\")\ninstances = modules.instances(\"{wrapper_id}\")"
        ))
        .unwrap();
        let inspect = vm.create_process(&instance_query).unwrap();
        assert_eq!(
            vm.run(inspect, 1_000).unwrap().status,
            ProcessStatus::Halted
        );
        assert!(matches!(
            vm.variable(inspect, "instances"),
            Ok(Value::Array(ref instances)) if instances.len() == 1
        ));
        let wrapper: ObjectId = wrapper_id.parse().unwrap();
        let Value::Record(wrapper_fields) = manager
            .value(AccessContext::new(SYSTEM_SUBJECT), wrapper)
            .unwrap()
        else {
            panic!("expected persisted wrapper metadata");
        };
        assert_eq!(
            wrapper_fields["dependencies"],
            Value::Array(vec![Value::Text(dependency_id.clone())])
        );
        assert_eq!(wrapper_fields["status"], Value::Text("enabled".to_owned()));
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn praxis_compiles_and_executes_program_objects_through_compiler_service() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let program = compile(
        r#"
console = object.find("console")
compiler = object.find("compiler")
source = "func main() { value = 40 + 2 }"
console.println(compiler.validate(source))
program_id = compiler.compile(source)
program = object.find(program_id)
console.println(program.type)
child_id = program.execute()
child = object.find(child_id)
console.println(child.wait())
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 2_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["true", "core.program", "halted"]);
}

#[test]
fn process_wait_keeps_driving_a_child_suspended_for_console_input() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputConsole {
        lines: Mutex::new(VecDeque::new()),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager, console, driver.clone()).unwrap();
    let delayed_input = Arc::clone(&driver);
    let input_thread = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        delayed_input
            .lines
            .lock()
            .unwrap()
            .push_back("ready".to_owned());
    });
    let program = compile(
        r#"
console = object.find("console")
compiler = object.find("compiler")
source = "func main() { console = object.find(\"console\")\nconsole.read_line() }"
program_id = compiler.compile(source)
child_program = object.find(program_id)
child_id = child_program.execute()
child = object.find(child_id)
console.println(child.wait())
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();

    let report = vm.run(process, 2_000).unwrap();
    input_thread.join().unwrap();

    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["halted"]);
    assert!(driver.reads.load(Ordering::SeqCst) >= 2);
}

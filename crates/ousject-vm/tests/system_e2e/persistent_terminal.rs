#![allow(clippy::wildcard_imports)]

use super::*;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
struct RecordingTerminal(Arc<Mutex<Vec<String>>>);

impl TerminalProvider for RecordingTerminal {
    fn println(&self, text: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

#[derive(Debug)]
struct InterruptingTerminal(AtomicUsize);

impl TerminalProvider for InterruptingTerminal {
    fn println(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn is_interactive(&self) -> bool {
        true
    }

    fn take_interrupt(&self, _process: ObjectId) -> Result<bool, String> {
        let mut remaining = self.0.load(Ordering::Acquire);
        loop {
            if remaining == 0 {
                return Ok(false);
            }
            match self.0.compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(true),
                Err(current) => remaining = current,
            }
        }
    }
}

#[derive(Debug, Default)]
struct DriverDeviceProvider;

impl ObjectProvider for DriverDeviceProvider {
    fn type_id(&self) -> TypeId {
        NET_ENDPOINT_TYPE
    }

    fn user_creatable(&self) -> bool {
        true
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Ok(Value::Record(BTreeMap::from([
            ("writes".to_owned(), Value::Integer(0)),
            ("last_payload".to_owned(), Value::Bytes(Vec::new())),
        ])))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        match (capability, arguments) {
            ("input", [Value::Integer(maximum)]) if *maximum > 0 => {
                let Value::Record(fields) = state else {
                    return Err(ProviderError::InvalidArguments(
                        "device state must be a Record",
                    ));
                };
                let Some(Value::Bytes(bytes)) = fields.get("last_payload") else {
                    return Err(ProviderError::InvalidArguments("missing input bytes"));
                };
                let maximum = usize::try_from(*maximum)
                    .map_err(|_| ProviderError::InvalidArguments("input size is too large"))?;
                Ok(ProviderOutcome::result(Value::Bytes(
                    bytes.iter().copied().take(maximum).collect(),
                )))
            }
            ("output", [Value::Bytes(bytes)]) => {
                let Value::Record(fields) = state else {
                    return Err(ProviderError::InvalidArguments(
                        "device state must be a Record",
                    ));
                };
                let writes = match fields.get("writes") {
                    Some(Value::Integer(writes)) => writes + 1,
                    _ => return Err(ProviderError::InvalidArguments("missing write count")),
                };
                Ok(
                    ProviderOutcome::result(Value::Integer(i64::try_from(bytes.len()).unwrap()))
                        .with_state(Value::Record(BTreeMap::from([
                            ("writes".to_owned(), Value::Integer(writes)),
                            ("last_payload".to_owned(), Value::Bytes(bytes.clone())),
                        ]))),
                )
            }
            _ => Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        }
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["input".to_owned(), "output".to_owned()]
            .into_iter()
            .collect()
    }
}

#[test]
fn type_registry_reports_the_registered_provider_capabilities() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let vm = vm_with_terminal(Arc::clone(&manager));
    vm.register_provider(Arc::new(DriverDeviceProvider))
        .unwrap();

    let program = compile(
        r#"
terminal = object.find("terminal")
types = object.find("types")
descriptor = types.descriptor("net.endpoint")
terminal.println(descriptor)
terminal.println(types.types())
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 1_000).unwrap();

    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output.len(), 2);
    let descriptor = &report.output[0];
    assert!(descriptor.contains("input"), "{descriptor}");
    assert!(descriptor.contains("output"), "{descriptor}");
    for unsupported in ["connect", "listen", "accept", "send", "receive", "close"] {
        assert!(!descriptor.contains(unsupported), "{descriptor}");
    }

    let all_types = &report.output[1];
    let endpoint_name = all_types
        .find("\"name\": Text(\"net.endpoint\")")
        .expect("dynamic type list contains Network Endpoint");
    let endpoint_start = all_types[..endpoint_name]
        .rfind("Record({")
        .expect("Network Endpoint descriptor starts with a Record");
    let endpoint_end = all_types[endpoint_name..]
        .find("}), Record(")
        .map_or(all_types.len(), |offset| endpoint_name + offset + 1);
    let endpoint = &all_types[endpoint_start..endpoint_end];
    assert!(endpoint.contains("Text(\"input\")"), "{endpoint}");
    assert!(endpoint.contains("Text(\"output\")"), "{endpoint}");
    for unsupported in ["connect", "listen", "accept", "send", "receive", "close"] {
        assert!(!endpoint.contains(unsupported), "{endpoint}");
    }
}

#[test]
fn praxis_module_cannot_register_a_host_provider() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let vm = vm_with_terminal(Arc::clone(&manager));
    vm.register_provider(Arc::new(DriverDeviceProvider))
        .unwrap();

    let installer_program = compile(
        r#"
modules = object.find("modules")
module_id = modules.install("host_boundary", "1.0.0", "func attempt_provider() { providers = object.find(\"providers\")\nproviders.register(\"host\") }", [])
modules.enable(module_id)
providers = object.find("providers")
before = providers.providers()
terminal = object.find("terminal")
terminal_id = terminal.shell()
shell_terminal = object.find(terminal_id)
shell_id = shell_terminal.process()
shell = object.find(shell_id)
shell_terminal.submit("import \"host_boundary\"\nattempt_provider()")
shell.wait()
registered = providers.providers()
"#,
    )
    .unwrap();
    let installer = vm.create_process(&installer_program).unwrap();
    // The installer is complete; its interactive Process attempted the Module
    // call and failed without changing the host Provider Registry.
    let report = vm.run(installer, 10_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    let shell_id: ObjectId = match vm.variable(installer, "shell_id").unwrap() {
        Value::Text(id) => id.parse().unwrap(),
        other => panic!("expected Shell Process id, got {other:?}"),
    };
    let shell_state = vm.process_state(shell_id).unwrap();
    assert_eq!(shell_state.status, ProcessStatus::Failed);
    assert!(
        format!("{:?}", shell_state.error).contains("unknown Object capability"),
        "the imported Praxis function should fail at the unavailable host API: {:?}",
        shell_state.error
    );
    let registered = vm.variable(installer, "registered").unwrap();
    assert_eq!(vm.variable(installer, "before"), Ok(registered.clone()));
    let Value::Array(providers) = registered else {
        panic!("expected Provider Type list");
    };
    assert!(providers.contains(&Value::Text(NET_ENDPOINT_TYPE.to_string())));
    assert_eq!(
        vm.register_provider(Arc::new(DriverDeviceProvider)),
        Err(VmError::Provider("Sealed".to_owned()))
    );
}

#[test]
fn terminal_shell_keeps_one_process_and_uses_latest_function_definition() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let terminal =
        VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let vm = VirtualMachine::with_terminal(
        manager.clone(),
        terminal,
        Arc::new(RecordingTerminal(Arc::clone(&output))),
    )
    .unwrap();
    let source = r#"
terminal = object.find("terminal")
terminal_id = terminal.shell()
shell_terminal = object.find(terminal_id)
process_id = shell_terminal.process()
process = object.find(process_id)
shell_terminal.submit("answer = 7")
process.wait()
shell_terminal.submit("func twice(value) { return value * 2 }")
process.wait()
shell_terminal.submit("func twice(value) { return value * 3 }")
process.wait()
shell_terminal.submit("result = twice(answer)")
process.wait()
shell_terminal.submit("class Counter {\nvalue = 0\npublic func add(amount) {\nthis.value = this.value + amount\nreturn this.value\n}\n}")
process.wait()
shell_terminal.submit("counter = object.create(\"Counter\", {})")
process.wait()
shell_terminal.submit("class_result = counter.add(5)")
process.wait()
shell_terminal.submit("twice(7)")
process.wait()
process_again = shell_terminal.process()
again = terminal.shell()
"#;
    let parent = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(parent, 100_000).unwrap();

    assert_eq!(
        report.status,
        ProcessStatus::Halted,
        "report={report:?}, state={:?}",
        vm.process_state(parent)
    );
    let Value::Text(terminal_id) = vm.variable(parent, "terminal_id").unwrap() else {
        panic!("expected a Terminal ObjectId");
    };
    let Value::Text(process_id) = vm.variable(parent, "process_id").unwrap() else {
        panic!("expected a Process ObjectId");
    };
    assert_eq!(
        vm.variable(parent, "again").unwrap(),
        Value::Text(terminal_id.clone())
    );
    assert_eq!(
        vm.variable(parent, "process_again").unwrap(),
        Value::Text(process_id.clone())
    );
    let process_id = process_id.parse().unwrap();
    let state = vm.process_state(process_id).unwrap();
    assert_eq!(
        state.status,
        ProcessStatus::Halted,
        "persistent Process state={state:?}; output={:?}",
        output.lock().unwrap()
    );
    assert_eq!(vm.variable(process_id, "result"), Ok(Value::Integer(21)));
    assert_eq!(
        vm.variable(process_id, "class_result"),
        Ok(Value::Integer(5))
    );
    assert!(output.lock().unwrap().iter().any(|line| line == "21"));
    let shell_terminal = vm
        .manager()
        .value(
            AccessContext::new(SYSTEM_SUBJECT),
            terminal_id.parse().unwrap(),
        )
        .unwrap();
    let Value::Record(fields) = shell_terminal else {
        panic!("expected persisted Terminal state");
    };
    assert_eq!(fields["process"], Value::Text(process_id.to_string()));
}

#[test]
fn system_shell_state_lives_on_terminal_objects() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_terminal(manager.clone());
    let source = r#"
terminal = object.find("terminal")
terminal_capabilities = terminal.capabilities
provider_registry = object.find("providers")
provider_capabilities = provider_registry.capabilities
shell_id = terminal.shell()
shell_terminal = object.find(shell_id)
process_id = shell_terminal.process()
process = object.find(process_id)
shell_terminal.submit("counter = 1")
process.wait()
shell_terminal.submit("counter++")
process.wait()
shell_terminal.submit("counter++")
process.wait()
shell_id_again = terminal.shell()
"#;
    let parent = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(parent, 10_000).unwrap_or_else(|error| {
        panic!(
            "run failed: {error:?}; parent={:?}",
            vm.process_state(parent)
        )
    });
    assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
    assert_eq!(
        vm.variable(parent, "shell_id_again"),
        vm.variable(parent, "shell_id")
    );
    let Value::Array(terminal_capabilities) = vm.variable(parent, "terminal_capabilities").unwrap()
    else {
        panic!("expected actual Terminal capability list");
    };
    assert!(terminal_capabilities.contains(&Value::Text("create".to_owned())));
    assert!(terminal_capabilities.contains(&Value::Text("shell".to_owned())));
    assert!(!terminal_capabilities.contains(&Value::Text("input".to_owned())));
    assert!(terminal_capabilities.contains(&Value::Text("output".to_owned())));
    let Value::Array(provider_capabilities) = vm.variable(parent, "provider_capabilities").unwrap()
    else {
        panic!("expected provider registry capability list");
    };
    assert!(!provider_capabilities.contains(&Value::Text("register".to_owned())));
    let Value::Text(process_id) = vm.variable(parent, "process_id").unwrap() else {
        panic!("expected Terminal Process id");
    };
    let process_id = process_id.parse().unwrap();
    assert_eq!(vm.variable(process_id, "counter"), Ok(Value::Integer(3)));
    let shell_terminals = manager
        .query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_TERMINAL_TYPE),
        )
        .unwrap()
        .into_iter()
        .filter(|terminal| terminal.parent_id.is_some())
        .collect::<Vec<_>>();
    assert_eq!(shell_terminals.len(), 1);
    assert!(matches!(
        vm.register_provider(Arc::new(DriverDeviceProvider)),
        Err(VmError::Provider(message)) if message.contains("Sealed")
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn failed_user_space_driver_process_leaves_device_and_kernel_usable() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_terminal(manager.clone());
    vm.register_provider(Arc::new(DriverDeviceProvider))
        .unwrap();
    let device_state = DriverDeviceProvider.create(&Value::Null).unwrap();
    let device =
        VirtualMachine::publish_provider_object(&manager, NET_ENDPOINT_TYPE, &device_state)
            .unwrap();
    let source = format!(
        "device = object.find(\"{device}\")\nsemantic = object.create(\"core.namespace\", {{}})\nsemantic.link(\"device\", device)\npayload = \"A\"\ndevice.output(payload.utf8_bytes())\nsample = device.input(1)\nservice = object.create(\"core.value\", #sample)\nsemantic.link(\"sample_length\", service)\ndriver_result = missing_driver_symbol"
    );
    let crashing_driver = vm.create_process(&compile(&source).unwrap()).unwrap();
    assert!(matches!(
        vm.run(crashing_driver, 1_000),
        Err(VmError::UndefinedVariable(name)) if name == "missing_driver_symbol"
    ));
    assert_eq!(
        vm.process_state(crashing_driver).unwrap().status,
        ProcessStatus::Failed
    );
    let semantic_id = vm.process_state(crashing_driver).unwrap().variables["semantic"];
    let consumer = vm
        .create_process(
            &compile(&format!(
                "semantic = object.find(\"{semantic_id}\")\nservice_id = semantic.resolve(\"sample_length\")\nservice = object.find(service_id)\nsample_length = service.value"
            ))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        vm.run(consumer, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    assert_eq!(
        vm.variable(consumer, "sample_length"),
        Ok(Value::Integer(1))
    );

    let device_header = manager
        .query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(NET_ENDPOINT_TYPE),
        )
        .unwrap()
        .into_iter()
        .find(|header| header.id == device)
        .expect("host-discovered Device Object remains published");
    assert_eq!(device_header.parent_id, None);
    let capabilities = manager
        .read(AccessContext::new(SYSTEM_SUBJECT), device_header.id)
        .unwrap()
        .capabilities()
        .clone();
    let Value::Record(device_state) = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), device_header.id)
        .unwrap()
    else {
        panic!("expected durable Device state");
    };
    assert_eq!(device_state["writes"], Value::Integer(1));
    assert_eq!(device_state["last_payload"], Value::Bytes(b"A".to_vec()));
    assert_eq!(
        vm.register_provider(Arc::new(DriverDeviceProvider)),
        Err(VmError::Provider("Sealed".to_owned())),
        "a failed user Process cannot mutate the host Provider set"
    );

    // A user-space supervisor may start another Process with the same stable
    // Device Object; this models restart policy without a kernel Driver Manager.
    let restart = vm
        .create_process(
            &compile(&format!(
                "device = object.find(\"{}\")\npayload = \"B\"\ndevice.output(payload.utf8_bytes())",
                device_header.id
            ))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        vm.run(restart, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    let after_restart = manager
        .read(AccessContext::new(SYSTEM_SUBJECT), device_header.id)
        .unwrap();
    assert_eq!(after_restart.capabilities(), &capabilities);
    let Value::Record(device_state) = Value::decode(after_restart.state()).unwrap() else {
        panic!("expected durable Device state after driver restart");
    };
    assert_eq!(device_state["writes"], Value::Integer(2));
    assert_eq!(device_state["last_payload"], Value::Bytes(b"B".to_vec()));

    let subject = SubjectId::new();
    let mut grant_view = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
    grant_view.expect(
        device,
        manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), device)
            .unwrap()
            .version,
    );
    grant_view.grant(device, subject, Capability::ViewValue);
    grant_view.grant(device, subject, Capability::Inspect);
    manager.commit(grant_view).unwrap();
    for source in [
        format!("device = object.find(\"{device}\")\nbytes = device.input(16)"),
        format!(
            "device = object.find(\"{device}\")\npayload = \"denied\"\ndevice.output(payload.utf8_bytes())"
        ),
    ] {
        let process = vm
            .create_process_as(&compile(&source).unwrap(), subject)
            .unwrap();
        assert!(vm.run(process, 1_000).is_err());
        assert_eq!(
            vm.process_state(process).unwrap().status,
            ProcessStatus::Failed
        );
    }
    let Value::Record(unchanged) = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), device)
        .unwrap()
    else {
        panic!("expected Device state after denied operations");
    };
    assert_eq!(unchanged["writes"], Value::Integer(2));

    let mut grant_invoke = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
    grant_invoke.expect(
        device,
        manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), device)
            .unwrap()
            .version,
    );
    grant_invoke.grant(device, subject, Capability::Invoke);
    manager.commit(grant_invoke).unwrap();
    let allowed = vm
        .create_process_as(
            &compile(&format!(
                "device = object.find(\"{device}\")\npayload = \"C\"\ndevice.output(payload.utf8_bytes())"
            ))
            .unwrap(),
            subject,
        )
        .unwrap();
    assert_eq!(
        vm.run(allowed, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    let Value::Record(allowed_state) = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), device)
        .unwrap()
    else {
        panic!("expected Device state after authorized operation");
    };
    assert_eq!(allowed_state["writes"], Value::Integer(3));
}

#[test]
#[allow(clippy::too_many_lines)]
fn terminal_and_process_survive_a_store_restart() {
    let directory = std::env::temp_dir().join(format!("ousject-terminal-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let terminal_id;
    let process_id;
    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let terminal =
            VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
        let vm = VirtualMachine::with_terminal(
            manager,
            terminal,
            Arc::new(RecordingTerminal(Arc::new(Mutex::new(Vec::new())))),
        )
        .unwrap();
        let source = r#"
terminal = object.find("terminal")
terminal_id = terminal.shell()
shell_terminal = object.find(terminal_id)
process_id = shell_terminal.process()
shell_terminal.update_size(120, 40)
shell_terminal.submit("answer = 40")
process = object.find(process_id)
process.wait()
shell_terminal.save_input("unfinished {")
"#;
        let parent = vm.create_process(&compile(source).unwrap()).unwrap();
        let report = vm.run(parent, 10_000).unwrap();
        assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
        let Value::Text(id) = vm.variable(parent, "terminal_id").unwrap() else {
            panic!("expected Terminal id");
        };
        terminal_id = id;
        let Value::Text(id) = vm.variable(parent, "process_id").unwrap() else {
            panic!("expected process id");
        };
        process_id = id;
    }

    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let terminal =
            VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let vm = VirtualMachine::with_terminal(
            manager,
            terminal,
            Arc::new(RecordingTerminal(Arc::clone(&output))),
        )
        .unwrap();
        let source = r#"
terminal = object.find("terminal")
terminal_id = terminal.shell()
shell_terminal = object.find(terminal_id)
process_id = shell_terminal.process()
pending_before = shell_terminal.pending_input()
history_before = shell_terminal.history()
shell_terminal.save_input("")
shell_terminal.submit("answer++")
process = object.find(process_id)
process.wait()
shell_terminal.submit("answer")
process.wait()
"#;
        let parent = vm.create_process(&compile(source).unwrap()).unwrap();
        let report = vm.run(parent, 10_000).unwrap();
        assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
        assert_eq!(
            vm.variable(parent, "terminal_id"),
            Ok(Value::Text(terminal_id.clone()))
        );
        assert_eq!(
            vm.variable(parent, "process_id"),
            Ok(Value::Text(process_id.clone()))
        );
        assert_eq!(
            vm.variable(parent, "pending_before"),
            Ok(Value::Text("unfinished {".to_owned()))
        );
        assert_eq!(
            vm.variable(parent, "history_before"),
            Ok(Value::Array(vec![Value::Text("answer = 40".to_owned())]))
        );
        let process = process_id.parse().unwrap();
        assert_eq!(vm.variable(process, "answer"), Ok(Value::Integer(41)));
        assert!(output.lock().unwrap().iter().any(|line| line == "41"));
        let shell_terminal = vm
            .manager()
            .value(
                AccessContext::new(SYSTEM_SUBJECT),
                terminal_id.parse().unwrap(),
            )
            .unwrap();
        let Value::Record(shell_terminal) = shell_terminal else {
            panic!("expected persistent Terminal state");
        };
        assert_eq!(shell_terminal["columns"], Value::Integer(120));
        assert_eq!(shell_terminal["rows"], Value::Integer(40));
    }

    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn terminal_shells_are_separate_for_each_process_user() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_shell_terminal(manager.clone());
    let setup = vm
        .create_process(
            &compile(
                r#"
authentication = object.find("authentication")
users = object.find("users")
authentication.initialize_local("local-password")
users.create_user("alice", "alice-password")
identity = authentication.login("alice", "alice-password")
"#,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(vm.run(setup, 5_000).unwrap().status, ProcessStatus::Halted);
    let Value::Record(identity) = vm.variable(setup, "identity").unwrap() else {
        panic!("expected Alice identity");
    };
    let Value::Text(subject) = &identity["subject"] else {
        panic!("expected Alice subject");
    };
    let alice: SubjectId = subject.parse().unwrap();

    let root_process = vm
        .create_process(
            &compile("terminal = object.find(\"terminal\")\nshell_terminal = terminal.shell()")
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        vm.run(root_process, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    let Value::Text(root_shell_terminal) = vm.variable(root_process, "shell_terminal").unwrap()
    else {
        panic!("expected root shell_terminal");
    };

    let alice_process = vm
        .create_process_as(
            &compile("terminal = object.find(\"terminal\")\nshell_terminal = terminal.shell()")
                .unwrap(),
            alice,
        )
        .unwrap();
    assert_eq!(
        vm.run(alice_process, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    let Value::Text(alice_shell_terminal) = vm.variable(alice_process, "shell_terminal").unwrap()
    else {
        panic!("expected Alice shell_terminal");
    };
    assert_ne!(root_shell_terminal, alice_shell_terminal);
    let root_id = root_shell_terminal.parse().unwrap();
    let alice_id = alice_shell_terminal.parse().unwrap();
    let root_record = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), root_id)
        .unwrap();
    let alice_record = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), alice_id)
        .unwrap();
    let Value::Record(root_fields) = root_record else {
        panic!("expected root shell_terminal record");
    };
    let Value::Record(alice_fields) = alice_record else {
        panic!("expected Alice shell_terminal record");
    };
    assert_eq!(
        root_fields["owner"],
        Value::Text(SYSTEM_SUBJECT.to_string())
    );
    assert_eq!(alice_fields["owner"], Value::Text(alice.to_string()));
}

#[test]
fn cancel_stops_the_current_submission_but_keeps_the_terminal_open() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_shell_terminal(manager);
    let source = r#"
terminal = object.find("terminal")
terminal_id = terminal.shell()
shell_terminal = object.find(terminal_id)
process_id = shell_terminal.process()
process = object.find(process_id)
shell_terminal.submit("count = 1")
process.wait()
shell_terminal.submit("count = 100")
shell_terminal.cancel()
process.wait()
shell_terminal.submit("count++")
process.wait()
"#;
    let parent = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(parent, 10_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
    let Value::Text(process_id) = vm.variable(parent, "process_id").unwrap() else {
        panic!("expected terminal Process id");
    };
    assert_eq!(
        vm.variable(process_id.parse().unwrap(), "count"),
        Ok(Value::Integer(2))
    );
}

#[test]
fn ctrl_c_interrupts_a_running_submission_and_the_same_process_continues() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let terminal =
        VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let vm = VirtualMachine::with_terminal(
        manager,
        terminal,
        Arc::new(InterruptingTerminal(AtomicUsize::new(2))),
    )
    .unwrap();
    let source = r#"
terminal = object.find("terminal")
terminal_id = terminal.shell()
shell_terminal = object.find(terminal_id)
process_id = shell_terminal.process()
process = object.find(process_id)
shell_terminal.submit("counter = 1")
process.wait()
shell_terminal.submit("while true { counter++ }")
interrupt_result = process.wait()
shell_terminal.submit("counter++")
process.wait()
"#;
    let parent = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(parent, 10_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
    assert_eq!(
        vm.variable(parent, "interrupt_result"),
        Ok(Value::Text("halted".to_owned()))
    );
    let Value::Text(process) = vm.variable(parent, "process_id").unwrap() else {
        panic!("expected terminal Process id");
    };
    let process = process.parse().unwrap();
    assert!(matches!(vm.variable(process, "counter"), Ok(Value::Integer(value)) if value > 1));
    assert_eq!(
        vm.process_state(process).unwrap().status,
        ProcessStatus::Halted
    );
}

#![allow(clippy::wildcard_imports)]

use super::*;

#[test]
fn assignment_copies_value_into_an_independent_object() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm
        .create_process(
            &compile(
                "console = object.find(\"console\")\nitem = object.create(\"core.text\", \"one\")\ncopy = item\nconsole.println(copy == item)\nconsole.println(copy.value)",
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(vm.run(process, 100).unwrap().output, vec!["true", "one"]);
    assert_eq!(
        vm.variable(process, "copy"),
        Ok(Value::Text("one".to_owned()))
    );
    let bindings = vm.process_state(process).unwrap().variables;
    assert_ne!(bindings["item"], bindings["copy"]);
}

#[test]
fn assignment_and_explicit_core_value_creation_bind_the_same_kind_of_object() {
    let source = r#"
x = 46
note = object.create("core.value", 42)
hex_text = "00000000000000000000000000000001"
aaa = object.find("console")
aaa.println(x + 1)
aaa.println(note + 1)
aaa.println(x.type)
aaa.println(note.type)
aaa.println(x.value)
aaa.println(note.value)
aaa.println(hex_text.value)
aaa.println("named console object")
"#;
    let program = compile(source).unwrap();
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&program).unwrap();
    assert_eq!(
        vm.run(process, 200).unwrap().output,
        vec![
            "47",
            "43",
            "core.value",
            "core.value",
            "46",
            "42",
            "00000000000000000000000000000001",
            "named console object"
        ]
    );
    let state = vm.process_state(process).unwrap();
    let context = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);
    for name in ["x", "note"] {
        let header = vm
            .manager()
            .inspect(context, state.variables[name])
            .unwrap();
        assert_eq!(header.type_id, CORE_VALUE_TYPE);
        assert_eq!(header.parent_id, Some(process));
    }
    assert_eq!(vm.variable(process, "x"), Ok(Value::Integer(46)));
    assert_eq!(vm.variable(process, "note"), Ok(Value::Integer(42)));
    let console = vm.manager().read(context, process).unwrap().links()["console"];
    assert_eq!(state.variables["aaa"], console);
    assert_eq!(
        vm.manager().inspect(context, console).unwrap().type_id,
        CORE_CONSOLE_TYPE
    );
}

#[derive(Debug, Default)]
struct RenderConsole {
    frames: Mutex<Vec<Vec<u8>>>,
}

impl ConsoleProvider for RenderConsole {
    fn println(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn render(&self, frame: &[u8]) -> Result<(), String> {
        self.frames.lock().unwrap().push(frame.to_vec());
        Ok(())
    }
}

#[test]
fn transient_console_render_batches_process_state_without_creating_effects() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(RenderConsole::default());
    let vm = VirtualMachine::with_console(manager.clone(), console, driver.clone()).unwrap();
    let process = vm
        .create_process(
            &compile(
                "console = object.find(\"console\")\nconsole.render(\"frame 1\")\nconsole.render(\"frame 2\")",
            )
            .unwrap(),
        )
        .unwrap();

    let report = vm.run(process, 100).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, Vec::<String>::new());
    assert_eq!(
        *driver.frames.lock().unwrap(),
        vec![b"frame 1".to_vec(), b"frame 2".to_vec()]
    );
    let context = AccessContext::new(SYSTEM_SUBJECT);
    assert_eq!(
        manager
            .query(context, &ObjectQuery::new().with_type(CORE_EFFECT_TYPE))
            .unwrap(),
        Vec::<oms_types::ObjectHeader>::new()
    );
}

#[derive(Debug, Default)]
pub(super) struct FaultBackend {
    bytes: Mutex<Option<Vec<u8>>>,
    pub(super) fail_next: AtomicBool,
}

impl SnapshotBackend for FaultBackend {
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError> {
        Ok(self.bytes.lock().unwrap().clone())
    }

    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(OmsError::Storage("forced failure".to_owned()));
        }
        *self.bytes.lock().unwrap() = Some(snapshot.to_vec());
        Ok(())
    }
}

#[derive(Debug)]
struct FaultingConsole {
    backend: Arc<FaultBackend>,
    deliveries: AtomicUsize,
}

impl ConsoleProvider for FaultingConsole {
    fn println(&self, _text: &str) -> Result<(), String> {
        self.deliveries.fetch_add(1, Ordering::SeqCst);
        self.backend.fail_next.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn console_output_has_a_durable_effect_and_is_not_repeated_after_completion_failure() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(FaultingConsole {
        backend,
        deliveries: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager.clone(), console, driver.clone()).unwrap();
    let program =
        compile("console = object.find(\"console\")\nconsole.println(\"hello\")").unwrap();
    let call = program
        .tokens
        .iter()
        .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "println"))
        .unwrap();
    let process = vm.create_process(&program).unwrap();
    while usize::try_from(vm.process_state(process).unwrap().token_position).unwrap() < call {
        vm.run(process, 1).unwrap();
    }

    let failed_attempt = vm.run(process, 1);
    assert!(
        matches!(failed_attempt, Err(VmError::Oms(OmsError::Storage(_)))),
        "expected output completion persistence to fail, got {failed_attempt:?}"
    );
    assert_eq!(driver.deliveries.load(Ordering::SeqCst), 1);
    assert!(
        manager
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), process)
            .unwrap()
            .links()
            .contains_key("$effect")
    );

    assert!(vm.poll_pending_effect(process).unwrap());
    let report = vm.run(process, 100).unwrap();
    assert_eq!(report.output, ["hello"]);
    assert_eq!(driver.deliveries.load(Ordering::SeqCst), 1);
}

#[derive(Debug)]
struct TestEffectProvider {
    backend: Arc<FaultBackend>,
    outcomes: Mutex<BTreeMap<ObjectId, ProviderOutcome>>,
    invocations: AtomicUsize,
    recovery_policy: EffectRecoveryPolicy,
}

impl ObjectProvider for TestEffectProvider {
    fn type_id(&self) -> TypeId {
        NET_ENDPOINT_TYPE
    }

    fn user_creatable(&self) -> bool {
        true
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Ok(Value::Record(BTreeMap::from([(
            "status".to_owned(),
            Value::Text("new".to_owned()),
        )])))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if let Some(outcome) = self.outcomes.lock().unwrap().get(&effect).cloned() {
            return Ok(outcome);
        }
        assert_eq!(capability, "send");
        assert_eq!(arguments, [Value::Text("hello".to_owned())]);
        self.invocations.fetch_add(1, Ordering::SeqCst);
        let outcome =
            ProviderOutcome::result(Value::Integer(5)).with_state(Value::Record(BTreeMap::from([
                ("status".to_owned(), Value::Text("sent".to_owned())),
            ])));
        self.outcomes
            .lock()
            .unwrap()
            .insert(effect, outcome.clone());
        self.backend.fail_next.store(true, Ordering::SeqCst);
        Ok(outcome)
    }

    fn capabilities(&self) -> std::collections::BTreeSet<String> {
        ["send".to_owned()].into_iter().collect()
    }

    fn effect_recovery_policy(&self, _capability: &str) -> EffectRecoveryPolicy {
        self.recovery_policy
    }
}

#[test]
fn provider_effect_is_durable_and_idempotent_across_completion_failure() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = VirtualMachine::new(manager.clone());
    let provider = Arc::new(TestEffectProvider {
        backend: backend.clone(),
        outcomes: Mutex::new(BTreeMap::new()),
        invocations: AtomicUsize::new(0),
        recovery_policy: EffectRecoveryPolicy::Manual,
    });
    vm.register_provider(provider.clone()).unwrap();
    let program = compile(
        "endpoint = object.create(\"net.endpoint\", { transport: \"tcp\" })\nsent = endpoint.send(\"hello\")",
    )
    .unwrap();
    let call = program
        .tokens
        .iter()
        .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "send"))
        .unwrap();
    let process = vm.create_process(&program).unwrap();
    while usize::try_from(vm.process_state(process).unwrap().token_position).unwrap() < call {
        vm.run(process, 1).unwrap();
    }

    let failed_attempt = vm.run(process, 1);
    assert!(
        matches!(failed_attempt, Err(VmError::Oms(OmsError::Storage(_)))),
        "expected Effect completion persistence to fail, got {failed_attempt:?}"
    );
    assert_eq!(provider.invocations.load(Ordering::SeqCst), 1);
    let process_view = manager
        .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), process)
        .unwrap();
    let effect = process_view.links()["$effect"];
    assert_eq!(
        EffectRecord::decode(
            manager
                .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), effect)
                .unwrap()
                .state()
        )
        .unwrap()
        .status,
        ousject_provider::EffectStatus::Running
    );

    assert!(vm.poll_pending_effect(process).unwrap());
    assert_eq!(vm.run(process, 10).unwrap().status, ProcessStatus::Halted);
    assert_eq!(provider.invocations.load(Ordering::SeqCst), 1);
    assert_eq!(vm.variable(process, "sent"), Ok(Value::Integer(5)));
    assert!(
        !manager
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), process)
            .unwrap()
            .links()
            .contains_key("$effect")
    );
    let effects = manager
        .query(
            AccessContext::new(ousject_vm::SYSTEM_SUBJECT),
            &oms_runtime::ObjectQuery::new().with_type(CORE_EFFECT_TYPE),
        )
        .unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(
        EffectRecord::decode(
            manager
                .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), effect)
                .unwrap()
                .state()
        )
        .unwrap()
        .status,
        ousject_provider::EffectStatus::Completed
    );
}

fn invoke_provider_until_send(vm: &VirtualMachine, source: &str) -> (ObjectId, u32) {
    let program = compile(source).unwrap();
    let call = u32::try_from(
        program
            .tokens
            .iter()
            .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "send"))
            .unwrap(),
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    while vm.process_state(process).unwrap().token_position < call {
        vm.run(process, 1).unwrap();
    }
    (process, call)
}

#[test]
fn manual_effect_recovery_marks_outcome_unknown_and_local_can_resolve_it() {
    let backend = Arc::new(FaultBackend::default());
    let provider = Arc::new(TestEffectProvider {
        backend: backend.clone(),
        outcomes: Mutex::new(BTreeMap::new()),
        invocations: AtomicUsize::new(0),
        recovery_policy: EffectRecoveryPolicy::Manual,
    });
    let program = "endpoint = object.create(\"net.endpoint\", { transport: \"tcp\" })\nsent = endpoint.send(\"hello\")";
    let (process, effect) = {
        let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
        let vm = VirtualMachine::new(manager.clone());
        vm.register_provider(provider.clone()).unwrap();
        let (process, _) = invoke_provider_until_send(&vm, program);
        assert!(matches!(
            vm.run(process, 1),
            Err(VmError::Oms(OmsError::Storage(_)))
        ));
        let effect = manager
            .read(AccessContext::new(SYSTEM_SUBJECT), process)
            .unwrap()
            .links()["$effect"];
        (process, effect)
    };

    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = VirtualMachine::new(manager.clone());
    vm.register_provider(provider.clone()).unwrap();
    vm.recover_processes().unwrap();
    assert_eq!(
        vm.process_state(process).unwrap().status,
        ProcessStatus::Waiting
    );
    assert_eq!(
        EffectRecord::decode(
            manager
                .read(AccessContext::new(SYSTEM_SUBJECT), effect)
                .unwrap()
                .state()
        )
        .unwrap()
        .status,
        EffectStatus::Unknown
    );

    let resolve = compile(&format!(
        "effect = object.find(\"{effect}\")\neffect.resolve(9)"
    ))
    .unwrap();
    let resolver = vm.create_process(&resolve).unwrap();
    assert_eq!(vm.run(resolver, 100).unwrap().status, ProcessStatus::Halted);
    assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
    assert_eq!(vm.variable(process, "sent"), Ok(Value::Integer(9)));
    assert_eq!(provider.invocations.load(Ordering::SeqCst), 1);
}

#[test]
fn idempotent_effect_recovery_retries_with_the_same_effect_identity() {
    let backend = Arc::new(FaultBackend::default());
    let provider = Arc::new(TestEffectProvider {
        backend: backend.clone(),
        outcomes: Mutex::new(BTreeMap::new()),
        invocations: AtomicUsize::new(0),
        recovery_policy: EffectRecoveryPolicy::RetryIdempotent,
    });
    let program = "endpoint = object.create(\"net.endpoint\", { transport: \"tcp\" })\nsent = endpoint.send(\"hello\")";
    let process = {
        let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
        let vm = VirtualMachine::new(manager);
        vm.register_provider(provider.clone()).unwrap();
        let (process, _) = invoke_provider_until_send(&vm, program);
        assert!(matches!(
            vm.run(process, 1),
            Err(VmError::Oms(OmsError::Storage(_)))
        ));
        process
    };

    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = VirtualMachine::new(manager.clone());
    vm.register_provider(provider.clone()).unwrap();
    let report = CooperativeScheduler::recover(&vm)
        .unwrap()
        .run(100)
        .unwrap();
    assert!(report.total_steps > 0);
    assert_eq!(
        vm.process_state(process).unwrap().status,
        ProcessStatus::Halted
    );
    assert_eq!(provider.invocations.load(Ordering::SeqCst), 1);
    assert_eq!(vm.variable(process, "sent"), Ok(Value::Integer(5)));
}

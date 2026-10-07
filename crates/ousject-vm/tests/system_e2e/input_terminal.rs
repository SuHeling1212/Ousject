#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Debug)]
pub(super) struct InputTerminal {
    pub(super) lines: Mutex<VecDeque<String>>,
    pub(super) reads: AtomicUsize,
}

impl TerminalProvider for InputTerminal {
    fn println(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn try_read_line(&self) -> Result<Option<String>, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.lines.lock().unwrap().pop_front())
    }
}

#[test]
fn praxis_reads_terminal_input_through_a_durable_effect() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let terminal =
        VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputTerminal {
        lines: Mutex::new(VecDeque::from(["你好 Ousject".to_owned()])),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_terminal(manager, terminal, driver.clone()).unwrap();
    let program = compile(
        "terminal = object.find(\"terminal\")\nline = terminal.read_line()\nterminal.println(line)",
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 100).unwrap();

    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["你好 Ousject"]);
    assert_eq!(
        vm.variable(process, "line"),
        Ok(Value::Text("你好 Ousject".to_owned()))
    );
    assert_eq!(driver.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn terminal_input_suspends_only_the_waiting_process_and_reuses_its_effect() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let terminal =
        VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputTerminal {
        lines: Mutex::new(VecDeque::new()),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_terminal(manager, terminal, driver.clone()).unwrap();
    let program = compile(
        "terminal = object.find(\"terminal\")\nline = terminal.read_line()\nterminal.println(line)",
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();

    let waiting = vm.run(process, 1_000).unwrap();
    assert_eq!(waiting.status, ProcessStatus::Waiting);
    let pending_effect = vm
        .manager()
        .read(AccessContext::new(SYSTEM_SUBJECT), process)
        .unwrap()
        .links()["$effect"];
    driver.lines.lock().unwrap().push_back("ready".to_owned());
    assert!(vm.poll_pending_effect(process).unwrap());
    let completed = vm.run(process, 1_000).unwrap();
    assert_eq!(completed.status, ProcessStatus::Halted);
    assert_eq!(completed.output, ["ready"]);
    assert_eq!(driver.reads.load(Ordering::SeqCst), 2);
    let effect = EffectRecord::decode(
        vm.manager()
            .read(AccessContext::new(SYSTEM_SUBJECT), pending_effect)
            .unwrap()
            .state(),
    )
    .unwrap();
    assert_eq!(effect.status, ousject_provider::EffectStatus::Completed);
    assert_eq!(effect.result, Some(Value::Text("ready".to_owned())));
}

#[test]
fn two_terminal_waiters_keep_distinct_effects_and_inputs() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let terminal =
        VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputTerminal {
        lines: Mutex::new(VecDeque::new()),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_terminal(manager, terminal, driver.clone()).unwrap();
    let program = compile(
        "terminal = object.find(\"terminal\")\nline = terminal.read_line()\nterminal.println(line)",
    )
    .unwrap();
    let first = vm.create_process(&program).unwrap();
    let second = vm.create_process(&program).unwrap();
    assert_eq!(vm.run(first, 1_000).unwrap().status, ProcessStatus::Waiting);
    assert_eq!(
        vm.run(second, 1_000).unwrap().status,
        ProcessStatus::Waiting
    );
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let first_effect = vm.manager().read(context, first).unwrap().links()["$effect"];
    let second_effect = vm.manager().read(context, second).unwrap().links()["$effect"];
    assert_ne!(first_effect, second_effect);

    driver
        .lines
        .lock()
        .unwrap()
        .extend(["first".to_owned(), "second".to_owned()]);
    assert!(vm.poll_pending_effect(first).unwrap());
    assert!(vm.poll_pending_effect(second).unwrap());
    assert_eq!(vm.run(first, 1_000).unwrap().output, ["first"]);
    assert_eq!(vm.run(second, 1_000).unwrap().output, ["second"]);
}

#[test]
fn pending_terminal_input_recovers_after_store_reopen() {
    let directory =
        std::env::temp_dir().join(format!("ousject-input-recovery-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let process;
    let terminal;
    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        terminal =
            VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
        let driver = Arc::new(InputTerminal {
            lines: Mutex::new(VecDeque::new()),
            reads: AtomicUsize::new(0),
        });
        let vm = VirtualMachine::with_terminal(manager, terminal, driver).unwrap();
        let program = compile(
            "terminal = object.find(\"terminal\")\nline = terminal.read_line()\nterminal.println(line)",
        )
        .unwrap();
        process = vm.create_process(&program).unwrap();
        assert_eq!(
            vm.run(process, 1_000).unwrap().status,
            ProcessStatus::Waiting
        );
    }

    let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
    let driver = Arc::new(InputTerminal {
        lines: Mutex::new(VecDeque::from(["after restart".to_owned()])),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_terminal(manager, terminal, driver).unwrap();
    assert!(vm.poll_pending_effect(process).unwrap());
    let report = vm.run(process, 1_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["after restart"]);
    drop(vm);
    std::fs::remove_dir_all(directory).unwrap();
}

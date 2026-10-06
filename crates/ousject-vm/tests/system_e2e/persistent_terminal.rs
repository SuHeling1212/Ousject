#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Debug, Clone)]
struct RecordingConsole(Arc<Mutex<Vec<String>>>);

impl ConsoleProvider for RecordingConsole {
    fn println(&self, text: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

#[derive(Debug)]
struct InterruptingConsole(AtomicUsize);

impl ConsoleProvider for InterruptingConsole {
    fn println(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn is_interactive(&self) -> bool {
        true
    }

    fn take_interrupt(&self, _process: ObjectId) -> Result<bool, String> {
        Ok(self
            .0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                if remaining > 0 {
                    Some(remaining - 1)
                } else {
                    None
                }
            })
            .is_ok())
    }
}

#[test]
fn terminal_session_keeps_one_process_and_uses_latest_function_definition() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let output = Arc::new(Mutex::new(Vec::new()));
    let vm = VirtualMachine::with_console(
        manager.clone(),
        console,
        Arc::new(RecordingConsole(Arc::clone(&output))),
    )
    .unwrap();
    let source = r#"
console = object.find("console")
terminal = object.find("terminal")
session_id = terminal.open()
session = object.find(session_id)
process_id = session.process()
process = object.find(process_id)
session.submit("answer = 7")
process.wait()
session.submit("func twice(value) { return value * 2 }")
process.wait()
session.submit("func twice(value) { return value * 3 }")
process.wait()
session.submit("result = twice(answer)")
process.wait()
session.submit("class Counter {\nvalue = 0\npublic func add(amount) {\nthis.value = this.value + amount\nreturn this.value\n}\n}")
process.wait()
session.submit("counter = object.create(\"Counter\", {})")
process.wait()
session.submit("class_result = counter.add(5)")
process.wait()
session.submit("twice(7)")
process.wait()
process_again = session.process()
again = terminal.open()
"#;
    let parent = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(parent, 100_000).unwrap();

    assert_eq!(
        report.status,
        ProcessStatus::Halted,
        "report={report:?}, state={:?}",
        vm.process_state(parent)
    );
    let Value::Text(session_id) = vm.variable(parent, "session_id").unwrap() else {
        panic!("expected a Terminal Session ObjectId");
    };
    let Value::Text(process_id) = vm.variable(parent, "process_id").unwrap() else {
        panic!("expected a Process ObjectId");
    };
    assert_eq!(
        vm.variable(parent, "again").unwrap(),
        Value::Text(session_id.clone())
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
    let session = vm
        .manager()
        .value(
            AccessContext::new(SYSTEM_SUBJECT),
            session_id.parse().unwrap(),
        )
        .unwrap();
    let Value::Record(fields) = session else {
        panic!("expected persisted Terminal Session state");
    };
    assert_eq!(fields["process"], Value::Text(process_id.to_string()));
}

#[test]
fn terminal_session_and_process_survive_a_store_restart() {
    let directory = std::env::temp_dir().join(format!("ousject-terminal-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let session_id;
    let process_id;
    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let console =
            VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
        let vm = VirtualMachine::with_console(
            manager,
            console,
            Arc::new(RecordingConsole(Arc::new(Mutex::new(Vec::new())))),
        )
        .unwrap();
        let source = r#"
terminal = object.find("terminal")
session_id = terminal.open()
session = object.find(session_id)
process_id = session.process()
session.update_size(120, 40)
session.submit("answer = 40")
process = object.find(process_id)
process.wait()
session.save_input("unfinished {")
"#;
        let parent = vm.create_process(&compile(source).unwrap()).unwrap();
        let report = vm.run(parent, 10_000).unwrap();
        assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
        let Value::Text(id) = vm.variable(parent, "session_id").unwrap() else {
            panic!("expected session id");
        };
        session_id = id;
        let Value::Text(id) = vm.variable(parent, "process_id").unwrap() else {
            panic!("expected process id");
        };
        process_id = id;
    }

    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let console =
            VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let vm = VirtualMachine::with_console(
            manager,
            console,
            Arc::new(RecordingConsole(Arc::clone(&output))),
        )
        .unwrap();
        let source = r#"
terminal = object.find("terminal")
session_id = terminal.open()
session = object.find(session_id)
process_id = session.process()
pending_before = session.pending_input()
history_before = session.history()
session.save_input("")
session.submit("answer++")
process = object.find(process_id)
process.wait()
session.submit("answer")
process.wait()
"#;
        let parent = vm.create_process(&compile(source).unwrap()).unwrap();
        let report = vm.run(parent, 10_000).unwrap();
        assert_eq!(report.status, ProcessStatus::Halted, "{report:?}");
        assert_eq!(
            vm.variable(parent, "session_id"),
            Ok(Value::Text(session_id.clone()))
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
        let session = vm
            .manager()
            .value(
                AccessContext::new(SYSTEM_SUBJECT),
                session_id.parse().unwrap(),
            )
            .unwrap();
        let Value::Record(session) = session else {
            panic!("expected persistent Terminal Session state");
        };
        assert_eq!(session["columns"], Value::Integer(120));
        assert_eq!(session["rows"], Value::Integer(40));
    }

    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn terminal_sessions_are_separate_for_each_process_user() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager.clone());
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
            &compile("terminal = object.find(\"terminal\")\nsession = terminal.open()").unwrap(),
        )
        .unwrap();
    assert_eq!(
        vm.run(root_process, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    let Value::Text(root_session) = vm.variable(root_process, "session").unwrap() else {
        panic!("expected root session");
    };

    let alice_process = vm
        .create_process_as(
            &compile("terminal = object.find(\"terminal\")\nsession = terminal.open()").unwrap(),
            alice,
        )
        .unwrap();
    assert_eq!(
        vm.run(alice_process, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    let Value::Text(alice_session) = vm.variable(alice_process, "session").unwrap() else {
        panic!("expected Alice session");
    };
    assert_ne!(root_session, alice_session);
    let root_id = root_session.parse().unwrap();
    let alice_id = alice_session.parse().unwrap();
    let root_record = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), root_id)
        .unwrap();
    let alice_record = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), alice_id)
        .unwrap();
    let Value::Record(root_fields) = root_record else {
        panic!("expected root session record");
    };
    let Value::Record(alice_fields) = alice_record else {
        panic!("expected Alice session record");
    };
    assert_eq!(
        root_fields["owner"],
        Value::Text(SYSTEM_SUBJECT.to_string())
    );
    assert_eq!(alice_fields["owner"], Value::Text(alice.to_string()));
}

#[test]
fn cancel_stops_the_current_submission_but_keeps_the_terminal_session_open() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let source = r#"
terminal = object.find("terminal")
session_id = terminal.open()
session = object.find(session_id)
process_id = session.process()
process = object.find(process_id)
session.submit("count = 1")
process.wait()
session.submit("count = 100")
session.cancel()
process.wait()
session.submit("count++")
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
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let vm = VirtualMachine::with_console(
        manager,
        console,
        Arc::new(InterruptingConsole(AtomicUsize::new(2))),
    )
    .unwrap();
    let source = r#"
terminal = object.find("terminal")
session_id = terminal.open()
session = object.find(session_id)
process_id = session.process()
process = object.find(process_id)
session.submit("counter = 1")
process.wait()
session.submit("while true { counter++ }")
interrupt_result = process.wait()
session.submit("counter++")
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

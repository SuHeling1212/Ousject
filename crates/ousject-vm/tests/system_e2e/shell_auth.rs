#![allow(clippy::wildcard_imports)]

use super::persistent_terminal::ShellTerminalProvider;
use super::*;

#[test]
fn praxis_initializes_local_and_logs_in_through_authentication_objects() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let program = compile(
        r#"
console = object.find("console")
authentication = object.find("authentication")
users = object.find("users")
authentication.initialize_local("local-password")
users.create_user("alice", "alice-password")
session = authentication.login("alice", "alice-password")
console.println(session["subject"])
authentication.change_password("alice", "new-password")
authentication.logout(session["token"])
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 2_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output.len(), 1);
    assert_ne!(report.output[0], SYSTEM_SUBJECT.to_string());
    assert_eq!(
        vm.process_state(process).unwrap().subject.to_string(),
        report.output[0]
    );
}

#[test]
fn shell_context_keeps_variables_and_current_user_between_command_processes() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let source = r#"
func main() {
console = object.find("console")
authentication = object.find("authentication")
users = object.find("users")
authentication.initialize_local("local-password")
users.create_user("alice", "alice-password")
authentication.login("alice", "alice-password")
identity = authentication.current_user()
console.println(identity["name"])
a_object = object.create("core.value", 123456789)
a_id_object = object.create("core.value", a_object.id)
context = {"a": a_object.id, "a_id": a_id_object.id}
compiler = object.find("compiler")
    first_id = compiler.compile("func main() {}")
first_program = object.find(first_id)
first_process_id = first_program.execute(context)
first_process = object.find(first_process_id)
first_process.wait()
context = first_process.bindings()
    second_id = compiler.compile("func main() { target = object.find(a_id)\ntarget.replace(a + 1) }")
second_program = object.find(second_id)
second_process_id = second_program.execute(context)
second_process = object.find(second_process_id)
    second_process.wait()
    second_context = second_process.bindings()
    shared_object = object.find(second_context["a"])
console.println(shared_object.value)
}
"#;
    let process = vm
        .create_process(&compile_program(source).unwrap())
        .unwrap();
    let report = vm.run(process, 10_000).unwrap();

    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["alice", "123456790"]);
    let subject = vm.process_state(process).unwrap().subject;
    assert_ne!(subject, SYSTEM_SUBJECT);
}

#[test]
fn system_shell_collects_bracket_and_backslash_continuations() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputConsole {
        lines: Mutex::new(VecDeque::from([
            "\r".to_owned(),
            "a = {".to_owned(),
            "x: 3".to_owned(),
            "}".to_owned(),
            "b = 4 + \\".to_owned(),
            "5".to_owned(),
            "objects".to_owned(),
            "exit".to_owned(),
        ])),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager.clone(), console, driver.clone()).unwrap();
    vm.register_provider(Arc::new(ShellTerminalProvider))
        .unwrap();
    let setup = vm
        .create_process(
                &compile_program(
                    "func main() { authentication = object.find(\"authentication\")\nauthentication.initialize_local(\"local-password\") }",
                )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(vm.run(setup, 1_000).unwrap().status, ProcessStatus::Halted);

    let shell = vm
        .create_process(&compile_program(include_str!("../../../../system/shell.px")).unwrap())
        .unwrap();
    let report = vm.run(shell, 100_000).unwrap();

    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(
        driver.reads.load(Ordering::SeqCst),
        8,
        "Shell output: {:?}",
        report.output
    );
    assert!(
        report
            .output
            .iter()
            .all(|line| !line.starts_with("provider_error")),
        "Shell reported an error: {:?}",
        report.output
    );
    let child_terminals = manager
        .query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_TERMINAL_TYPE),
        )
        .unwrap()
        .into_iter()
        .filter(|terminal| terminal.parent_id.is_some())
        .collect::<Vec<_>>();
    assert_eq!(child_terminals.len(), 1);
    let terminal = manager
        .value(AccessContext::new(SYSTEM_SUBJECT), child_terminals[0].id)
        .unwrap();
    let Value::Record(terminal_fields) = terminal else {
        panic!("Terminal should be stored as a Record");
    };
    let Value::Text(process_id) = &terminal_fields["process"] else {
        panic!("Terminal should refer to its persistent Process");
    };
    let process_id = process_id.parse().unwrap();
    let state = vm.process_state(process_id).unwrap();
    let a = vm.variable(process_id, "a").unwrap();
    let b = vm.variable(process_id, "b").unwrap();
    assert_eq!(
        a,
        Value::Map(BTreeMap::from([("x".to_owned(), Value::Integer(3))]))
    );
    assert_eq!(b, Value::Integer(9));
    assert_eq!(state.subject, vm.process_state(shell).unwrap().subject);
}

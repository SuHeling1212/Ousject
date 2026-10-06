#![allow(clippy::wildcard_imports)]

use super::*;

#[test]
fn praxis_login_and_process_identity_transition_are_one_atomic_commit() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager.clone());
    let program = compile(
        r#"
authentication = object.find("authentication")
users = object.find("users")
authentication.initialize_local("local-password")
users.create_user("alice", "alice-password")
session = authentication.login("alice", "alice-password")
"#,
    )
    .unwrap();
    let login_position = u32::try_from(
        program
            .tokens
            .iter()
            .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "login"))
            .unwrap(),
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    while vm.process_state(process).unwrap().token_position != login_position {
        assert_eq!(vm.run(process, 1).unwrap().status, ProcessStatus::Ready);
    }
    let before = vm.process_state(process).unwrap();

    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    assert_eq!(
        manager
            .query(
                AccessContext::new(SYSTEM_SUBJECT),
                &ObjectQuery::new().with_type(CORE_SESSION_TYPE),
            )
            .unwrap(),
        Vec::<oms_types::ObjectHeader>::new()
    );

    assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
    assert_ne!(vm.process_state(process).unwrap().subject, SYSTEM_SUBJECT);
    assert_eq!(
        manager
            .query(
                AccessContext::new(SYSTEM_SUBJECT),
                &ObjectQuery::new().with_type(CORE_SESSION_TYPE),
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn reserved_local_user_cannot_be_replaced_or_retired_by_praxis() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(Arc::clone(&manager));
    let program = compile(
        r#"
console = object.find("console")
authentication = object.find("authentication")
authentication.initialize_local("password")
found = object.query("core.user")
local = object.find(found[0])
try {
    local.replace({ name: "forged" })
} catch (error) {
    console.println("replace denied")
}
try {
    local.retire()
} catch (error) {
    console.println("retire denied")
}
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 2_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["replace denied", "retire denied"]);
    let users = ousject_auth::AuthService::new(manager).users().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].name, "local");
    assert_eq!(users[0].subject, SYSTEM_SUBJECT);
}

#[test]
fn praxis_process_recovers_and_finishes_after_restart() {
    let source = r#"
count = 0
console = object.find("console")
while count < 5 {
    count++
}
console.println(count)
"#;
    let program = compile(source).unwrap();
    let directory = std::env::temp_dir().join(format!("ousject-e2e-{}", ObjectId::new()));
    let path = directory.join("objects.oms");

    let process = {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_console(manager);
        let process = vm.create_process(&program).unwrap();
        let partial = vm.run(process, 7).unwrap();
        assert_eq!(partial.status, ProcessStatus::Ready);
        assert_eq!(partial.output, Vec::<String>::new());
        process
    };

    let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
    let vm = vm_with_console(manager);
    vm.reconnect_hardware(process).unwrap();
    let completed = vm.run(process, 200).unwrap();
    assert_eq!(completed.status, ProcessStatus::Halted);
    assert_eq!(completed.output, vec!["5"]);
    assert_eq!(vm.variable(process, "count"), Ok(Value::Integer(5)));
    vm.manager().health_check().unwrap();
    drop(vm);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn expired_timer_wakes_after_restart_and_retries_a_failed_fire_commit() {
    let backend = Arc::new(FaultBackend::default());
    let process = {
        let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
        let vm = vm_with_console(manager);
        let program =
            compile("time = object.find(\"time\")\ntime.sleep(10)\nfinished = true").unwrap();
        let process = vm.create_process(&program).unwrap();
        assert_eq!(
            vm.run(process, 1_000).unwrap().status,
            ProcessStatus::Waiting
        );
        process
    };

    std::thread::sleep(std::time::Duration::from_millis(20));
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.recover_processes(),
        Err(VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(
        vm.process_state(process).unwrap().status,
        ProcessStatus::Waiting
    );

    let report = CooperativeScheduler::recover(&vm)
        .unwrap()
        .run(100)
        .unwrap();
    assert!(report.total_steps > 0);
    assert_eq!(
        vm.process_state(process).unwrap().status,
        ProcessStatus::Halted
    );
    assert_eq!(vm.variable(process, "finished"), Ok(Value::Bool(true)));
}

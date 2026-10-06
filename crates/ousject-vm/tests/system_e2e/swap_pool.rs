#![allow(clippy::wildcard_imports)]

use super::*;

#[test]
fn swap_pool_membership_preserves_object_identity_rights_and_conflicts() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let vm = vm_with_console(Arc::clone(&manager));
    let subject_a = SubjectId::new();
    let subject_b = SubjectId::new();
    let create = compile(
        r#"
pool = object.create("core.swap_pool", {})
shared = object.create("core.value", 1)
pool.attach("counter", shared.id)
pool_id = pool.id
shared_id = shared.id
"#,
    )
    .unwrap();
    let owner = vm.create_process_as(&create, subject_a).unwrap();
    assert_eq!(vm.run(owner, 1_000).unwrap().status, ProcessStatus::Halted);
    let Value::Text(pool_id) = vm.variable(owner, "pool_id").unwrap() else {
        panic!("expected SwapPool Object id");
    };
    let Value::Text(shared_id) = vm.variable(owner, "shared_id").unwrap() else {
        panic!("expected shared Object id");
    };
    let pool_id = pool_id.parse::<ObjectId>().unwrap();
    let shared_id = shared_id.parse::<ObjectId>().unwrap();
    let system = AccessContext::new(SYSTEM_SUBJECT);

    let pool_view = manager.read(system, pool_id).unwrap();
    let mut permissions = manager.begin(system);
    permissions
        .expect(pool_id, pool_view.header().version)
        .grant(pool_id, subject_b, Capability::Inspect)
        .grant(pool_id, subject_b, Capability::Invoke);
    manager.commit(permissions).unwrap();

    let denied_lookup = compile(&format!(
        r#"
pool = object.find("{pool_id}")
denied = false
try {{
    pool.get("counter")
}} catch (error) {{
    denied = true
}}
"#
    ))
    .unwrap();
    let reader = vm.create_process_as(&denied_lookup, subject_b).unwrap();
    assert_eq!(vm.run(reader, 1_000).unwrap().status, ProcessStatus::Halted);
    assert_eq!(vm.variable(reader, "denied"), Ok(Value::Bool(true)));

    let detach = compile(&format!(
        r#"
pool = object.find("{pool_id}")
shared = object.find("{shared_id}")
pool.detach("counter")
shared.replace(2)
"#
    ))
    .unwrap();
    let owner = vm.create_process_as(&detach, subject_a).unwrap();
    assert_eq!(vm.run(owner, 1_000).unwrap().status, ProcessStatus::Halted);
    assert_eq!(manager.value(system, shared_id), Ok(Value::Integer(2)));
    assert!(
        !manager
            .read(system, pool_id)
            .unwrap()
            .links()
            .contains_key("member:counter"),
        "detach removes membership without retiring the Object"
    );
    assert_eq!(
        manager.inspect(system, shared_id).unwrap().lifecycle,
        LifecycleState::Active
    );

    let shared_view = manager.read(system, shared_id).unwrap();
    let mut grants = manager.begin(system);
    grants
        .expect(shared_id, shared_view.header().version)
        .grant(shared_id, subject_b, Capability::Inspect)
        .grant(shared_id, subject_b, Capability::ReplaceValue);
    manager.commit(grants).unwrap();
    let version = manager.inspect(system, shared_id).unwrap().version;
    let mut update_a = manager.begin(AccessContext::new(subject_a));
    update_a
        .expect(shared_id, version)
        .update_state(shared_id, Value::Integer(3).encode().unwrap());
    let mut update_b = manager.begin(AccessContext::new(subject_b));
    update_b
        .expect(shared_id, version)
        .update_state(shared_id, Value::Integer(4).encode().unwrap());
    manager.commit(update_a).unwrap();
    assert!(matches!(
        manager.commit(update_b),
        Err(OmsError::Conflict { object, .. }) if object == shared_id
    ));
    assert_eq!(manager.value(system, shared_id), Ok(Value::Integer(3)));
}

#[test]
fn swap_pool_shared_state_is_old_or_fully_committed_after_reopen() {
    let backend = Arc::new(FaultBackend::default());
    let subject = SubjectId::new();
    let (pool, shared) = {
        let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
        let vm = vm_with_console(manager);
        let program = compile(
            "pool = object.create(\"core.swap_pool\", {})\nshared = object.create(\"core.value\", 1)\npool.attach(\"state\", shared.id)\npool_id = pool.id\nshared_id = shared.id",
        )
        .unwrap();
        let process = vm.create_process_as(&program, subject).unwrap();
        assert_eq!(
            vm.run(process, 1_000).unwrap().status,
            ProcessStatus::Halted
        );
        let Value::Text(pool) = vm.variable(process, "pool_id").unwrap() else {
            panic!("expected SwapPool Object id");
        };
        let Value::Text(shared) = vm.variable(process, "shared_id").unwrap() else {
            panic!("expected shared Object id");
        };
        (
            pool.parse::<ObjectId>().unwrap(),
            shared.parse::<ObjectId>().unwrap(),
        )
    };

    {
        let manager = InMemoryObjectManager::open_with_backend(backend.clone()).unwrap();
        let context = AccessContext::new(subject);
        let shared_view = manager.read(context, shared).unwrap();
        let mut interrupted = manager.begin(context);
        interrupted
            .expect(shared, shared_view.header().version)
            .update_state(shared, Value::Integer(2).encode().unwrap());
        backend.fail_next.store(true, Ordering::SeqCst);
        assert!(matches!(
            manager.commit(interrupted),
            Err(OmsError::Storage(_))
        ));
        assert_eq!(manager.value(context, shared), Ok(Value::Integer(1)));
    }

    {
        let manager = InMemoryObjectManager::open_with_backend(backend.clone()).unwrap();
        let context = AccessContext::new(subject);
        assert_eq!(manager.value(context, shared), Ok(Value::Integer(1)));
        assert_eq!(
            manager
                .read(context, pool)
                .unwrap()
                .links()
                .get("member:state"),
            Some(&shared)
        );
        let shared_view = manager.read(context, shared).unwrap();
        let mut committed = manager.begin(context);
        committed
            .expect(shared, shared_view.header().version)
            .update_state(shared, Value::Integer(3).encode().unwrap());
        manager.commit(committed).unwrap();
    }

    let recovered = InMemoryObjectManager::open_with_backend(backend).unwrap();
    assert_eq!(
        recovered.value(AccessContext::new(subject), shared),
        Ok(Value::Integer(3))
    );
    assert_eq!(
        recovered
            .read(AccessContext::new(subject), pool)
            .unwrap()
            .links()
            .get("member:state"),
        Some(&shared)
    );
}

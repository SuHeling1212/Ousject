#![allow(clippy::wildcard_imports)]

use super::*;

#[test]
fn retiring_an_object_atomically_cleans_names_links_and_children() {
    let source = r#"
root = object.create("core.namespace", {})
item = object.create("core.text", "old")
child = object.create("core.text", "child", item)
alias link item
root.link("item", item)
item.retire()
item = "fresh"
"#;
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_terminal(manager);
    let program = compile(source).unwrap();
    let retire_position = u32::try_from(
        program
            .tokens
            .iter()
            .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "retire"))
            .unwrap(),
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    while vm.process_state(process).unwrap().token_position != retire_position {
        assert_eq!(vm.run(process, 1).unwrap().status, ProcessStatus::Ready);
    }
    let before = vm.process_state(process).unwrap();
    let old_item = before.variables["item"];
    let old_child = before.variables["child"];
    let root = before.variables["root"];
    let context = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);

    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    assert_eq!(
        vm.manager().inspect(context, old_item).unwrap().lifecycle,
        LifecycleState::Active
    );
    assert_eq!(
        vm.manager().read(context, root).unwrap().links()["item"],
        old_item
    );

    assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
    let after = vm.process_state(process).unwrap();
    assert!(!after.variables.contains_key("alias"));
    assert!(!after.variables.contains_key("child"));
    assert_ne!(after.variables["item"], old_item);
    assert_eq!(
        vm.variable(process, "item"),
        Ok(Value::Text("fresh".to_owned()))
    );
    assert!(vm.manager().read(context, root).unwrap().links().is_empty());
    for retired in [old_item, old_child] {
        assert_eq!(
            vm.manager().inspect(context, retired).unwrap().lifecycle,
            LifecycleState::Tombstoned
        );
        assert!(matches!(
            vm.manager().value(context, retired),
            Err(OmsError::InvalidLifecycle { .. })
        ));
    }
}

#[test]
fn process_subject_is_persistent_and_enforced_for_object_access() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = VirtualMachine::new(manager.clone());
    let system = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);
    let secret = manager
        .create_object(
            system,
            CreateSpec::new("core.text", Value::Text("secret".to_owned())),
        )
        .unwrap();
    let subject = SubjectId::new();
    let program = compile(&format!(
        "secret = object.find(\"{secret}\")\ncopy = secret"
    ))
    .unwrap();
    let process = vm.create_process_as(&program, subject).unwrap();
    assert_eq!(vm.process_state(process).unwrap().subject, subject);

    assert!(matches!(
        vm.run(process, 100),
        Err(VmError::Oms(OmsError::Denied {
            capability: Capability::Inspect,
            ..
        }))
    ));

    let mut grant_inspect = manager.begin(system);
    grant_inspect
        .expect(secret, manager.inspect(system, secret).unwrap().version)
        .grant(secret, subject, Capability::Inspect);
    manager.commit(grant_inspect).unwrap();
    let process = vm.create_process_as(&program, subject).unwrap();
    assert!(matches!(
        vm.run(process, 100),
        Err(VmError::Oms(OmsError::Denied {
            capability: Capability::ViewValue,
            ..
        }))
    ));

    let mut grant_value = manager.begin(system);
    grant_value
        .expect(secret, manager.inspect(system, secret).unwrap().version)
        .grant(secret, subject, Capability::ViewValue);
    manager.commit(grant_value).unwrap();
    let process = vm.create_process_as(&program, subject).unwrap();
    assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
    assert_eq!(
        vm.variable(process, "copy"),
        Ok(Value::Text("secret".to_owned()))
    );
}

#[test]
fn namespace_program_and_channel_capabilities_run_end_to_end() {
    let source = r#"
func receiver() {
    inbox = object.find("inbox")
    inbox.wait()
    message = inbox.receive()
    out = object.find("out")
    out.send(message)
}

root = object.create("core.namespace", {})
item = object.create("core.text", "named")
root.bind("item", item.id)
found = object.find(root.resolve("item"))
root.unbind("item")

inbox = object.create("core.channel", [])
out = object.create("core.channel", [])
child = object.create("core.process", {
    entry: "receiver",
    links: { inbox: inbox.id, out: out.id }
})
child.start()
first_status = child.wait()
inbox.send("hello")
second_status = child.wait()
received = out.receive()

program = object.find("program")
executed_id = program.execute("receiver")
executed = object.find(executed_id)
executed.terminate()
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = VirtualMachine::new(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    assert_eq!(
        vm.run(process, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    assert_eq!(
        vm.variable(process, "found"),
        Ok(Value::Text("named".to_owned()))
    );
    assert_eq!(
        vm.variable(process, "first_status"),
        Ok(Value::Text("suspended".to_owned()))
    );
    assert_eq!(
        vm.variable(process, "second_status"),
        Ok(Value::Text("halted".to_owned()))
    );
    assert_eq!(
        vm.variable(process, "received"),
        Ok(Value::Text("hello".to_owned()))
    );
    let root = vm.process_state(process).unwrap().variables["root"];
    assert!(
        vm.manager()
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), root)
            .unwrap()
            .links()
            .is_empty()
    );
    let executed = vm.process_state(process).unwrap().variables["executed"];
    assert_eq!(
        vm.process_state(executed).unwrap().status,
        ProcessStatus::Terminated
    );
}

#[test]
fn shared_channel_atomically_wakes_a_waiter_owned_by_another_subject() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let vm = VirtualMachine::new(manager.clone());
    let system = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);
    let channel = manager
        .create_object(
            system,
            CreateSpec::new("core.channel", Value::Array(Vec::new())),
        )
        .unwrap();
    let receiver_subject = SubjectId::new();
    let sender_subject = SubjectId::new();
    let mut grants = manager.begin(system);
    let channel_version = manager.inspect(system, channel).unwrap().version;
    grants.expect(channel, channel_version);
    for subject in [receiver_subject, sender_subject] {
        for capability in [
            Capability::Inspect,
            Capability::Invoke,
            Capability::ViewValue,
            Capability::ReplaceValue,
            Capability::Link,
        ] {
            grants.grant(channel, subject, capability);
        }
    }
    manager.commit(grants).unwrap();

    let receiver = vm
        .create_process_as(
            &compile(&format!(
                "channel = object.find(\"{channel}\")\nchannel.wait()\nmessage = channel.receive()"
            ))
            .unwrap(),
            receiver_subject,
        )
        .unwrap();
    let sender = vm
        .create_process_as(
            &compile(&format!(
                "channel = object.find(\"{channel}\")\nchannel.send(\"cross-user\")"
            ))
            .unwrap(),
            sender_subject,
        )
        .unwrap();

    assert_eq!(
        vm.run(receiver, 100).unwrap().status,
        ProcessStatus::Waiting
    );
    assert_eq!(vm.run(sender, 100).unwrap().status, ProcessStatus::Halted);
    assert_eq!(
        vm.process_state(receiver).unwrap().status,
        ProcessStatus::Ready
    );
    assert_eq!(vm.run(receiver, 100).unwrap().status, ProcessStatus::Halted);
    assert_eq!(
        vm.variable(receiver, "message"),
        Ok(Value::Text("cross-user".to_owned()))
    );
}

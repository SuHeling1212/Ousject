#![allow(clippy::wildcard_imports)]

use super::*;

#[test]
fn praxis_object_creation_and_process_advance_are_atomic() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    let process = vm
        .create_process(&compile("item = object.create(\"core.text\", \"x\")").unwrap())
        .unwrap();
    assert_eq!(vm.run(process, 2).unwrap().status, ProcessStatus::Ready);
    let before = vm.process_state(process).unwrap();
    let count = vm.manager().stats().unwrap().object_count;
    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    assert_eq!(vm.manager().stats().unwrap().object_count, count);
    assert_eq!(vm.run(process, 10).unwrap().status, ProcessStatus::Halted);
    let item = vm.process_state(process).unwrap().variables["item"];
    assert_eq!(
        vm.manager()
            .value(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), item),
        Ok(Value::Text("x".to_owned()))
    );
}

#[test]
fn praxis_object_replacement_and_process_advance_are_atomic() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    let program =
        compile("item = object.create(\"core.text\", \"old\")\nitem.replace(\"new\")").unwrap();
    let replace_position = program
        .tokens
        .iter()
        .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "replace"))
        .unwrap();
    let process = vm.create_process(&program).unwrap();
    assert_eq!(
        vm.run(process, u64::try_from(replace_position).unwrap())
            .unwrap()
            .status,
        ProcessStatus::Ready
    );
    let item = vm.process_state(process).unwrap().variables["item"];
    let before = vm.process_state(process).unwrap();
    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    let reopened = InMemoryObjectManager::open_with_backend(backend).unwrap();
    assert_eq!(
        reopened.value(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), item),
        Ok(Value::Text("old".to_owned()))
    );
    assert_eq!(vm.run(process, 10).unwrap().status, ProcessStatus::Halted);
    assert_eq!(
        vm.manager()
            .value(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), item),
        Ok(Value::Text("new".to_owned()))
    );
}

#[test]
fn praxis_link_and_process_advance_are_atomic() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    let program = compile(
        "root = object.create(\"core.namespace\", {})\nitem = object.create(\"core.text\", \"x\")\nroot.link(\"item\", item)",
    )
    .unwrap();
    let link_position = program
        .tokens
        .iter()
        .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "link"))
        .unwrap();
    let process = vm.create_process(&program).unwrap();
    vm.run(process, u64::try_from(link_position).unwrap())
        .unwrap();
    let root = vm.process_state(process).unwrap().variables["root"];
    let before = vm.process_state(process).unwrap();
    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    let reopened = InMemoryObjectManager::open_with_backend(backend).unwrap();
    assert!(
        reopened
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), root)
            .unwrap()
            .links()
            .is_empty()
    );
    assert_eq!(vm.run(process, 10).unwrap().status, ProcessStatus::Halted);
    assert!(
        vm.manager()
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), root)
            .unwrap()
            .links()
            .contains_key("item")
    );
}

#[test]
fn praxis_transaction_commits_multiple_objects_atomically() {
    let source = r#"
class Account { balance = 0 }
source = object.create("Account", { balance: 20 })
target = object.create("Account", { balance: 2 })
transaction {
    source.balance = source.balance - 10
    target.balance = target.balance + 10
}
"#;
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    let program = compile(source).unwrap();
    let transaction_position = u32::try_from(
        program
            .tokens
            .iter()
            .position(|token| matches!(token, tf_format::Token::Transaction { .. }))
            .unwrap(),
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    while vm.process_state(process).unwrap().token_position != transaction_position {
        assert_eq!(vm.run(process, 1).unwrap().status, ProcessStatus::Ready);
    }
    let bindings = vm.process_state(process).unwrap().variables;
    let source = bindings["source"];
    let target = bindings["target"];
    let context = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);
    let before_process = vm.process_state(process).unwrap();

    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before_process));
    for (object, expected) in [(source, 20), (target, 2)] {
        let Value::Record(fields) = vm.manager().value(context, object).unwrap() else {
            panic!("expected Account state")
        };
        assert_eq!(fields["balance"], Value::Integer(expected));
    }

    assert_eq!(vm.run(process, 50).unwrap().status, ProcessStatus::Halted);
    for (object, expected) in [(source, 10), (target, 12)] {
        let Value::Record(fields) = vm.manager().value(context, object).unwrap() else {
            panic!("expected Account state")
        };
        assert_eq!(fields["balance"], Value::Integer(expected));
    }
}

#[test]
fn praxis_can_create_run_and_link_process_objects() {
    let source = r#"
class Channel { value = 0 }

func worker() {
    channel = object.find("channel")
    channel.value++
}

console = object.find("console")
self = object.find("process")
program = object.find("program")
channel = object.create("Channel", {})
child = object.create("core.process", {
    entry: "worker",
    links: { channel: channel.id }
})
console.println(self.type)
console.println(program.type)
console.println(child.status)
child.start()
console.println(child.wait())
console.println(channel.value)
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 1_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(
        report.output,
        vec!["core.process", "core.program", "suspended", "halted", "1"]
    );
    let child = vm.process_state(process).unwrap().variables["child"];
    assert_eq!(
        vm.process_state(child).unwrap().status,
        ProcessStatus::Halted
    );
}

#[test]
fn collection_uses_variable_value_capabilities() {
    let source = r#"
console = object.find("console")
items = object.create("core.collection", [1, 2])
items[1] = 9
console.println(items[1])
items.replace([7, 9, 11])
console.println(items[2])
console.println(#items)
console.println(items.value)

mapping = object.create("core.collection", {"a": 1})
mapping["b"] = 2
console.println(mapping["a"])
console.println(mapping["b"])
console.println(#mapping)
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 500).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(
        report.output,
        vec![
            "9",
            "11",
            "3",
            "[Integer(7), Integer(9), Integer(11)]",
            "1",
            "2",
            "2"
        ]
    );
}

#[test]
fn praxis_object_api_can_grant_permissions() {
    let subject = oms_types::SubjectId::from_u128(0xabc);
    let source = format!(
        "item = object.create(\"core.text\", \"shared\")\nitem.grant(\"{subject}\", \"view_value\")\n"
    );
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(&source).unwrap()).unwrap();
    assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
    let item = vm.process_state(process).unwrap().variables["item"];
    assert_eq!(
        vm.manager().value(AccessContext::new(subject), item),
        Ok(Value::Text("shared".to_owned()))
    );
}

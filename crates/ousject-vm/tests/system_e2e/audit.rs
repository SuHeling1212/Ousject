#![allow(clippy::wildcard_imports)]

use super::*;

#[test]
fn permission_events_are_append_only_and_exclude_object_values() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(Arc::clone(&manager));
    let recipient = SubjectId::new();
    let program = compile(&format!(
        "secret = object.create(\"core.text\", \"secret:private-payload\")\nsecret.grant(\"{recipient}\", \"inspect\")"
    ))
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    assert_eq!(
        vm.run(process, 1_000).unwrap().status,
        ProcessStatus::Halted
    );

    let system = AccessContext::new(SYSTEM_SUBJECT);
    let roots = manager
        .query(
            system,
            &ObjectQuery::new().with_type(oms_types::CORE_AUDIT_TYPE),
        )
        .unwrap();
    assert_eq!(roots.len(), 1);
    let events = manager
        .query(
            system,
            &ObjectQuery::new().with_type(oms_types::CORE_AUDIT_EVENT_TYPE),
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    let event_view = manager.read(system, events[0].id).unwrap();
    let event = Value::decode(event_view.state()).unwrap();
    assert!(matches!(
        event,
        Value::Record(ref fields)
            if fields.get("action") == Some(&Value::Text("permission.grant".to_owned()))
    ));
    assert!(!String::from_utf8_lossy(event_view.state()).contains("private-payload"));

    let root_view = manager.read(system, roots[0].id).unwrap();
    let mut edit_root = manager.begin(system);
    edit_root
        .expect(roots[0].id, root_view.header().version)
        .update_state(roots[0].id, Value::Null.encode().unwrap());
    assert!(manager.commit(edit_root).is_err());

    let mut retire_event = manager.begin(system);
    retire_event
        .expect(events[0].id, event_view.header().version)
        .tombstone(events[0].id);
    assert!(manager.commit(retire_event).is_err());
    assert_eq!(
        manager
            .query(
                system,
                &ObjectQuery::new().with_type(oms_types::CORE_AUDIT_EVENT_TYPE),
            )
            .unwrap()
            .len(),
        1
    );
}

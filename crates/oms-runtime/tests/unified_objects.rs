use oms_runtime::{
    AccessContext, CreateObject, CreateSpec, CreationPolicy, InMemoryObjectManager, ObjectQuery,
    ValueSchema,
};
use oms_types::{
    CORE_PROCESS_TYPE, CORE_TEXT_TYPE, DEVICE_DISPLAY_TYPE, ObjectId, OmsError, SYSTEM_SUBJECT,
    SubjectId, TYPE_DESCRIPTOR_TYPE, Value,
};
use std::collections::{BTreeMap, BTreeSet};

fn setup() -> (InMemoryObjectManager, AccessContext) {
    (
        InMemoryObjectManager::new(1).unwrap(),
        AccessContext::new(SubjectId::new()),
    )
}

#[test]
fn dynamic_type_descriptor_persists_and_controls_creation() {
    let directory = std::env::temp_dir().join(format!("ousject-type-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let system = AccessContext::new(SYSTEM_SUBJECT);
    let descriptor = {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        assert!(
            manager
                .register_type(
                    AccessContext::new(SubjectId::new()),
                    "app.note",
                    ValueSchema::Text,
                    CreationPolicy::Public,
                    BTreeSet::from(["publish".to_owned()]),
                )
                .is_err()
        );
        let descriptor = manager
            .register_type(
                system,
                "app.note",
                ValueSchema::Text,
                CreationPolicy::Public,
                BTreeSet::from(["publish".to_owned()]),
            )
            .unwrap();
        let note = manager
            .create_object(
                system,
                CreateSpec::new("app.note", Value::Text("durable".to_owned())),
            )
            .unwrap();
        assert_eq!(
            manager.inspect(system, note).unwrap().type_id,
            descriptor.id
        );
        descriptor
    };

    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(recovered.type_by_name("app.note").unwrap(), descriptor);
    assert!(
        recovered
            .types()
            .unwrap()
            .iter()
            .any(|item| item.name == "app.note")
    );
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn malformed_dynamic_type_update_is_rejected_atomically() {
    let (manager, _) = setup();
    let system = AccessContext::new(SYSTEM_SUBJECT);
    manager
        .register_type(
            system,
            "app.safe",
            ValueSchema::Text,
            CreationPolicy::Public,
            BTreeSet::new(),
        )
        .unwrap();
    let header = manager
        .query(system, &ObjectQuery::new().with_type(TYPE_DESCRIPTOR_TYPE))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let before = manager.read(system, header.id).unwrap();
    let mut transaction = manager.begin(system);
    transaction
        .expect(header.id, header.version)
        .update_state(header.id, b"not a Value".to_vec());

    assert!(manager.commit(transaction).is_err());
    let after = manager.read(system, header.id).unwrap();
    assert_eq!(after.header(), before.header());
    assert_eq!(after.state(), before.state());
    manager.health_check().unwrap();
}

#[test]
fn creates_selected_type_and_replaces_validated_value() {
    let (manager, context) = setup();
    let object = manager
        .create_object(
            context,
            CreateSpec::new("core.text", Value::Text("one".into())),
        )
        .unwrap();

    assert_eq!(
        manager.find(context, object).unwrap().header().type_id,
        CORE_TEXT_TYPE
    );
    assert_eq!(
        manager.value(context, object),
        Ok(Value::Text("one".into()))
    );
    let version = manager
        .replace_value(context, object, &Value::Text("two".into()))
        .unwrap();
    assert_eq!(version.get(), 1);
    assert_eq!(
        manager.value(context, object),
        Ok(Value::Text("two".into()))
    );
    assert!(matches!(
        manager.replace_value(context, object, &Value::Integer(2)),
        Err(OmsError::ValueSchemaMismatch { .. })
    ));
}

#[test]
fn provider_only_device_cannot_be_forged() {
    let (manager, context) = setup();
    let result = manager.create_object(
        context,
        CreateSpec::new("device.display", Value::Record(BTreeMap::new())),
    );
    assert_eq!(
        result,
        Err(OmsError::TypeCreationDenied(DEVICE_DISPLAY_TYPE))
    );
}

#[test]
fn typed_replacement_cannot_corrupt_provider_owned_process() {
    let (manager, context) = setup();
    let request = CreateObject::new(CORE_PROCESS_TYPE, b"internal process state");
    let process = request.id;
    let mut transaction = manager.begin(context);
    transaction.create(request);
    manager.commit(transaction).unwrap();
    assert_eq!(
        manager.replace_value(context, process, &Value::Record(BTreeMap::new())),
        Err(OmsError::TypeCreationDenied(CORE_PROCESS_TYPE))
    );
    assert_eq!(
        manager.read(context, process).unwrap().state(),
        b"internal process state"
    );
}

#[test]
fn query_uses_type_and_domain_capability() {
    let (manager, context) = setup();
    let text = manager
        .create_object(
            context,
            CreateSpec::new("core.text", Value::Text("note".into())),
        )
        .unwrap();
    let process_value = Value::Record(BTreeMap::new()).encode().unwrap();
    let request = CreateObject::new(CORE_PROCESS_TYPE, process_value).with_link("note", text);
    let process = request.id;
    let mut publish = manager.begin(context);
    publish.create(request);
    manager.commit(publish).unwrap();

    let matches = manager
        .query(
            context,
            &ObjectQuery::new()
                .with_type(CORE_PROCESS_TYPE)
                .with_domain_capability("start"),
        )
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, process);
    assert_eq!(
        manager.find(context, process).unwrap().links()["note"],
        text
    );
    manager.health_check().unwrap();
}

#[test]
fn typed_values_and_type_index_recover_from_storage() {
    let directory = std::env::temp_dir().join(format!("ousject-unified-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let context = AccessContext::new(SubjectId::new());
    let object = {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        manager
            .create_object(
                context,
                CreateSpec::new("core.text", Value::Text("durable".into())),
            )
            .unwrap()
    };

    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(
        recovered.value(context, object),
        Ok(Value::Text("durable".into()))
    );
    let matches = recovered
        .query(context, &ObjectQuery::new().with_type(CORE_TEXT_TYPE))
        .unwrap();
    assert_eq!(
        matches.iter().map(|header| header.id).collect::<Vec<_>>(),
        vec![object]
    );
    recovered.health_check().unwrap();
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn namespace_paths_resolve_any_object_without_files() {
    let (manager, context) = setup();
    let empty = || Value::Record(BTreeMap::new());
    let root = manager
        .create_object(context, CreateSpec::new("core.namespace", empty()))
        .unwrap();
    let docs = manager
        .create_object(context, CreateSpec::new("core.namespace", empty()))
        .unwrap();
    let readme = manager
        .create_object(
            context,
            CreateSpec::new("core.text", Value::Text("# Ousject".into())),
        )
        .unwrap();

    manager.bind_name(context, root, "docs", docs).unwrap();
    manager.bind_name(context, docs, "README", readme).unwrap();
    assert_eq!(manager.resolve(context, root, "/docs/README"), Ok(readme));
    assert!(matches!(
        manager.bind_name(context, root, "docs", readme),
        Err(OmsError::InvalidOperation("namespace name already exists"))
    ));
    manager.unbind_name(context, docs, "README").unwrap();
    assert!(matches!(
        manager.resolve(context, root, "docs/README"),
        Err(OmsError::NameNotFound { .. })
    ));
}

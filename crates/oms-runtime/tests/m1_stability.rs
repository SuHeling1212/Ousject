use oms_runtime::{
    AccessContext, CreateObject, InMemoryObjectManager, ObjectManager, SnapshotBackend,
};
use oms_types::{Capability, LifecycleState, ObjectId, OmsError, SubjectId, TypeId};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

fn setup() -> (Arc<InMemoryObjectManager>, AccessContext) {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let context = AccessContext::new(SubjectId::new());
    (manager, context)
}

fn create(
    manager: &InMemoryObjectManager,
    context: AccessContext,
    state: &[u8],
    parent: Option<ObjectId>,
) -> ObjectId {
    let mut request = CreateObject::new(TypeId::new(), state);
    let mut transaction = manager.begin(context);
    if let Some(parent) = parent {
        request = request.with_parent(parent);
        let version = manager.inspect(context, parent).unwrap().version;
        transaction.expect(parent, version);
    }
    let id = request.id;
    transaction.create(request);
    manager.commit(transaction).unwrap();
    id
}

#[test]
fn concurrent_writers_publish_exactly_one_version() {
    let (manager, context) = setup();
    let object = create(&manager, context, b"initial", None);
    let version = manager.read(context, object).unwrap().header().version;
    let barrier = Arc::new(Barrier::new(8));

    let handles = (0..8)
        .map(|writer| {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut transaction = manager.begin(context);
                transaction
                    .expect(object, version)
                    .update_state(object, format!("writer-{writer}"));
                barrier.wait();
                manager.commit(transaction)
            })
        })
        .collect::<Vec<_>>();

    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(OmsError::Conflict { .. })))
            .count(),
        7
    );
    assert_eq!(
        manager.read(context, object).unwrap().header().version,
        version.next()
    );
    manager.health_check().unwrap();
}

#[test]
fn late_failure_discards_every_candidate_change() {
    let (manager, context) = setup();
    let object = create(&manager, context, b"before", None);
    let before = manager.read(context, object).unwrap();
    let missing = ObjectId::new();

    let mut transaction = manager.begin(context);
    transaction
        .expect(object, before.header().version)
        .update_state(object, b"candidate")
        .set_link(object, "invalid", missing);

    assert_eq!(
        manager.commit(transaction),
        Err(OmsError::NotFound(missing))
    );
    let after = manager.read(context, object).unwrap();
    assert_eq!(after.state(), b"before");
    assert_eq!(after.header().version, before.header().version);
    assert!(after.links().is_empty());
}

#[test]
fn parent_cycle_is_rejected_without_partial_indexes() {
    let (manager, context) = setup();
    let parent = create(&manager, context, b"parent", None);
    let child = create(&manager, context, b"child", Some(parent));
    let parent_before = manager.read(context, parent).unwrap();
    let child_before = manager.read(context, child).unwrap();

    let mut transaction = manager.begin(context);
    transaction
        .expect(parent, parent_before.header().version)
        .expect(child, child_before.header().version)
        .reparent(parent, Some(child));
    assert_eq!(manager.commit(transaction), Err(OmsError::ParentCycle));

    let parent_after = manager.read(context, parent).unwrap();
    let child_after = manager.read(context, child).unwrap();
    assert_eq!(parent_after.header().parent_id, None);
    assert_eq!(child_after.header().parent_id, Some(parent));
    assert!(parent_after.children().contains(&child));
    assert!(child_after.children().is_empty());
    manager.health_check().unwrap();
}

#[test]
fn cross_shard_transaction_commits_atomically() {
    let manager = InMemoryObjectManager::new(2).unwrap();
    let context = AccessContext::new(SubjectId::new());
    let first = ObjectId::from_u128(1);
    let second = ObjectId::from_u128(2);
    assert_ne!(manager.shard_for(first), manager.shard_for(second));

    for id in [first, second] {
        let mut transaction = manager.begin(context);
        transaction.create(CreateObject::new(TypeId::new(), b"state").with_id(id));
        manager.commit(transaction).unwrap();
    }

    let version = manager.read(context, first).unwrap().header().version;
    let mut transaction = manager.begin(context);
    transaction
        .expect(first, version)
        .set_link(first, "cross", second);
    manager.commit(transaction).unwrap();
    assert_eq!(
        manager.read(context, first).unwrap().links()["cross"],
        second
    );
    manager.health_check().unwrap();
}

#[test]
fn cross_shard_parent_relationship_is_globally_consistent() {
    let manager = InMemoryObjectManager::new(2).unwrap();
    let context = AccessContext::new(SubjectId::new());
    let parent = ObjectId::from_u128(1);
    let child = ObjectId::from_u128(2);
    assert_ne!(manager.shard_for(parent), manager.shard_for(child));

    let mut create_parent = manager.begin(context);
    create_parent.create(CreateObject::new(TypeId::new(), b"parent").with_id(parent));
    manager.commit(create_parent).unwrap();

    let version = manager.inspect(context, parent).unwrap().version;
    let mut create_child = manager.begin(context);
    create_child.expect(parent, version).create(
        CreateObject::new(TypeId::new(), b"child")
            .with_id(child)
            .with_parent(parent),
    );
    manager.commit(create_child).unwrap();

    assert!(
        manager
            .read(context, parent)
            .unwrap()
            .children()
            .contains(&child)
    );
    assert_eq!(
        manager.read(context, child).unwrap().header().parent_id,
        Some(parent)
    );
    manager.health_check().unwrap();
}

#[test]
fn revoked_capability_stops_future_reads() {
    let (manager, owner_context) = setup();
    let reader = SubjectId::new();
    let reader_context = AccessContext::new(reader);
    let object = create(&manager, owner_context, b"state", None);

    let version = manager
        .read(owner_context, object)
        .unwrap()
        .header()
        .version;
    let mut grant = manager.begin(owner_context);
    grant
        .expect(object, version)
        .grant(object, reader, Capability::ViewValue);
    manager.commit(grant).unwrap();
    assert!(manager.read(reader_context, object).is_ok());

    let version = manager
        .read(owner_context, object)
        .unwrap()
        .header()
        .version;
    let mut revoke = manager.begin(owner_context);
    revoke
        .expect(object, version)
        .revoke(object, reader, Capability::ViewValue);
    manager.commit(revoke).unwrap();
    assert!(matches!(
        manager.read(reader_context, object),
        Err(OmsError::Denied {
            capability: Capability::ViewValue,
            ..
        })
    ));
}

#[test]
fn tombstone_remains_inspectable_and_counted() {
    let (manager, context) = setup();
    let object = create(&manager, context, b"state", None);
    let version = manager.inspect(context, object).unwrap().version;
    let mut transaction = manager.begin(context);
    transaction.expect(object, version).tombstone(object);
    manager.commit(transaction).unwrap();

    assert!(matches!(
        manager.read(context, object),
        Err(OmsError::InvalidLifecycle {
            state: LifecycleState::Tombstoned,
            ..
        })
    ));
    assert_eq!(
        manager.inspect(context, object).unwrap().lifecycle,
        LifecycleState::Tombstoned
    );
    let stats = manager.stats().unwrap();
    assert_eq!(stats.object_count, 1);
    assert_eq!(stats.active_count, 0);
    assert_eq!(stats.tombstoned_count, 1);
    assert_eq!(ObjectManager::list(&*manager, context).unwrap().len(), 1);
}

#[test]
fn persistent_store_recovers_committed_state() {
    let directory = std::env::temp_dir().join(format!("ousject-test-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let owner = SubjectId::new();
    let context = AccessContext::new(owner);

    let object = {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        let object = create(&manager, context, b"before", None);
        let version = manager.read(context, object).unwrap().header().version;
        let mut update = manager.begin(context);
        update
            .expect(object, version)
            .update_state(object, b"durable");
        manager.commit(update).unwrap();
        object
    };

    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(recovered.read(context, object).unwrap().state(), b"durable");
    recovered.health_check().unwrap();
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn persistent_store_rejects_corruption() {
    let directory = std::env::temp_dir().join(format!("ousject-test-{}", ObjectId::new()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("objects.oms");
    std::fs::write(&path, b"not an OMS snapshot").unwrap();
    assert!(matches!(
        InMemoryObjectManager::open_persistent(&path),
        Err(OmsError::Corruption(_))
    ));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn persistent_store_is_exclusive_until_owner_drops() {
    let directory = std::env::temp_dir().join(format!("ousject-lock-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let first = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert!(matches!(
        InMemoryObjectManager::open_persistent(&path),
        Err(OmsError::StoreInUse(_))
    ));
    drop(first);
    let reopened = InMemoryObjectManager::open_persistent(&path).unwrap();
    drop(reopened);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn persistent_store_reclaims_a_lock_from_a_dead_process() {
    let directory = std::env::temp_dir().join(format!("ousject-stale-lock-{}", ObjectId::new()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("objects.oms");
    std::fs::write(path.with_extension("lock"), "4294967295 abandoned\n").unwrap();

    let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
    drop(manager);
    assert!(!path.with_extension("lock").exists());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn synced_wal_recovers_when_checkpoint_cannot_replace_snapshot() {
    let directory = std::env::temp_dir().join(format!("ousject-wal-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let owner = SubjectId::new();
    let context = AccessContext::new(owner);

    let object = {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let object = create(&manager, context, b"wal-only", None);
        assert_eq!(manager.read(context, object).unwrap().state(), b"wal-only");
        object
    };

    std::fs::remove_dir(&path).unwrap();
    let wal = path.with_extension("wal");
    let mut bytes = std::fs::read(&wal).unwrap();
    bytes.extend_from_slice(b"OMW0\x20");
    std::fs::write(&wal, bytes).unwrap();

    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(
        recovered.read(context, object).unwrap().state(),
        b"wal-only"
    );
    recovered.health_check().unwrap();
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn checkpoint_batches_commits_without_weakening_wal_recovery() {
    let directory = std::env::temp_dir().join(format!("ousject-checkpoint-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let owner = SubjectId::new();
    let context = AccessContext::new(owner);
    let object;
    {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        object = create(&manager, context, b"0", None);
        for value in 1_u8..64 {
            let view = manager.read(context, object).unwrap();
            let mut transaction = manager.begin(context);
            transaction
                .expect(object, view.header().version)
                .update_state(object, [value]);
            manager.commit(transaction).unwrap();
        }
        assert!(path.exists());
        assert_eq!(
            std::fs::metadata(path.with_extension("wal")).unwrap().len(),
            0
        );

        let view = manager.read(context, object).unwrap();
        let mut transaction = manager.begin(context);
        transaction
            .expect(object, view.header().version)
            .update_state(object, [64]);
        manager.commit(transaction).unwrap();
        assert!(
            std::fs::metadata(path.with_extension("wal")).unwrap().len()
                < std::fs::metadata(&path).unwrap().len()
        );
    }

    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(recovered.read(context, object).unwrap().state(), [64]);
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn compressed_incremental_wal_rejects_bit_corruption() {
    let directory = std::env::temp_dir().join(format!("ousject-wal-corrupt-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let context = AccessContext::new(SubjectId::new());
    {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        let object = create(&manager, context, &vec![0_u8; 4096], None);
        for value in 1_u8..64 {
            let view = manager.read(context, object).unwrap();
            let mut transaction = manager.begin(context);
            transaction
                .expect(object, view.header().version)
                .update_state(object, vec![value; 4096]);
            manager.commit(transaction).unwrap();
        }
        let view = manager.read(context, object).unwrap();
        let mut transaction = manager.begin(context);
        transaction
            .expect(object, view.header().version)
            .update_state(object, vec![255_u8; 4096]);
        manager.commit(transaction).unwrap();
    }
    let wal = path.with_extension("wal");
    let mut bytes = std::fs::read(&wal).unwrap();
    assert!(bytes.len() > 24);
    bytes[16] ^= 0x40;
    std::fs::write(&wal, bytes).unwrap();
    assert!(matches!(
        InMemoryObjectManager::open_persistent(&path),
        Err(OmsError::Corruption(_))
    ));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn persistent_multi_shard_state_recovers_with_stable_routing() {
    let directory = std::env::temp_dir().join(format!("ousject-shards-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let context = AccessContext::new(SubjectId::new());
    let first = ObjectId::from_u128(1);
    let second = ObjectId::from_u128(2);

    {
        let manager = InMemoryObjectManager::open_persistent_with_shards(&path, 2).unwrap();
        assert_ne!(manager.shard_for(first), manager.shard_for(second));
        let mut transaction = manager.begin(context);
        transaction
            .create(CreateObject::new(TypeId::new(), b"first").with_id(first))
            .create(CreateObject::new(TypeId::new(), b"second").with_id(second));
        manager.commit(transaction).unwrap();
    }

    let recovered = InMemoryObjectManager::open_persistent_with_shards(&path, 2).unwrap();
    assert_eq!(recovered.read(context, first).unwrap().state(), b"first");
    assert_eq!(recovered.read(context, second).unwrap().state(), b"second");
    recovered.health_check().unwrap();
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[derive(Debug, Default)]
struct FaultBackend {
    snapshot: Mutex<Option<Vec<u8>>>,
    fail_store: AtomicBool,
    stores: AtomicUsize,
}

impl SnapshotBackend for FaultBackend {
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError> {
        Ok(self.snapshot.lock().unwrap().clone())
    }

    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        self.stores.fetch_add(1, Ordering::SeqCst);
        if self.fail_store.load(Ordering::SeqCst) {
            return Err(OmsError::Storage("injected failure".to_owned()));
        }
        *self.snapshot.lock().unwrap() = Some(snapshot.to_vec());
        Ok(())
    }
}

#[test]
fn group_commit_uses_one_durable_write_and_publishes_every_transaction_together() {
    let backend = Arc::new(FaultBackend::default());
    let manager = InMemoryObjectManager::open_with_backend(backend.clone()).unwrap();
    let context = AccessContext::new(SubjectId::new());
    let first = create(&manager, context, b"first", None);
    let second = create(&manager, context, b"second", None);
    backend.stores.store(0, Ordering::SeqCst);

    let mut one = manager.begin(context);
    one.expect(first, manager.inspect(context, first).unwrap().version)
        .update_state(first, b"one");
    let mut two = manager.begin(context);
    two.expect(second, manager.inspect(context, second).unwrap().version)
        .update_state(second, b"two");

    let results = manager.commit_batch(vec![one, two]).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(backend.stores.load(Ordering::SeqCst), 1);
    assert_eq!(manager.read(context, first).unwrap().state(), b"one");
    assert_eq!(manager.read(context, second).unwrap().state(), b"two");

    backend.fail_store.store(true, Ordering::SeqCst);
    let mut one = manager.begin(context);
    one.expect(first, manager.inspect(context, first).unwrap().version)
        .update_state(first, b"not-one");
    let mut two = manager.begin(context);
    two.expect(second, manager.inspect(context, second).unwrap().version)
        .update_state(second, b"not-two");
    assert!(matches!(
        manager.commit_batch(vec![one, two]),
        Err(OmsError::Storage(_))
    ));
    assert_eq!(manager.read(context, first).unwrap().state(), b"one");
    assert_eq!(manager.read(context, second).unwrap().state(), b"two");
}

#[test]
fn failed_durable_write_never_becomes_visible() {
    let backend = Arc::new(FaultBackend::default());
    let manager = InMemoryObjectManager::open_with_backend(backend.clone()).unwrap();
    let context = AccessContext::new(SubjectId::new());
    let object = create(&manager, context, b"committed", None);
    let before = manager.read(context, object).unwrap();

    backend.fail_store.store(true, Ordering::SeqCst);
    let mut transaction = manager.begin(context);
    transaction
        .expect(object, before.header().version)
        .update_state(object, b"must-not-publish");
    assert!(matches!(
        manager.commit(transaction),
        Err(OmsError::Storage(_))
    ));

    let after = manager.read(context, object).unwrap();
    assert_eq!(after.state(), b"committed");
    assert_eq!(after.header().version, before.header().version);
}

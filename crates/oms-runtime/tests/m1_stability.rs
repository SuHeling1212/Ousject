use oms_runtime::{
    AccessContext, CreateObject, InMemoryObjectManager, ObjectManager, SnapshotBackend,
};
use oms_types::{
    CORE_EFFECT_TYPE, Capability, LifecycleState, ObjectId, OmsError, SubjectId, TypeId, Value,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

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
fn gc_dry_run_is_read_only_and_compaction_preserves_identity_and_live_objects() {
    let directory = std::env::temp_dir().join(format!("ousject-gc-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let owner = SubjectId::new();
    let context = AccessContext::new(owner);
    let dead;
    let active;
    let tombstone_header;
    let active_before;
    {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        let mut seed = 0x9e37_79b9_u32;
        let payload = (0..64 * 1024)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed.to_le_bytes()[0]
            })
            .collect::<Vec<_>>();
        dead = create(&manager, context, &payload, None);
        active = create(&manager, context, b"must remain unchanged", None);

        let version = manager.inspect(context, dead).unwrap().version;
        let mut link = manager.begin(context);
        link.expect(dead, version).set_link(dead, "anchor", active);
        manager.commit(link).unwrap();
        let version = manager.inspect(context, dead).unwrap().version;
        let mut retire = manager.begin(context);
        retire.expect(dead, version).tombstone(dead);
        manager.commit(retire).unwrap();

        tombstone_header = manager.inspect(context, dead).unwrap();
        active_before = manager.read(context, active).unwrap();
        let expired_time = SystemTime::now() + Duration::from_secs(8 * 24 * 60 * 60);
        let before_dry_run = manager.analyze_gc_at(expired_time).unwrap();
        let after_dry_run = manager.analyze_gc_at(expired_time).unwrap();
        assert_eq!(before_dry_run, after_dry_run);
        assert_eq!(before_dry_run.tombstones_waiting_for_retention, 0);
        assert!(before_dry_run.payload_bytes_reclaimable >= 64 * 1024);
        let still_retained = manager
            .analyze_gc_at(SystemTime::now() + Duration::from_secs(6 * 24 * 60 * 60))
            .unwrap();
        assert_eq!(still_retained.objects_compactable, 0);
        assert_eq!(still_retained.tombstones_waiting_for_retention, 1);
        assert!(matches!(
            manager.read(context, dead),
            Err(OmsError::InvalidLifecycle {
                state: LifecycleState::Tombstoned,
                ..
            })
        ));

        let report = manager.compact_expired_tombstones_at(expired_time).unwrap();
        assert_eq!(report.objects_compacted, 1);
        assert!(report.payload_bytes_reclaimed >= 64 * 1024);
        assert_eq!(manager.inspect(context, dead).unwrap(), tombstone_header);
        let active_after = manager.read(context, active).unwrap();
        assert_eq!(active_after.state(), active_before.state());
        assert_eq!(active_after.header(), active_before.header());
        manager.health_check().unwrap();
    }

    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(recovered.inspect(context, dead).unwrap(), tombstone_header);
    assert!(matches!(
        recovered.read(context, dead),
        Err(OmsError::InvalidLifecycle {
            state: LifecycleState::Tombstoned,
            ..
        })
    ));
    assert_eq!(
        recovered.read(context, active).unwrap().state(),
        b"must remain unchanged"
    );
    recovered.health_check().unwrap();

    let mut duplicate = recovered.begin(context);
    duplicate.create(CreateObject::new(TypeId::new(), b"reuse").with_id(dead));
    assert!(matches!(
        recovered.commit(duplicate),
        Err(OmsError::InvalidOperation("ObjectId already exists"))
    ));
    let fresh = create(&recovered, context, b"fresh", None);
    assert_ne!(fresh, dead);
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn persistent_tombstone_retention_survives_restart() {
    let directory = std::env::temp_dir().join(format!("ousject-retention-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let context = AccessContext::new(SubjectId::new());
    let object;
    let tombstone;
    {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        object = create(&manager, context, b"keep this for seven days", None);
        let version = manager.inspect(context, object).unwrap().version;
        let mut retire = manager.begin(context);
        retire.expect(object, version).tombstone(object);
        manager.commit(retire).unwrap();
        tombstone = manager.inspect(context, object).unwrap();
        let report = manager.analyze_gc().unwrap();
        assert_eq!(report.tombstones_waiting_for_retention, 1);
        assert_eq!(report.objects_compactable, 0);
    }

    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(recovered.inspect(context, object).unwrap(), tombstone);
    let report = recovered.analyze_gc().unwrap();
    assert_eq!(report.tombstones_waiting_for_retention, 1);
    assert_eq!(report.objects_compactable, 0);
    assert_eq!(recovered.compact().unwrap().objects_compacted, 0);
    recovered.health_check().unwrap();
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unresolved_effect_cannot_be_tombstoned_or_compacted() {
    let (manager, context) = setup();
    let effect = CreateObject::new(
        CORE_EFFECT_TYPE,
        Value::Record(std::collections::BTreeMap::from([(
            "status".to_owned(),
            Value::Text("outcome_unknown".to_owned()),
        )]))
        .encode()
        .unwrap(),
    );
    let effect_id = effect.id;
    let mut create_effect = manager.begin(context);
    create_effect.create(effect);
    manager.commit(create_effect).unwrap();

    let version = manager.inspect(context, effect_id).unwrap().version;
    let mut retire = manager.begin(context);
    retire.expect(effect_id, version).tombstone(effect_id);
    assert_eq!(
        manager.commit(retire),
        Err(OmsError::InvalidOperation(
            "an unresolved Effect cannot be tombstoned"
        ))
    );
    assert!(manager.read(context, effect_id).is_ok());
    assert_eq!(manager.compact().unwrap().objects_compacted, 0);
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
        for _ in 0..16 {
            create(&manager, context, b"another object", None);
        }
        for value in 1_u8..64 {
            let view = manager.read(context, object).unwrap();
            let mut transaction = manager.begin(context);
            transaction
                .expect(object, view.header().version)
                .update_state(object, [value]);
            manager.commit(transaction).unwrap();
        }
        let manifest = path.with_extension("manifest");
        let checkpoint = path.with_extension("oms.g1");
        let wal = path.with_extension("wal.g1");
        assert!(!manifest.exists(), "small commits must not force snapshots");
        manager.checkpoint().unwrap();
        assert!(manifest.exists());
        assert!(checkpoint.exists());
        assert_eq!(
            std::fs::metadata(&wal).map_or(0, |metadata| metadata.len()),
            0
        );

        let view = manager.read(context, object).unwrap();
        let mut transaction = manager.begin(context);
        transaction
            .expect(object, view.header().version)
            .update_state(object, [64]);
        manager.commit(transaction).unwrap();
        assert!(
            std::fs::metadata(&wal).unwrap().len() < std::fs::metadata(&checkpoint).unwrap().len()
        );
    }

    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(recovered.read(context, object).unwrap().state(), [64]);
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn background_checkpoint_triggers_at_quarter_of_existing_snapshot() {
    let directory =
        std::env::temp_dir().join(format!("ousject-checkpoint-ratio-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let context = AccessContext::new(SubjectId::new());
    let initial_state = (0..1_024)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect::<Vec<_>>();
    let next_state = (0..2_048)
        .map(|index| u8::try_from((index * 17) % 251).unwrap())
        .collect::<Vec<_>>();
    let object;

    {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        object = create(&manager, context, &initial_state, None);
        manager.checkpoint().unwrap();

        let view = manager.read(context, object).unwrap();
        let mut transaction = manager.begin(context);
        transaction
            .expect(object, view.header().version)
            .update_state(object, next_state.clone());
        manager.commit(transaction).unwrap();

        let manifest = path.with_extension("manifest");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let switched = loop {
            let generation = std::fs::read(&manifest)
                .ok()
                .filter(|bytes| bytes.len() == 20)
                .map(|bytes| u64::from_le_bytes(bytes[4..12].try_into().unwrap()));
            if generation.is_some_and(|generation| generation >= 2) {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(5));
        };
        assert!(
            switched,
            "WAL above one quarter of the checkpoint did not trigger background compaction"
        );
        assert_eq!(manager.read(context, object).unwrap().state(), next_state);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let recovered = loop {
        match InMemoryObjectManager::open_persistent(&path) {
            Ok(manager) => break manager,
            Err(OmsError::StoreInUse(_)) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("failed to reopen checkpointed store: {error}"),
        }
    };
    assert_eq!(recovered.read(context, object).unwrap().state(), next_state);
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn generation_switch_recovers_both_sides_of_the_manifest_commit_point() {
    let directory = std::env::temp_dir().join(format!("ousject-gc-crash-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let context = AccessContext::new(SubjectId::new());
    let object;
    {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        object = create(&manager, context, b"durable old generation", None);
    }

    // Simulate a crash after preparing the next snapshot but before switching
    // the manifest. The previous generation's WAL remains authoritative.
    let abandoned_snapshot = path.with_extension("oms.g1");
    let abandoned_wal = path.with_extension("wal.g1");
    std::fs::write(&abandoned_snapshot, b"incomplete next generation").unwrap();
    std::fs::write(&abandoned_wal, b"uncommitted WAL").unwrap();
    {
        let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
        assert_eq!(
            recovered.read(context, object).unwrap().state(),
            b"durable old generation"
        );
        assert!(!abandoned_snapshot.exists());
        assert!(!abandoned_wal.exists());
        recovered.checkpoint().unwrap();
    }

    // Simulate a crash during post-switch cleanup: the manifest and new
    // checkpoint are complete, but obsolete generation-zero files remain.
    std::fs::write(&path, b"obsolete old checkpoint").unwrap();
    std::fs::write(path.with_extension("wal"), b"obsolete old WAL").unwrap();
    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(
        recovered.read(context, object).unwrap().state(),
        b"durable old generation"
    );
    assert!(!path.exists());
    assert!(!path.with_extension("wal").exists());
    recovered.health_check().unwrap();
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn generation_manifest_corruption_is_not_treated_as_an_empty_store() {
    let directory = std::env::temp_dir().join(format!("ousject-manifest-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        create(
            &manager,
            AccessContext::new(SubjectId::new()),
            b"state",
            None,
        );
        manager.checkpoint().unwrap();
    }
    std::fs::write(path.with_extension("manifest"), b"corrupt manifest").unwrap();
    assert!(matches!(
        InMemoryObjectManager::open_persistent(&path),
        Err(OmsError::Corruption(_))
    ));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn compressed_incremental_wal_rejects_bit_corruption() {
    let directory = std::env::temp_dir().join(format!("ousject-wal-corrupt-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let context = AccessContext::new(SubjectId::new());
    {
        let manager = InMemoryObjectManager::open_persistent(&path).unwrap();
        let object = create(&manager, context, &vec![0_u8; 65_536], None);
        manager.checkpoint().unwrap();
        let view = manager.read(context, object).unwrap();
        let mut transaction = manager.begin(context);
        transaction
            .expect(object, view.header().version)
            .update_state(object, vec![255_u8; 4096]);
        manager.commit(transaction).unwrap();
    }
    let wal = path.with_extension("wal.g1");
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

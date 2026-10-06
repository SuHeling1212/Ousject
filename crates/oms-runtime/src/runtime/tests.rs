#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_old_snapshot_versions() {
        assert!(matches!(
            decode_snapshot(b"OMS1\0\0\0\0"),
            Err(OmsError::Corruption(_))
        ));
    }

    #[test]
    fn grouped_wal_frame_recovers_every_after_image_as_one_frame() {
        let directory = std::env::temp_dir().join(format!("ousject-wal-group-{}", ObjectId::new()));
        let path = directory.join("objects.oms");
        let manager = InMemoryObjectManager::new(1).unwrap();
        let context = AccessContext::new(SubjectId::new());
        let first = CreateObject::new(TypeId::new(), b"first-before");
        let first_id = first.id;
        manager.commit({
            let mut transaction = manager.begin(context);
            transaction.create(first);
            transaction
        }).unwrap();
        let second = CreateObject::new(TypeId::new(), b"second-before");
        let second_id = second.id;
        manager.commit({
            let mut transaction = manager.begin(context);
            transaction.create(second);
            transaction
        }).unwrap();

        let guards = manager
            .shards
            .iter()
            .map(|shard| shard.read().unwrap())
            .collect::<Vec<_>>();
        let mut state = combine_shards(guards.iter().map(|guard| &**guard)).unwrap();
        state.objects.get_mut(&first_id).unwrap().state = Arc::from(&b"first-after"[..]);
        let first_delta = encode_snapshot_delta(&state, &BTreeSet::from([first_id])).unwrap();
        state.objects.get_mut(&second_id).unwrap().state = Arc::from(&b"second-after"[..]);
        let second_delta = encode_snapshot_delta(&state, &BTreeSet::from([second_id])).unwrap();

        let backend = FileSnapshotBackend::new(&path);
        backend.load_recovery().unwrap();
        backend
            .persist_delta_group(&[&first_delta, &second_delta])
            .unwrap();
        drop(backend);

        let recovered_backend = FileSnapshotBackend::new(&path);
        let recovery = recovered_backend.load_recovery().unwrap();
        assert_eq!(recovery.updates.len(), 2);
        let journal = recovered_backend.journal.lock().unwrap();
        assert_eq!(journal.sequence, 1);
        assert_eq!(journal.records.len(), 1);
        drop(journal);
        let mut recovered = ShardState::default();
        for update in recovery.updates {
            apply_snapshot_delta(&mut recovered, &update).unwrap();
        }
        assert_eq!(recovered.objects[&first_id].state.as_ref(), b"first-after");
        assert_eq!(recovered.objects[&second_id].state.as_ref(), b"second-after");
        drop(recovered_backend);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn owner() -> (SubjectId, AccessContext) {
        let owner = SubjectId::new();
        (owner, AccessContext::new(owner))
    }

    fn legacy_tombstone_snapshot(id: ObjectId, type_id: TypeId, owner: SubjectId) -> Vec<u8> {
        let mut legacy = Vec::new();
        legacy.extend_from_slice(SNAPSHOT_MAGIC);
        snapshot_u32(&mut legacy, 1);
        snapshot_u128(&mut legacy, id.as_u128());
        snapshot_u128(&mut legacy, type_id.as_u128());
        legacy.push(0);
        snapshot_u64(&mut legacy, 9);
        legacy.push(lifecycle_tag(LifecycleState::Tombstoned));
        snapshot_bytes(&mut legacy, b"retained legacy payload").unwrap();
        snapshot_u32(&mut legacy, 0);
        snapshot_u32(&mut legacy, 0);
        snapshot_u16(&mut legacy, capability_bits(&all_capabilities()));
        snapshot_u128(&mut legacy, owner.as_u128());
        snapshot_u32(&mut legacy, 0);
        legacy
    }

    #[test]
    fn legacy_tombstones_get_a_fresh_retention_period_and_keep_metadata() {
        let id = ObjectId::new();
        let type_id = TypeId::new();
        let owner = SubjectId::new();
        let legacy = legacy_tombstone_snapshot(id, type_id, owner);

        let mut state = decode_snapshot(&legacy).unwrap();
        let record = state.objects.get(&id).unwrap();
        let retired_at = record.retired_at_unix_ms.unwrap();
        assert_eq!(record.header.type_id, type_id);
        assert_eq!(record.header.version, ObjectVersion::new(9));
        assert_eq!(record.policy.owner, owner);
        assert_eq!(record.state.as_ref(), b"retained legacy payload");
        assert_eq!(
            next_tombstone_reap_deadline(&state),
            retired_at.saturating_add(TOMBSTONE_RETENTION_MILLIS)
        );

        let encoded = encode_snapshot(&state).unwrap();
        state = decode_snapshot(&encoded).unwrap();
        let record = state.objects.get(&id).unwrap();
        assert_eq!(record.retired_at_unix_ms, Some(retired_at));

        let mut too_early = state.clone();
        let just_before = compact_tombstone_payloads(&mut too_early, retired_at - 1);
        assert_eq!(just_before.objects_compacted, 0);
        assert_eq!(just_before.tombstones_waiting_for_retention, 1);

        let mut expired = state;
        let eligible = compact_tombstone_payloads(&mut expired, retired_at);
        assert_eq!(eligible.objects_compacted, 1);
        assert_eq!(expired.objects[&id].state.len(), 0);
        assert_eq!(expired.objects[&id].header.id, id);
        assert_eq!(expired.objects[&id].header.type_id, type_id);
        assert_eq!(expired.objects[&id].header.version, ObjectVersion::new(9));
        assert_eq!(expired.objects[&id].policy.owner, owner);
        assert_eq!(expired.objects[&id].retired_at_unix_ms, Some(retired_at));
    }

    #[test]
    fn legacy_retirement_time_upgrade_survives_store_restart() {
        let directory = std::env::temp_dir().join(format!("ousject-legacy-{}", ObjectId::new()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("objects.oms");
        let id = ObjectId::new();
        let type_id = TypeId::new();
        let owner = SubjectId::new();
        std::fs::write(&path, legacy_tombstone_snapshot(id, type_id, owner)).unwrap();

        let first = InMemoryObjectManager::open_persistent(&path).unwrap();
        let context = AccessContext::new(owner);
        let first_header = first.inspect(context, id).unwrap();
        let first_retired_at = first
            .shard(id)
            .read()
            .unwrap()
            .objects
            .get(&id)
            .unwrap()
            .retired_at_unix_ms;
        assert!(first_retired_at.is_some());
        drop(first);

        let reopened = InMemoryObjectManager::open_persistent(&path).unwrap();
        assert_eq!(reopened.inspect(context, id).unwrap(), first_header);
        assert_eq!(
            reopened
                .shard(id)
                .read()
                .unwrap()
                .objects
                .get(&id)
                .unwrap()
                .retired_at_unix_ms,
            first_retired_at
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn background_reaper_wakes_for_an_expired_tombstone_and_keeps_its_id() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let (owner, context) = owner();
        let object = create(&manager, context, b"payload removed after expiry");
        let version = manager.inspect(context, object).unwrap().version;
        let mut retire = manager.begin(context);
        retire.expect(object, version).tombstone(object);
        manager.commit(retire).unwrap();

        let retired_at = unix_time_millis()
            .unwrap()
            .saturating_sub(TOMBSTONE_RETENTION_MILLIS + 1);
        manager
            .shard(object)
            .write()
            .unwrap()
            .objects
            .get_mut(&object)
            .unwrap()
            .retired_at_unix_ms = Some(retired_at);
        manager.next_tombstone_reap_unix_ms.store(
            retired_at.saturating_add(TOMBSTONE_RETENTION_MILLIS),
            Ordering::Release,
        );

        let reaper = TombstoneReaper::start(&manager).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if manager.shard(object).read().unwrap().objects[&object]
                .state
                .is_empty()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "reaper did not compact the expired object"
            );
            thread::yield_now();
        }
        drop(reaper);

        let header = manager.inspect(AccessContext::new(owner), object).unwrap();
        assert_eq!(header.lifecycle, LifecycleState::Tombstoned);
        assert_eq!(manager.stats().unwrap().tombstoned_count, 1);
        assert_eq!(
            manager.next_tombstone_reap_unix_ms.load(Ordering::Acquire),
            u64::MAX
        );
        let mut duplicate = manager.begin(context);
        duplicate.create(CreateObject::new(TypeId::new(), b"reuse").with_id(object));
        assert!(matches!(
            manager.commit(duplicate),
            Err(OmsError::InvalidOperation("ObjectId already exists"))
        ));
    }

    fn create(manager: &InMemoryObjectManager, context: AccessContext, state: &[u8]) -> ObjectId {
        let request = CreateObject::new(TypeId::new(), state);
        let id = request.id;
        let mut transaction = manager.begin(context);
        transaction.create(request);
        manager.commit(transaction).unwrap();
        id
    }

    #[test]
    fn immutable_view_survives_new_version() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, context) = owner();
        let object = create(&manager, context, b"zero");
        let old = manager.read(context, object).unwrap();

        let mut transaction = manager.begin(context);
        transaction
            .expect(object, old.header().version)
            .update_state(object, b"one");
        manager.commit(transaction).unwrap();

        let new = manager.read(context, object).unwrap();
        assert_eq!(old.state(), b"zero");
        assert_eq!(new.state(), b"one");
        assert_eq!(new.header().version, old.header().version.next());
    }

    #[test]
    fn optimistic_conflict_preserves_winner() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, context) = owner();
        let object = create(&manager, context, b"zero");
        let version = manager.read(context, object).unwrap().header().version;

        let mut first = manager.begin(context);
        first.expect(object, version).update_state(object, b"first");
        let mut second = manager.begin(context);
        second
            .expect(object, version)
            .update_state(object, b"second");

        manager.commit(first).unwrap();
        assert!(matches!(
            manager.commit(second),
            Err(OmsError::Conflict { .. })
        ));
        assert_eq!(manager.read(context, object).unwrap().state(), b"first");
    }

    #[test]
    fn denied_write_changes_nothing() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, owner_context) = owner();
        let object = create(&manager, owner_context, b"safe");
        let before = manager.read(owner_context, object).unwrap();
        let intruder = AccessContext::new(SubjectId::new());

        let mut transaction = manager.begin(intruder);
        transaction
            .expect(object, before.header().version)
            .update_state(object, b"corrupted");
        assert!(matches!(
            manager.commit(transaction),
            Err(OmsError::Denied {
                capability: Capability::ReplaceValue,
                ..
            })
        ));

        let after = manager.read(owner_context, object).unwrap();
        assert_eq!(after.state(), b"safe");
        assert_eq!(after.header().version, before.header().version);
    }

    #[test]
    fn parent_link_and_tombstone_are_transactional() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, context) = owner();
        let parent = create(&manager, context, b"process");
        let parent_version = manager.read(context, parent).unwrap().header().version;

        let child_request = CreateObject::new(TypeId::new(), b"0".to_vec()).with_parent(parent);
        let child = child_request.id;
        let mut create_child = manager.begin(context);
        create_child
            .expect(parent, parent_version)
            .create(child_request);
        manager.commit(create_child).unwrap();

        let parent_view = manager.read(context, parent).unwrap();
        assert!(parent_view.children().contains(&child));

        let mut link = manager.begin(context);
        link.expect(parent, parent_view.header().version)
            .set_link(parent, "displayed", child);
        manager.commit(link).unwrap();
        assert_eq!(
            manager.read(context, parent).unwrap().links()["displayed"],
            child
        );

        let parent_version = manager.read(context, parent).unwrap().header().version;
        let child_version = manager.read(context, child).unwrap().header().version;
        let mut tombstone = manager.begin(context);
        tombstone
            .expect(parent, parent_version)
            .expect(child, child_version)
            .tombstone(child);
        manager.commit(tombstone).unwrap();

        assert!(
            !manager
                .read(context, parent)
                .unwrap()
                .children()
                .contains(&child)
        );
        assert!(matches!(
            manager.read(context, child),
            Err(OmsError::InvalidLifecycle {
                state: LifecycleState::Tombstoned,
                ..
            })
        ));
    }

    #[test]
    fn capability_can_be_granted_atomically() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, owner_context) = owner();
        let reader = SubjectId::new();
        let reader_context = AccessContext::new(reader);
        let object = create(&manager, owner_context, b"visible");
        assert!(matches!(
            manager.read(reader_context, object),
            Err(OmsError::Denied { .. })
        ));

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
        assert_eq!(
            manager.read(reader_context, object).unwrap().state(),
            b"visible"
        );
    }
}

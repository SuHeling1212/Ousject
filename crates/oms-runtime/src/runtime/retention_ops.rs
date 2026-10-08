#[derive(Debug, Clone, Copy, Default)]
#[cfg(feature = "std")]
struct GcCounts {
    objects_scanned: u64,
    active_objects: u64,
    tombstones: u64,
    tombstones_waiting_for_retention: u64,
    objects_compacted: u64,
    live_payload_bytes: u64,
    dead_payload_bytes_before: u64,
    dead_payload_bytes_after: u64,
}

fn needs_tombstone_compaction(record: &ObjectRecord) -> bool {
    record.header.lifecycle == LifecycleState::Tombstoned
        && !unresolved_effect_state(record.header.type_id, &record.state)
        && (!record.state.is_empty()
            || !record.links.is_empty()
            || record.capabilities != BTreeSet::from([Capability::Inspect])
            || record
                .policy
                .grants
                .values()
                .any(|capabilities| capabilities != &BTreeSet::from([Capability::Inspect])))
}

fn tombstone_reap_deadline(record: &ObjectRecord) -> Option<u64> {
    if !needs_tombstone_compaction(record) {
        return None;
    }
    record
        .retired_at_unix_ms
        .map(|retired_at| retired_at.saturating_add(TOMBSTONE_RETENTION_MILLIS))
}

#[cfg(feature = "std")]
fn next_tombstone_reap_deadline(state: &ShardState) -> u64 {
    state
        .objects
        .values()
        .filter_map(tombstone_reap_deadline)
        .min()
        .unwrap_or(u64::MAX)
}

#[cfg(feature = "std")]
fn compact_tombstone_payloads(state: &mut ShardState, cutoff_unix_ms: u64) -> GcCounts {
    let mut counts = GcCounts::default();
    let objects = state.objects.keys().copied().collect::<Vec<_>>();
    for object in objects {
        let Some(record) = state.objects.get_mut(&object) else {
            continue;
        };
        counts.objects_scanned = counts.objects_scanned.saturating_add(1);
        let payload = u64::try_from(record.state.len()).unwrap_or(u64::MAX);
        if record.header.lifecycle != LifecycleState::Tombstoned {
            counts.active_objects = counts.active_objects.saturating_add(1);
            counts.live_payload_bytes = counts.live_payload_bytes.saturating_add(payload);
            continue;
        }
        counts.tombstones = counts.tombstones.saturating_add(1);
        counts.dead_payload_bytes_before = counts.dead_payload_bytes_before.saturating_add(payload);
        if record
            .retired_at_unix_ms
            .is_none_or(|retired_at| retired_at > cutoff_unix_ms)
        {
            counts.tombstones_waiting_for_retention =
                counts.tombstones_waiting_for_retention.saturating_add(1);
            counts.dead_payload_bytes_after =
                counts.dead_payload_bytes_after.saturating_add(payload);
            continue;
        }
        if unresolved_effect_state(record.header.type_id, &record.state) {
            // An Effect may be in flight or have an unknown external outcome.
            // Keep its full durable idempotency record until it is resolved.
            counts.dead_payload_bytes_after =
                counts.dead_payload_bytes_after.saturating_add(payload);
            continue;
        }
        let compactable = needs_tombstone_compaction(record);
        if compactable {
            counts.objects_compacted = counts.objects_compacted.saturating_add(1);
            record.state = Arc::from(Vec::<u8>::new());
            record.links.clear();
            record.capabilities = BTreeSet::from([Capability::Inspect]);
            record.policy.grants.retain(|_, capabilities| {
                capabilities.retain(|capability| *capability == Capability::Inspect);
                !capabilities.is_empty()
            });
        }
        counts.dead_payload_bytes_after = counts
            .dead_payload_bytes_after
            .saturating_add(u64::try_from(record.state.len()).unwrap_or(u64::MAX));
    }
    counts
}

#[cfg(feature = "std")]
fn validate_gc_candidate(state: &ShardState, types: &TypeRegistry) -> Result<(), OmsError> {
    validate_parent_graph(state)?;
    validate_type_index(state)?;
    validate_dynamic_types(state, types)
}

fn unresolved_effect_state(type_id: TypeId, state: &[u8]) -> bool {
    if type_id != CORE_EFFECT_TYPE {
        return false;
    }
    let Ok(Value::Record(fields)) = Value::decode(state) else {
        // A malformed Effect cannot safely be classified as resolved.
        return true;
    };
    !matches!(
        fields.get("status"),
        Some(Value::Text(status)) if status == "completed" || status == "failed"
    )
}

#[cfg(feature = "std")]
fn unix_time_millis() -> Result<u64, OmsError> {
    system_time_to_unix_millis(SystemTime::now())
}

// Native has no trusted wall-clock source yet. Never fabricate Unix time for
// persisted retirement metadata; lifecycle operations that require it fail
// explicitly until a trusted platform clock is available.
#[cfg(not(feature = "std"))]
fn unix_time_millis() -> Result<u64, OmsError> {
    Err(OmsError::Storage(
        "Native trusted wall clock is unavailable".to_owned(),
    ))
}

#[cfg(feature = "std")]
fn system_time_to_unix_millis(time: SystemTime) -> Result<u64, OmsError> {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| OmsError::Storage("system time is before the Unix epoch".to_owned()))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| OmsError::Storage("Unix time is out of supported range".to_owned()))
}

#[cfg(feature = "std")]
const fn retention_cutoff_unix_ms(now_unix_ms: u64) -> u64 {
    now_unix_ms.saturating_sub(TOMBSTONE_RETENTION_MILLIS)
}

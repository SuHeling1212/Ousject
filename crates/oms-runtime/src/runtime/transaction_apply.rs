impl ObjectManager for InMemoryObjectManager {
    fn read(&self, context: AccessContext, object: ObjectId) -> Result<ObjectView, OmsError> {
        Self::read(self, context, object)
    }

    fn inspect(&self, context: AccessContext, object: ObjectId) -> Result<ObjectHeader, OmsError> {
        Self::inspect(self, context, object)
    }

    fn begin(&self, context: AccessContext) -> Transaction {
        Self::begin(self, context)
    }

    fn commit(&self, transaction: Transaction) -> Result<CommitResult, OmsError> {
        Self::commit(self, transaction)
    }

    fn list(&self, context: AccessContext) -> Result<Vec<ObjectHeader>, OmsError> {
        Self::list(self, context)
    }
}

fn transaction_lock_plan(
    transactions: &[Transaction],
    directory: &FixedDirectory,
    shard_count: usize,
) -> (BTreeSet<usize>, BTreeSet<usize>) {
    let mut writes = BTreeSet::new();
    let mut reads = BTreeSet::new();
    for transaction in transactions {
        for object in transaction.expected.keys() {
            reads.insert(directory.locate(*object).get() as usize);
        }
        for operation in &transaction.operations {
            match operation {
                Operation::Create(request) => {
                    writes.insert(directory.locate(request.id).get() as usize);
                    if let Some(parent) = request.parent {
                        writes.insert(directory.locate(parent).get() as usize);
                    }
                    for target in request.links.values() {
                        reads.insert(directory.locate(*target).get() as usize);
                    }
                }
                Operation::UpdateState { object, .. }
                | Operation::Grant { object, .. }
                | Operation::Revoke { object, .. } => {
                    writes.insert(directory.locate(*object).get() as usize);
                }
                Operation::SetLink { source, target, .. } => {
                    writes.insert(directory.locate(*source).get() as usize);
                    reads.insert(directory.locate(*target).get() as usize);
                }
                Operation::RemoveLink { source, .. } => {
                    writes.insert(directory.locate(*source).get() as usize);
                }
                // Reparenting and retirement inspect/change the old parent;
                // hold all shards in stable order until metadata-specific
                // lock planning can do so without a race.
                Operation::Reparent { .. } | Operation::Tombstone { .. } => {
                    writes.extend(0..shard_count);
                }
            }
        }
    }
    reads.retain(|shard| !writes.contains(shard));
    (writes, reads)
}

fn transaction_changes_dynamic_types(
    manager: &InMemoryObjectManager,
    transactions: &[Transaction],
) -> Result<bool, OmsError> {
    for transaction in transactions {
        for operation in &transaction.operations {
            match operation {
                Operation::Create(request) if request.type_id == TYPE_DESCRIPTOR_TYPE => {
                    return Ok(true);
                }
                Operation::UpdateState { object, .. } | Operation::Tombstone { object } => {
                    let shard = manager.shard(*object);
                    let state = shard
                        .read()
                        .map_err(|_| OmsError::TemporarilyUnavailable)?;
                    if state
                        .objects
                        .get(object)
                        .is_some_and(|record| record.header.type_id == TYPE_DESCRIPTOR_TYPE)
                    {
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(false)
}

fn validate_expected(
    state: &ShardState,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
) -> Result<(), OmsError> {
    for (&object, &version) in expected {
        let actual = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?
            .header
            .version;
        if version != actual {
            return Err(OmsError::Conflict {
                object,
                expected: version,
                actual,
            });
        }
    }
    Ok(())
}

fn require_expected(
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    object: ObjectId,
) -> Result<(), OmsError> {
    if expected.contains_key(&object) {
        Ok(())
    } else {
        Err(OmsError::InvalidOperation(
            "every modified existing object needs an expected version",
        ))
    }
}

struct ApplyState<'a> {
    access: AccessContext,
    expected: &'a BTreeMap<ObjectId, ObjectVersion>,
    changed: &'a mut BTreeSet<ObjectId>,
    created: &'a mut BTreeSet<ObjectId>,
    relationships_changed: &'a mut BTreeSet<(ObjectId, ObjectId, bool)>,
    cycle_starts: &'a mut BTreeSet<ObjectId>,
}

struct AppliedTransactions {
    results: Vec<CommitResult>,
    changed: BTreeSet<ObjectId>,
    tombstone_deadlines: Vec<u64>,
}

fn apply_transactions(
    candidate: &mut ShardState,
    transactions: Vec<Transaction>,
    types: &TypeRegistry,
) -> Result<AppliedTransactions, OmsError> {
    let mut results = Vec::with_capacity(transactions.len());
    let mut tombstone_deadlines = Vec::new();
    let mut batch_changed = BTreeSet::new();
    for transaction in transactions {
        validate_expected(candidate, &transaction.expected)?;
        let mut changed = BTreeSet::new();
        let mut created = BTreeSet::new();
        let mut relationships_changed = BTreeSet::new();
        let mut cycle_starts = BTreeSet::new();
        {
            let mut apply = ApplyState {
                access: transaction.context,
                expected: &transaction.expected,
                changed: &mut changed,
                created: &mut created,
                relationships_changed: &mut relationships_changed,
                cycle_starts: &mut cycle_starts,
            };
            for operation in transaction.operations {
                apply_operation(candidate, operation, &mut apply)?;
            }
        }

        validate_parent_changes(candidate, &relationships_changed, &cycle_starts)?;
        validate_created_type_index(candidate, &created)?;
        if changed.iter().any(|object| {
            candidate
                .objects
                .get(object)
                .is_some_and(|record| record.header.type_id == TYPE_DESCRIPTOR_TYPE)
        }) {
            validate_dynamic_types(candidate, types)?;
        }

        let mut versions = BTreeMap::new();
        for &object in &changed {
            let record = candidate
                .objects
                .get_mut(&object)
                .ok_or(OmsError::NotFound(object))?;
            if let Some(deadline) = tombstone_reap_deadline(record) {
                tombstone_deadlines.push(deadline);
            }
            if !created.contains(&object) {
                record.header.version = record
                    .header
                    .version
                    .checked_next()
                    .ok_or(OmsError::VersionExhausted(object))?;
            }
            versions.insert(object, record.header.version);
            batch_changed.insert(object);
        }
        results.push(CommitResult {
            transaction_id: transaction.id,
            versions,
        });
    }
    Ok(AppliedTransactions {
        results,
        changed: batch_changed,
        tombstone_deadlines,
    })
}

fn apply_operation(
    state: &mut ShardState,
    operation: Operation,
    apply: &mut ApplyState<'_>,
) -> Result<(), OmsError> {
    match operation {
        Operation::Create(request) => apply_create(state, request, apply),
        Operation::UpdateState {
            object,
            state: data,
        } => apply_update(state, object, data, apply),
        Operation::SetLink {
            source,
            name,
            target,
        } => apply_set_link(state, source, name, target, apply),
        Operation::RemoveLink { source, name } => apply_remove_link(state, source, &name, apply),
        Operation::Reparent { child, new_parent } => {
            apply_reparent(state, child, new_parent, apply)
        }
        Operation::Grant {
            object,
            subject,
            capability,
        } => apply_grant(state, object, subject, capability, apply),
        Operation::Revoke {
            object,
            subject,
            capability,
        } => apply_revoke(state, object, subject, capability, apply),
        Operation::Tombstone { object } => apply_tombstone(state, object, apply),
    }
}

fn apply_create(
    state: &mut ShardState,
    request: CreateObject,
    apply: &mut ApplyState<'_>,
) -> Result<(), OmsError> {
    if state.objects.contains_key(&request.id) {
        return Err(OmsError::InvalidOperation("ObjectId already exists"));
    }
    if request.links.keys().any(String::is_empty) {
        return Err(OmsError::InvalidOperation("link name cannot be empty"));
    }
    if request.type_id == CORE_NAMESPACE_TYPE {
        for name in request.links.keys() {
            validate_name(name)?;
        }
    }
    for target in request.links.values() {
        if *target != request.id {
            active_record(state, *target)?;
        }
    }
    if let Some(parent) = request.parent {
        if !apply.created.contains(&parent) {
            require_expected(apply.expected, parent)?;
        }
        let parent_record = active_record_mut(state, parent)?;
        parent_record.require(apply.access, Capability::CreateChild)?;
        parent_record.children.insert(request.id);
        apply.changed.insert(parent);
        apply.relationships_changed.insert((parent, request.id, true));
    }
    let id = request.id;
    let type_id = request.type_id;
    state.objects.insert(
        id,
        ObjectRecord {
            header: ObjectHeader {
                id,
                type_id: request.type_id,
                parent_id: request.parent,
                version: ObjectVersion::default(),
                lifecycle: LifecycleState::Active,
            },
            retired_at_unix_ms: None,
            state: Arc::from(request.state),
            children: BTreeSet::new(),
            links: request.links,
            capabilities: request.capabilities,
            policy: AccessPolicy {
                owner: apply.access.subject,
                grants: request.initial_grants,
            },
        },
    );
    state.by_type.entry(type_id).or_default().insert(id);
    apply.changed.insert(id);
    apply.created.insert(id);
    Ok(())
}

fn apply_update(
    state: &mut ShardState,
    object: ObjectId,
    data: Vec<u8>,
    apply: &mut ApplyState<'_>,
) -> Result<(), OmsError> {
    require_expected(apply.expected, object)?;
    let record = active_record_mut(state, object)?;
    record.require(apply.access, Capability::ReplaceValue)?;
    record.state = Arc::from(data);
    apply.changed.insert(object);
    Ok(())
}

fn apply_set_link(
    state: &mut ShardState,
    source: ObjectId,
    name: String,
    target: ObjectId,
    apply: &mut ApplyState<'_>,
) -> Result<(), OmsError> {
    if !apply.created.contains(&source) {
        require_expected(apply.expected, source)?;
    }
    if name.is_empty() {
        return Err(OmsError::InvalidOperation("link name cannot be empty"));
    }
    active_record(state, target)?;
    let record = active_record_mut(state, source)?;
    if record.header.type_id == CORE_NAMESPACE_TYPE {
        validate_name(&name)?;
    }
    record.require(apply.access, Capability::Link)?;
    record.links.insert(name, target);
    apply.changed.insert(source);
    Ok(())
}

fn apply_remove_link(
    state: &mut ShardState,
    source: ObjectId,
    name: &str,
    apply: &mut ApplyState<'_>,
) -> Result<(), OmsError> {
    require_expected(apply.expected, source)?;
    let record = active_record_mut(state, source)?;
    if record.header.type_id == CORE_NAMESPACE_TYPE {
        validate_name(name)?;
    }
    record.require(apply.access, Capability::Link)?;
    record.links.remove(name);
    apply.changed.insert(source);
    Ok(())
}

fn apply_reparent(
    state: &mut ShardState,
    child: ObjectId,
    new_parent: Option<ObjectId>,
    apply: &mut ApplyState<'_>,
) -> Result<(), OmsError> {
    require_expected(apply.expected, child)?;
    let old_parent = active_record(state, child)?.header.parent_id;
    active_record(state, child)?.require(apply.access, Capability::Reparent)?;

    if let Some(parent) = old_parent {
        require_expected(apply.expected, parent)?;
        let parent_record = active_record_mut(state, parent)?;
        parent_record.require(apply.access, Capability::Reparent)?;
        parent_record.children.remove(&child);
        apply.changed.insert(parent);
        apply.relationships_changed.insert((parent, child, false));
    }
    if let Some(parent) = new_parent {
        require_expected(apply.expected, parent)?;
        let parent_record = active_record_mut(state, parent)?;
        parent_record.require(apply.access, Capability::Reparent)?;
        parent_record.children.insert(child);
        apply.changed.insert(parent);
        apply.relationships_changed.insert((parent, child, true));
    }
    active_record_mut(state, child)?.header.parent_id = new_parent;
    apply.changed.insert(child);
    apply.cycle_starts.insert(child);
    Ok(())
}

fn apply_grant(
    state: &mut ShardState,
    object: ObjectId,
    subject: SubjectId,
    capability: Capability,
    apply: &mut ApplyState<'_>,
) -> Result<(), OmsError> {
    require_expected(apply.expected, object)?;
    let record = active_record_mut(state, object)?;
    record.require(apply.access, Capability::ManagePolicy)?;
    if !record.capabilities.contains(&capability) {
        return Err(OmsError::InvalidOperation(
            "cannot grant a capability unsupported by the object",
        ));
    }
    record
        .policy
        .grants
        .entry(subject)
        .or_default()
        .insert(capability);
    apply.changed.insert(object);
    Ok(())
}

fn apply_revoke(
    state: &mut ShardState,
    object: ObjectId,
    subject: SubjectId,
    capability: Capability,
    apply: &mut ApplyState<'_>,
) -> Result<(), OmsError> {
    require_expected(apply.expected, object)?;
    let record = active_record_mut(state, object)?;
    record.require(apply.access, Capability::ManagePolicy)?;
    if let Some(capabilities) = record.policy.grants.get_mut(&subject) {
        capabilities.remove(&capability);
        if capabilities.is_empty() {
            record.policy.grants.remove(&subject);
        }
    }
    apply.changed.insert(object);
    Ok(())
}

fn apply_tombstone(
    state: &mut ShardState,
    object: ObjectId,
    apply: &mut ApplyState<'_>,
) -> Result<(), OmsError> {
    require_expected(apply.expected, object)?;
    let object_record = active_record(state, object)?;
    if !object_record.children.is_empty() {
        return Err(OmsError::InvalidOperation(
            "an object with children must be reparented before tombstoning",
        ));
    }
    let parent = object_record.header.parent_id;
    object_record.require(apply.access, Capability::Retire)?;
    if unresolved_effect_state(object_record.header.type_id, &object_record.state) {
        return Err(OmsError::InvalidOperation(
            "an unresolved Effect cannot be tombstoned",
        ));
    }
    let retired_at_unix_ms = unix_time_millis()?;
    if let Some(parent) = parent {
        require_expected(apply.expected, parent)?;
        let parent_record = active_record_mut(state, parent)?;
        parent_record.require(apply.access, Capability::Reparent)?;
        parent_record.children.remove(&object);
        apply.changed.insert(parent);
        apply.relationships_changed.insert((parent, object, false));
    }
    let record = active_record_mut(state, object)?;
    record.header.parent_id = None;
    record.header.lifecycle = LifecycleState::Tombstoned;
    record.retired_at_unix_ms = Some(retired_at_unix_ms);
    apply.changed.insert(object);
    Ok(())
}

fn active_record(state: &ShardState, object: ObjectId) -> Result<&ObjectRecord, OmsError> {
    let record = state
        .objects
        .get(&object)
        .ok_or(OmsError::NotFound(object))?;
    if record.header.lifecycle == LifecycleState::Tombstoned {
        return Err(OmsError::InvalidLifecycle {
            object,
            state: record.header.lifecycle,
        });
    }
    Ok(record)
}

fn active_record_mut(
    state: &mut ShardState,
    object: ObjectId,
) -> Result<&mut ObjectRecord, OmsError> {
    let record = state
        .objects
        .get_mut(&object)
        .ok_or(OmsError::NotFound(object))?;
    if record.header.lifecycle == LifecycleState::Tombstoned {
        return Err(OmsError::InvalidLifecycle {
            object,
            state: record.header.lifecycle,
        });
    }
    Ok(record)
}

fn combine_shards<'a>(
    shards: impl IntoIterator<Item = &'a ShardState>,
) -> Result<ShardState, OmsError> {
    let mut combined = ShardState::default();
    for shard in shards {
        for (&object, record) in &shard.objects {
            if combined.objects.insert(object, record.clone()).is_some() {
                return Err(OmsError::InvalidOperation(
                    "an ObjectId exists in more than one shard",
                ));
            }
            combined
                .by_type
                .entry(record.header.type_id)
                .or_default()
                .insert(object);
        }
    }
    Ok(combined)
}

fn partition_shards(state: ShardState, directory: &FixedDirectory) -> Vec<ShardState> {
    let mut shards = (0..directory.shard_count())
        .map(|_| ShardState::default())
        .collect::<Vec<_>>();
    for (object, record) in state.objects {
        let shard = &mut shards[directory.locate(object).get() as usize];
        shard
            .by_type
            .entry(record.header.type_id)
            .or_default()
            .insert(object);
        shard.objects.insert(object, record);
    }
    shards
}

fn validate_parent_graph(state: &ShardState) -> Result<(), OmsError> {
    for (&start, record) in &state.objects {
        if record.header.lifecycle == LifecycleState::Tombstoned
            && (record.header.parent_id.is_some() || !record.children.is_empty())
        {
            return Err(OmsError::InvalidOperation(
                "tombstoned objects cannot retain parent or child relationships",
            ));
        }
        if let Some(parent) = record.header.parent_id {
            let parent_record = state
                .objects
                .get(&parent)
                .ok_or(OmsError::NotFound(parent))?;
            if !parent_record.children.contains(&start) {
                return Err(OmsError::InvalidOperation(
                    "parent header and child index are inconsistent",
                ));
            }
        }
        for child in &record.children {
            let child_record = state.objects.get(child).ok_or(OmsError::NotFound(*child))?;
            if child_record.header.parent_id != Some(start) {
                return Err(OmsError::InvalidOperation(
                    "child index and parent header are inconsistent",
                ));
            }
        }

        let mut seen = BTreeSet::new();
        let mut current = Some(start);
        while let Some(object) = current {
            if !seen.insert(object) {
                return Err(OmsError::ParentCycle);
            }
            current = state
                .objects
                .get(&object)
                .and_then(|record| record.header.parent_id);
        }
    }
    Ok(())
}

fn validate_type_index(state: &ShardState) -> Result<(), OmsError> {
    for (&object, record) in &state.objects {
        if !state
            .by_type
            .get(&record.header.type_id)
            .is_some_and(|objects| objects.contains(&object))
        {
            return Err(OmsError::InvalidOperation(
                "object and type index are inconsistent",
            ));
        }
    }
    for (&type_id, objects) in &state.by_type {
        for object in objects {
            if state
                .objects
                .get(object)
                .is_none_or(|record| record.header.type_id != type_id)
            {
                return Err(OmsError::InvalidOperation(
                    "type index and object are inconsistent",
                ));
            }
        }
    }
    Ok(())
}

fn validate_created_type_index(
    state: &ShardState,
    created: &BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    for &object in created {
        let record = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?;
        if !state
            .by_type
            .get(&record.header.type_id)
            .is_some_and(|objects| objects.contains(&object))
        {
            return Err(OmsError::InvalidOperation(
                "object and type index are inconsistent",
            ));
        }
    }
    Ok(())
}

fn validate_parent_changes(
    state: &ShardState,
    relationships_changed: &BTreeSet<(ObjectId, ObjectId, bool)>,
    cycle_starts: &BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    for &(parent, child, should_exist) in relationships_changed {
        let parent_record = state
            .objects
            .get(&parent)
            .ok_or(OmsError::NotFound(parent))?;
        let child_record = state
            .objects
            .get(&child)
            .ok_or(OmsError::NotFound(child))?;
        let edge_exists = parent_record.children.contains(&child)
            && child_record.header.parent_id == Some(parent);
        if edge_exists != should_exist {
            return Err(OmsError::InvalidOperation(
                "parent header and child index are inconsistent",
            ));
        }
    }

    for &start in cycle_starts {
        let mut seen = BTreeSet::new();
        let mut current = Some(start);
        while let Some(object) = current {
            if !seen.insert(object) {
                return Err(OmsError::ParentCycle);
            }
            current = state
                .objects
                .get(&object)
                .ok_or(OmsError::NotFound(object))?
                .header
                .parent_id;
        }
    }
    Ok(())
}

fn validate_dynamic_types(state: &ShardState, builtins: &TypeRegistry) -> Result<(), OmsError> {
    let mut ids = builtins.by_id.keys().copied().collect::<BTreeSet<_>>();
    let mut names = builtins.by_name.keys().cloned().collect::<BTreeSet<_>>();
    for object in state
        .by_type
        .get(&TYPE_DESCRIPTOR_TYPE)
        .into_iter()
        .flatten()
    {
        let record = state
            .objects
            .get(object)
            .ok_or(OmsError::NotFound(*object))?;
        if record.header.lifecycle == LifecycleState::Tombstoned {
            continue;
        }
        let descriptor = decode_type_descriptor(&record.state)?;
        if descriptor.name.is_empty() || descriptor.name.contains('\0') {
            return Err(OmsError::InvalidOperation(
                "Type Descriptor name must be non-empty and contain no NUL",
            ));
        }
        if !ids.insert(descriptor.id) || !names.insert(descriptor.name) {
            return Err(OmsError::InvalidOperation(
                "Type Descriptor id or name is duplicated",
            ));
        }
    }
    Ok(())
}

fn all_capabilities() -> BTreeSet<Capability> {
    [
        Capability::ViewValue,
        Capability::ReplaceValue,
        Capability::CreateChild,
        Capability::Invoke,
        Capability::Link,
        Capability::Reparent,
        Capability::Retire,
        Capability::Inspect,
        Capability::ManagePolicy,
    ]
    .into_iter()
    .collect()
}

fn validate_name(name: &str) -> Result<(), OmsError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        Err(OmsError::InvalidName(name.to_owned()))
    } else {
        Ok(())
    }
}

fn committed_version(result: &CommitResult, object: ObjectId) -> Result<ObjectVersion, OmsError> {
    result
        .versions
        .get(&object)
        .copied()
        .ok_or(OmsError::InvalidOperation("commit omitted modified object"))
}

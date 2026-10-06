impl InMemoryObjectManager {
    /// Creates an in-memory manager with fixed shard routing.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::InvalidShardCount`] when `shard_count` is zero.
    pub fn new(shard_count: u32) -> Result<Self, OmsError> {
        let directory = FixedDirectory::new(shard_count)?;
        let shards = (0..shard_count)
            .map(|_| RwLock::new(ShardState::default()))
            .collect();
        Ok(Self {
            directory,
            shards,
            persistence: None,
            types: TypeRegistry::builtins(),
            next_tombstone_reap_unix_ms: AtomicU64::new(u64::MAX),
            reaper_wakeup: Mutex::new(None),
            performance: PerformanceCounters::default(),
        })
    }

    /// Opens the single-Shard durable Object Store used by the system MVP.
    /// Existing state is validated before becoming visible.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot be read or is corrupt.
    pub fn open_persistent(path: impl AsRef<Path>) -> Result<Self, OmsError> {
        Self::open_with_backend(Arc::new(FileSnapshotBackend::new(path)))
    }

    /// Opens a durable Object Store with fixed multi-shard routing.
    ///
    /// The durable representation is one globally consistent image; recovery
    /// repartitions Objects using the same stable directory function.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid shard count, an in-use Store, storage
    /// failure, or corrupt durable state.
    pub fn open_persistent_with_shards(
        path: impl AsRef<Path>,
        shard_count: u32,
    ) -> Result<Self, OmsError> {
        Self::open_with_backend_and_shards(Arc::new(FileSnapshotBackend::new(path)), shard_count)
    }

    /// Opens a single-Shard store using a replaceable persistence backend.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot load a valid snapshot.
    pub fn open_with_backend(backend: Arc<dyn SnapshotBackend>) -> Result<Self, OmsError> {
        Self::open_with_backend_and_shards(backend, 1)
    }

    /// Opens a durable Store and repartitions its globally consistent state.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot load a valid snapshot or the
    /// shard count is zero.
    pub fn open_with_backend_and_shards(
        backend: Arc<dyn SnapshotBackend>,
        shard_count: u32,
    ) -> Result<Self, OmsError> {
        let recovery = backend.load_recovery()?;
        let needs_retirement_time_upgrade = recovery.checkpoint.as_ref().is_some_and(|snapshot| {
            snapshot.get(4..8) != Some(RETIREMENT_TIME_EXTENSION.as_slice())
        });
        let mut state = recovery.checkpoint.map_or_else(
            || Ok(ShardState::default()),
            |bytes| decode_snapshot(&bytes),
        )?;
        for update in recovery.updates {
            apply_snapshot_delta(&mut state, &update)?;
        }
        validate_parent_graph(&state)?;
        validate_type_index(&state)?;
        let types = TypeRegistry::builtins();
        validate_dynamic_types(&state, &types)?;
        if needs_retirement_time_upgrade {
            // Persist upgrade-time timestamps once, so later restarts do not
            // restart the retention period for legacy Tombstones.
            backend.compact(&encode_snapshot(&state)?)?;
        }
        let next_tombstone_reap_unix_ms = next_tombstone_reap_deadline(&state);
        let directory = FixedDirectory::new(shard_count)?;
        let shards = partition_shards(state, &directory)
            .into_iter()
            .map(RwLock::new)
            .collect();
        Ok(Self {
            directory,
            shards,
            persistence: Some(backend),
            types,
            next_tombstone_reap_unix_ms: AtomicU64::new(next_tombstone_reap_unix_ms),
            reaper_wakeup: Mutex::new(None),
            performance: PerformanceCounters::default(),
        })
    }

    /// Returns every valid built-in and persistent dynamic Type descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error if persistent descriptor state is malformed.
    pub fn types(&self) -> Result<Vec<TypeDescriptor>, OmsError> {
        let mut types = self.types.all();
        types.extend(self.dynamic_types()?);
        types.sort_by_key(|descriptor| descriptor.id);
        Ok(types)
    }

    /// Resolves a registered type by its stable source name.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::UnknownTypeName`] when no descriptor is registered.
    pub fn type_by_name(&self, name: &str) -> Result<TypeDescriptor, OmsError> {
        match self.types.by_name(name) {
            Ok(descriptor) => Ok(descriptor.clone()),
            Err(OmsError::UnknownTypeName(_)) => self
                .dynamic_types()?
                .into_iter()
                .find(|descriptor| descriptor.name == name)
                .ok_or_else(|| OmsError::UnknownTypeName(name.to_owned())),
            Err(error) => Err(error),
        }
    }

    /// Resolves a registered type by its stable identifier.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::UnknownType`] when no descriptor is registered.
    pub fn type_by_id(&self, id: TypeId) -> Result<TypeDescriptor, OmsError> {
        match self.types.by_id(id) {
            Ok(descriptor) => Ok(descriptor.clone()),
            Err(OmsError::UnknownType(_)) => self
                .dynamic_types()?
                .into_iter()
                .find(|descriptor| descriptor.id == id)
                .ok_or(OmsError::UnknownType(id)),
            Err(error) => Err(error),
        }
    }

    /// Registers a persistent Type Descriptor Object at runtime.
    ///
    /// # Errors
    ///
    /// Returns an error unless called by the trusted system identity, or when
    /// the name/id already exists or persistence fails.
    pub fn register_type(
        &self,
        context: AccessContext,
        name: &str,
        schema: ValueSchema,
        creation: CreationPolicy,
        domain_capabilities: BTreeSet<String>,
    ) -> Result<TypeDescriptor, OmsError> {
        let (descriptor, request) =
            self.prepare_register_type(context, name, schema, creation, domain_capabilities)?;
        let mut transaction = self.begin(context);
        transaction.create(request);
        self.commit(transaction)?;
        Ok(descriptor)
    }

    /// Validates a dynamic Type registration without committing it, allowing
    /// the descriptor to join a larger atomic transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for an untrusted caller, invalid/duplicate name, or
    /// an encoding failure.
    pub fn prepare_register_type(
        &self,
        context: AccessContext,
        name: &str,
        schema: ValueSchema,
        creation: CreationPolicy,
        domain_capabilities: BTreeSet<String>,
    ) -> Result<(TypeDescriptor, CreateObject), OmsError> {
        if context.subject != SYSTEM_SUBJECT {
            return Err(OmsError::InvalidOperation(
                "only the system subject can register Types",
            ));
        }
        if name.is_empty() || name.contains('\0') || self.type_by_name(name).is_ok() {
            return Err(OmsError::InvalidOperation(
                "Type name is invalid or already exists",
            ));
        }
        let descriptor = TypeDescriptor {
            id: TypeId::new(),
            name: name.to_owned(),
            schema,
            creation,
            capabilities: all_capabilities(),
            domain_capabilities,
        };
        let state = encode_type_descriptor(&descriptor)?;
        Ok((descriptor, CreateObject::new(TYPE_DESCRIPTOR_TYPE, state)))
    }

    fn dynamic_types(&self) -> Result<Vec<TypeDescriptor>, OmsError> {
        let mut descriptors = Vec::new();
        for shard in &self.shards {
            let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
            if let Some(objects) = state.by_type.get(&TYPE_DESCRIPTOR_TYPE) {
                for object in objects {
                    let record = state
                        .objects
                        .get(object)
                        .ok_or(OmsError::NotFound(*object))?;
                    if record.header.lifecycle != LifecycleState::Tombstoned {
                        descriptors.push(decode_type_descriptor(&record.state)?);
                    }
                }
            }
        }
        Ok(descriptors)
    }

    /// Validates a public typed creation without committing it. The caller may
    /// stage the returned request alongside other changes in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or provider-only type, a schema mismatch
    /// or an invalid Value.
    pub fn prepare_create(&self, spec: CreateSpec) -> Result<CreateObject, OmsError> {
        let descriptor = self.type_by_name(&spec.type_name)?;
        if descriptor.creation != CreationPolicy::Public {
            return Err(OmsError::TypeCreationDenied(descriptor.id));
        }
        let value = normalize_value(descriptor.schema, &spec.value);
        if !descriptor.schema.accepts(&value) {
            return Err(OmsError::ValueSchemaMismatch {
                type_id: descriptor.id,
                expected: descriptor.schema.name(),
                actual: value.kind(),
            });
        }
        let encoded = value
            .encode()
            .map_err(|error| OmsError::InvalidValue(error.to_string()))?;
        let mut request = CreateObject::new(descriptor.id, encoded);
        request.parent = spec.parent;
        request.links = spec.links;
        request.capabilities.clone_from(&descriptor.capabilities);
        Ok(request)
    }

    /// Creates a typed Value Object using capabilities supplied by its type.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or provider-only type, a schema mismatch,
    /// an invalid Value, or a failed atomic commit.
    pub fn create_object(
        &self,
        context: AccessContext,
        spec: CreateSpec,
    ) -> Result<ObjectId, OmsError> {
        let request = self.prepare_create(spec)?;
        let id = request.id;
        let mut transaction = self.begin(context);
        if let Some(parent) = request.parent {
            let version = self.inspect(context, parent)?.version;
            transaction.expect(parent, version);
        }
        transaction.create(request);
        self.commit(transaction)?;
        Ok(id)
    }

    /// Finds and returns the current immutable Object view.
    ///
    /// # Errors
    ///
    /// Returns an error when lookup or `ViewValue` authorization fails.
    pub fn find(&self, context: AccessContext, object: ObjectId) -> Result<ObjectView, OmsError> {
        self.read(context, object)
    }

    /// Decodes the current Object Value.
    ///
    /// # Errors
    ///
    /// Returns an error for denied access or a non-Value legacy state.
    pub fn value(&self, context: AccessContext, object: ObjectId) -> Result<Value, OmsError> {
        let view = self.read(context, object)?;
        Value::decode(view.state()).map_err(|error| OmsError::InvalidValue(error.to_string()))
    }

    /// Atomically replaces a typed Object Value using optimistic versioning.
    ///
    /// # Errors
    ///
    /// Returns an error for type mismatch, denied access or commit failure.
    pub fn replace_value(
        &self,
        context: AccessContext,
        object: ObjectId,
        value: &Value,
    ) -> Result<ObjectVersion, OmsError> {
        let (version, encoded) = self.prepare_replace_value(context, object, value)?;
        let mut transaction = self.begin(context);
        transaction
            .expect(object, version)
            .update_state(object, encoded);
        let result = self.commit(transaction)?;
        result
            .versions
            .get(&object)
            .copied()
            .ok_or(OmsError::InvalidOperation("commit omitted replaced object"))
    }

    /// Validates a public Value replacement without committing it, so callers
    /// can stage it with Process state in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for denied access, a provider-owned type, a schema
    /// mismatch, or an invalid Value.
    pub fn prepare_replace_value(
        &self,
        context: AccessContext,
        object: ObjectId,
        value: &Value,
    ) -> Result<(ObjectVersion, Vec<u8>), OmsError> {
        let view = self.read(context, object)?;
        let descriptor = self.type_by_id(view.header().type_id)?;
        if descriptor.creation != CreationPolicy::Public {
            return Err(OmsError::TypeCreationDenied(descriptor.id));
        }
        let normalized = normalize_value(descriptor.schema, value);
        if !descriptor.schema.accepts(&normalized) {
            return Err(OmsError::ValueSchemaMismatch {
                type_id: descriptor.id,
                expected: descriptor.schema.name(),
                actual: normalized.kind(),
            });
        }
        let encoded = normalized
            .encode()
            .map_err(|error| OmsError::InvalidValue(error.to_string()))?;
        Ok((view.header().version, encoded))
    }

    /// Queries visible Objects using indexed type lookup when available.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::TemporarilyUnavailable`] if a shard lock is poisoned.
    pub fn query(
        &self,
        context: AccessContext,
        query: &ObjectQuery,
    ) -> Result<Vec<ObjectHeader>, OmsError> {
        let mut matches = Vec::new();
        let descriptors = self.types()?;
        for shard in &self.shards {
            let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
            let candidates: Box<dyn Iterator<Item = &ObjectRecord> + '_> =
                if let Some(type_id) = query.type_id {
                    Box::new(
                        state
                            .by_type
                            .get(&type_id)
                            .into_iter()
                            .flatten()
                            .filter_map(|id| state.objects.get(id)),
                    )
                } else {
                    Box::new(state.objects.values())
                };
            for record in candidates {
                if record.header.lifecycle == LifecycleState::Tombstoned
                    || query
                        .parent
                        .is_some_and(|parent| record.header.parent_id != Some(parent))
                    || !record.policy.allows(context.subject, Capability::Inspect)
                    || !record.capabilities.contains(&Capability::Inspect)
                    || query.capability.is_some_and(|capability| {
                        !record.capabilities.contains(&capability)
                            || !record.policy.allows(context.subject, capability)
                    })
                {
                    continue;
                }
                if let Some(capability) = &query.domain_capability {
                    let descriptor = descriptors
                        .iter()
                        .find(|descriptor| descriptor.id == record.header.type_id)
                        .ok_or(OmsError::UnknownType(record.header.type_id))?;
                    if !descriptor.domain_capabilities.contains(capability)
                        || !record.capabilities.contains(&Capability::Invoke)
                        || !record.policy.allows(context.subject, Capability::Invoke)
                    {
                        continue;
                    }
                }
                matches.push(record.header.clone());
            }
        }
        matches.sort_by_key(|header| header.id);
        Ok(matches)
    }

    /// Atomically binds a name in a Namespace Object to any Object.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or duplicate name, wrong Object type,
    /// denied access, missing target or failed commit.
    pub fn bind_name(
        &self,
        context: AccessContext,
        namespace: ObjectId,
        name: &str,
        target: ObjectId,
    ) -> Result<ObjectVersion, OmsError> {
        validate_name(name)?;
        let view = self.read(context, namespace)?;
        if view.header().type_id != CORE_NAMESPACE_TYPE {
            return Err(OmsError::InvalidOperation(
                "names can only be bound in a core.namespace Object",
            ));
        }
        if view.links().contains_key(name) {
            return Err(OmsError::InvalidOperation("namespace name already exists"));
        }
        let mut transaction = self.begin(context);
        transaction
            .expect(namespace, view.header().version)
            .set_link(namespace, name, target);
        let result = self.commit(transaction)?;
        committed_version(&result, namespace)
    }

    /// Atomically removes a name from a Namespace Object.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing name, wrong Object type, denied access or
    /// failed commit.
    pub fn unbind_name(
        &self,
        context: AccessContext,
        namespace: ObjectId,
        name: &str,
    ) -> Result<ObjectVersion, OmsError> {
        validate_name(name)?;
        let view = self.read(context, namespace)?;
        if view.header().type_id != CORE_NAMESPACE_TYPE {
            return Err(OmsError::InvalidOperation(
                "names can only be removed from a core.namespace Object",
            ));
        }
        if !view.links().contains_key(name) {
            return Err(OmsError::NameNotFound {
                namespace,
                name: name.to_owned(),
            });
        }
        let mut transaction = self.begin(context);
        transaction
            .expect(namespace, view.header().version)
            .remove_link(namespace, name);
        let result = self.commit(transaction)?;
        committed_version(&result, namespace)
    }

    /// Resolves a slash-separated Namespace path to a stable `ObjectId`.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid components, missing names, non-Namespace
    /// intermediate Objects or denied visibility.
    pub fn resolve(
        &self,
        context: AccessContext,
        root: ObjectId,
        path: &str,
    ) -> Result<ObjectId, OmsError> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            self.inspect(context, root)?;
            return Ok(root);
        }
        let mut current = root;
        for name in trimmed.split('/') {
            validate_name(name)?;
            let view = self.read(context, current)?;
            if view.header().type_id != CORE_NAMESPACE_TYPE {
                return Err(OmsError::InvalidOperation(
                    "path component is not a core.namespace Object",
                ));
            }
            current = view
                .links()
                .get(name)
                .copied()
                .ok_or_else(|| OmsError::NameNotFound {
                    namespace: current,
                    name: name.to_owned(),
                })?;
        }
        self.inspect(context, current)?;
        Ok(current)
    }

    #[must_use]
    pub fn shard_for(&self, object: ObjectId) -> ShardId {
        self.directory.locate(object)
    }

    /// Returns an immutable view of the latest published object version.
    ///
    /// # Errors
    ///
    /// Returns an error when the object is missing, unavailable, tombstoned,
    /// or the caller lacks the `ViewValue` capability.
    pub fn read(&self, context: AccessContext, object: ObjectId) -> Result<ObjectView, OmsError> {
        let shard = self.shard(object);
        let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
        let record = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?;
        record.require(context, Capability::ViewValue)?;
        Ok(record.view())
    }

    /// Reads a tombstoned Object's retained payload for trusted recovery work.
    /// The Object must still be inside its seven-day retention window.
    ///
    /// # Errors
    ///
    /// Returns an error unless the caller is the system Subject, the Object is
    /// tombstoned, and its payload has not yet been compacted.
    pub fn read_retained(
        &self,
        context: AccessContext,
        object: ObjectId,
    ) -> Result<ObjectView, OmsError> {
        let shard = self.shard(object);
        let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
        let record = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?;
        if context.subject != SYSTEM_SUBJECT {
            return Err(OmsError::Denied {
                object,
                capability: Capability::ViewValue,
            });
        }
        if record.header.lifecycle != LifecycleState::Tombstoned {
            return Err(OmsError::InvalidLifecycle {
                object,
                state: record.header.lifecycle,
            });
        }
        let now = unix_time_millis()?;
        let retained_until = record
            .retired_at_unix_ms
            .unwrap_or_default()
            .saturating_add(TOMBSTONE_RETENTION_MILLIS);
        if record.state.is_empty() || now >= retained_until {
            return Err(OmsError::InvalidOperation(
                "retired Object payload is outside its retention window",
            ));
        }
        Ok(record.view())
    }

    /// Checks that the caller may use a specific Object capability.
    ///
    /// # Errors
    ///
    /// Returns an error if the Object is absent, unavailable, inactive or denied.
    pub fn require_capability(
        &self,
        context: AccessContext,
        object: ObjectId,
        capability: Capability,
    ) -> Result<(), OmsError> {
        let shard = self.shard(object);
        let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
        let record = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?;
        record.require(context, capability)
    }

    /// Returns object metadata after checking the `Inspect` capability.
    ///
    /// # Errors
    ///
    /// Returns an error when the object is missing, unavailable, tombstoned,
    /// or the caller lacks the `Inspect` capability.
    pub fn inspect(
        &self,
        context: AccessContext,
        object: ObjectId,
    ) -> Result<ObjectHeader, OmsError> {
        let shard = self.shard(object);
        let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
        let record = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?;
        record.require(context, Capability::Inspect)?;
        Ok(record.header.clone())
    }

    /// Lists object headers for which the caller has the `Inspect` capability.
    /// Tombstoned objects remain inspectable but cannot be read or modified.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::TemporarilyUnavailable`] if a shard lock is poisoned.
    pub fn list(&self, context: AccessContext) -> Result<Vec<ObjectHeader>, OmsError> {
        let mut objects = Vec::new();
        for shard in &self.shards {
            let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
            objects.extend(
                state
                    .objects
                    .values()
                    .filter(|record| {
                        record.capabilities.contains(&Capability::Inspect)
                            && record.policy.allows(context.subject, Capability::Inspect)
                    })
                    .map(|record| record.header.clone()),
            );
        }
        objects.sort_by_key(|header| header.id);
        Ok(objects)
    }

    /// Returns aggregate in-memory object counts.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::TemporarilyUnavailable`] if a shard lock is poisoned.
    pub fn stats(&self) -> Result<OmsStats, OmsError> {
        let mut object_count = 0;
        let mut tombstoned_count = 0;
        for shard in &self.shards {
            let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
            object_count += state.objects.len();
            tombstoned_count += state
                .objects
                .values()
                .filter(|record| record.header.lifecycle == LifecycleState::Tombstoned)
                .count();
        }
        Ok(OmsStats {
            shard_count: self.directory.shard_count(),
            object_count,
            active_count: object_count - tombstoned_count,
            tombstoned_count,
        })
    }

    /// Returns inexpensive process-local transaction counters and latency
    /// estimates. Histogram percentiles are rounded up to a power-of-two ns.
    #[must_use]
    pub fn performance_stats(&self) -> OmsPerformanceStats {
        let counters = &self.performance;
        let batches = counters.commit_batches.load(Ordering::Relaxed);
        OmsPerformanceStats {
            commit_batches: batches,
            transactions: counters.transactions.load(Ordering::Relaxed),
            persisted_batches: counters.persisted_batches.load(Ordering::Relaxed),
            delta_records: counters.delta_records.load(Ordering::Relaxed),
            delta_bytes: counters.delta_bytes.load(Ordering::Relaxed),
            full_snapshot_encodes: counters.full_snapshot_encodes.load(Ordering::Relaxed),
            full_snapshot_bytes: counters.full_snapshot_bytes.load(Ordering::Relaxed),
            total_commit_nanos: counters.total_commit_nanos.load(Ordering::Relaxed),
            commit_p50_nanos: counters.percentile(50, batches),
            commit_p95_nanos: counters.percentile(95, batches),
            commit_p99_nanos: counters.percentile(99, batches),
        }
    }

    /// Analyzes every Object without modifying memory, WAL or checkpoints.
    /// Only Tombstones at least seven days old are reclaimable.
    ///
    /// # Errors
    ///
    /// Returns an error if shard state is unavailable, inconsistent or cannot
    /// be encoded, or if backend size metadata cannot be read.
    pub fn analyze_gc(&self) -> Result<GcAnalysis, OmsError> {
        self.analyze_gc_at(SystemTime::now())
    }

    /// Estimates cleanup as of `now`; the supplied time is primarily useful to
    /// the kernel scheduler and deterministic retention-policy tests.
    ///
    /// # Errors
    ///
    /// Returns an error if state, time conversion, or storage metadata is
    /// unavailable.
    pub fn analyze_gc_at(&self, now: SystemTime) -> Result<GcAnalysis, OmsError> {
        let cutoff_unix_ms = retention_cutoff_unix_ms(system_time_to_unix_millis(now)?);
        let shards = self
            .shards
            .iter()
            .map(|shard| shard.read().map_err(|_| OmsError::TemporarilyUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        let current = combine_shards(shards.iter().map(|shard| &**shard))?;
        let mut compacted = current.clone();
        let counts = compact_tombstone_payloads(&mut compacted, cutoff_unix_ms);
        validate_gc_candidate(&compacted, &self.types)?;
        let usage = self
            .persistence
            .as_ref()
            .map_or(Ok(StorageUsage::default()), |backend| {
                backend.storage_usage()
            })?;
        Ok(GcAnalysis {
            objects_scanned: counts.objects_scanned,
            active_objects: counts.active_objects,
            tombstones: counts.tombstones,
            tombstones_waiting_for_retention: counts.tombstones_waiting_for_retention,
            objects_compactable: counts.objects_compacted,
            live_payload_bytes: counts.live_payload_bytes,
            dead_payload_bytes: counts.dead_payload_bytes_before,
            payload_bytes_reclaimable: counts
                .dead_payload_bytes_before
                .saturating_sub(counts.dead_payload_bytes_after),
            store_bytes_before: usage.store_bytes,
            wal_bytes_before: usage.wal_bytes,
            estimated_store_bytes_after: u64::try_from(encode_snapshot(&compacted)?.len())
                .map_err(|_| OmsError::Storage("compacted snapshot is too large".to_owned()))?,
        })
    }

    /// Exclusively compacts every eligible Tombstone to its minimal form.
    ///
    /// This is a stop-the-world maintenance operation for this manager. All
    /// shard write locks remain held until the compacted image is durable and
    /// every shard publishes the same candidate. Active Objects are copied
    /// byte-for-byte and `ObjectId`s are never removed or reused.
    ///
    /// # Errors
    ///
    /// Returns an error if locking, validation, encoding or durable generation
    /// switching fails. Before the durable switch, the old state remains live.
    pub fn compact(&self) -> Result<GcReport, OmsError> {
        self.compact_expired_tombstones_at(SystemTime::now())
    }

    /// Compacts Tombstones that have reached the seven-day retention age as
    /// of `now`. The kernel maintenance worker runs this automatically.
    ///
    /// # Errors
    ///
    /// Returns an error if locking, validation, encoding or durable generation
    /// switching fails. Before the durable switch, the old state remains live.
    pub fn compact_expired_tombstones_at(&self, now: SystemTime) -> Result<GcReport, OmsError> {
        let cutoff_unix_ms = retention_cutoff_unix_ms(system_time_to_unix_millis(now)?);
        let started = Instant::now();
        let mut locked = self
            .shards
            .iter()
            .map(|shard| shard.write().map_err(|_| OmsError::TemporarilyUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        let current = combine_shards(locked.iter().map(|shard| &**shard))?;
        let mut candidate = current.clone();
        let counts = compact_tombstone_payloads(&mut candidate, cutoff_unix_ms);
        let next_reap_deadline = next_tombstone_reap_deadline(&candidate);
        validate_gc_candidate(&candidate, &self.types)?;
        let usage_before = self
            .persistence
            .as_ref()
            .map_or(Ok(StorageUsage::default()), |backend| {
                backend.storage_usage()
            })?;

        if counts.objects_compacted != 0 {
            let snapshot = encode_snapshot(&candidate)?;
            if let Some(backend) = &self.persistence {
                backend.compact(&snapshot)?;
            }
            let partitioned = partition_shards(candidate, &self.directory);
            for (target, state) in locked.iter_mut().zip(partitioned) {
                **target = state;
            }
        }

        self.next_tombstone_reap_unix_ms
            .store(next_reap_deadline, Ordering::Release);

        let usage_after = self
            .persistence
            .as_ref()
            .map_or(Ok(StorageUsage::default()), |backend| {
                backend.storage_usage()
            })?;
        let before_total = usage_before
            .store_bytes
            .saturating_add(usage_before.wal_bytes);
        let after_total = usage_after
            .store_bytes
            .saturating_add(usage_after.wal_bytes);
        Ok(GcReport {
            objects_scanned: counts.objects_scanned,
            active_objects: counts.active_objects,
            tombstones: counts.tombstones,
            tombstones_waiting_for_retention: counts.tombstones_waiting_for_retention,
            objects_compacted: counts.objects_compacted,
            live_payload_bytes: counts.live_payload_bytes,
            dead_payload_bytes: counts.dead_payload_bytes_before,
            payload_bytes_reclaimed: counts
                .dead_payload_bytes_before
                .saturating_sub(counts.dead_payload_bytes_after),
            store_bytes_before: usage_before.store_bytes,
            store_bytes_after: usage_after.store_bytes,
            wal_bytes_before: usage_before.wal_bytes,
            wal_bytes_after: usage_after.wal_bytes,
            bytes_reclaimed: before_total.saturating_sub(after_total),
            gc_duration_millis: started.elapsed().as_millis(),
        })
    }

    /// Performs one kernel cleanup sweep using the current wall-clock time.
    ///
    /// # Errors
    ///
    /// Returns an error if the system clock, store, or durable backend fails.
    pub fn reap_expired_tombstones(&self) -> Result<GcReport, OmsError> {
        let now_unix_ms = unix_time_millis()?;
        if self.next_tombstone_reap_unix_ms.load(Ordering::Acquire) > now_unix_ms {
            return Ok(GcReport::default());
        }
        self.compact_expired_tombstones_at(SystemTime::now())
    }

    /// Validates all first-stage in-memory invariants.
    ///
    /// # Errors
    ///
    /// Returns an OMS error when a shard lock is poisoned or an invariant is broken.
    pub fn health_check(&self) -> Result<(), OmsError> {
        let shards = self
            .shards
            .iter()
            .map(|shard| shard.read().map_err(|_| OmsError::TemporarilyUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        let combined = combine_shards(shards.iter().map(|shard| &**shard))?;
        validate_parent_graph(&combined)?;
        validate_type_index(&combined)?;
        validate_dynamic_types(&combined, &self.types)?;
        Ok(())
    }

    /// Writes a verified durable checkpoint of the current committed state.
    /// In-memory stores have nothing to checkpoint and return successfully.
    ///
    /// # Errors
    ///
    /// Returns an error when shard state cannot be read/encoded or the
    /// persistence backend cannot durably replace its checkpoint.
    pub fn checkpoint(&self) -> Result<(), OmsError> {
        let Some(backend) = &self.persistence else {
            return Ok(());
        };
        let shards = self
            .shards
            .iter()
            .map(|shard| shard.read().map_err(|_| OmsError::TemporarilyUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        let combined = combine_shards(shards.iter().map(|shard| &**shard))?;
        backend.checkpoint(&encode_snapshot(&combined)?)
    }

    #[must_use]
    pub fn begin(&self, context: AccessContext) -> Transaction {
        Transaction::new(context)
    }

    /// Atomically publishes a validated transaction across all affected shards.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty transaction, stale object versions,
    /// invalid relationships or lifecycle changes, denied capabilities,
    /// missing objects, persistence failure, or an unavailable shard lock.
    pub fn commit(&self, transaction: Transaction) -> Result<CommitResult, OmsError> {
        self.commit_batch(vec![transaction])?
            .pop()
            .ok_or(OmsError::InvalidOperation("transaction batch is empty"))
    }

    /// Validates and publishes several transactions with one durable backend
    /// write/flush. Transactions are applied in input order; if any one fails,
    /// none of the batch becomes visible or durable.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty batch/transaction, conflicts, invalid
    /// operations, denied access, persistence failure, or poisoned locks.
    pub fn commit_batch(
        &self,
        transactions: Vec<Transaction>,
    ) -> Result<Vec<CommitResult>, OmsError> {
        let _metrics = CommitMetricsGuard::new(&self.performance, transactions.len());
        if transactions.is_empty() {
            return Err(OmsError::InvalidOperation("transaction batch is empty"));
        }
        if transactions
            .iter()
            .any(|transaction| transaction.operations.is_empty())
        {
            return Err(OmsError::InvalidOperation(
                "transaction contains no operations",
            ));
        }

        let (write_shards, mut read_shards) =
            transaction_lock_plan(&transactions, &self.directory, self.shards.len());
        if transaction_changes_dynamic_types(self, &transactions)? {
            read_shards.extend(0..self.shards.len());
        }
        if self
            .persistence
            .as_ref()
            .is_some_and(|backend| !backend.supports_delta_records())
        {
            // Compatibility SnapshotBackend implementations require a full
            // image. Preserve their atomic semantics by taking a read lock on
            // every otherwise untouched shard.
            read_shards.extend(0..self.shards.len());
        }
        read_shards.retain(|index| !write_shards.contains(index));

        // Acquire only touched shards, in stable global order. This keeps
        // unrelated Process/Object updates concurrent while still locking
        // every object that validation reads.
        let lock_shards = write_shards
            .union(&read_shards)
            .copied()
            .collect::<BTreeSet<_>>();
        let mut locked = lock_shards
            .into_iter()
            .map(|index| {
                let shard = &self.shards[index];
                let guard = if write_shards.contains(&index) {
                    shard
                        .write()
                        .map(LockedShard::Write)
                        .map_err(|_| OmsError::TemporarilyUnavailable)?
                } else {
                    shard
                        .read()
                        .map(LockedShard::Read)
                        .map_err(|_| OmsError::TemporarilyUnavailable)?
                };
                Ok((index, guard))
            })
            .collect::<Result<Vec<_>, OmsError>>()?;
        let mut candidate = if locked.len() == 1 {
            locked[0].1.state().clone()
        } else {
            combine_shards(locked.iter().map(|(_, shard)| shard.state()))?
        };

        let mut results = Vec::with_capacity(transactions.len());
        let mut new_tombstone_deadlines = Vec::new();
        let mut batch_changed = BTreeSet::new();
        for transaction in transactions {
            validate_expected(&candidate, &transaction.expected)?;
            let mut changed = BTreeSet::new();
            let mut created = BTreeSet::new();
            let mut relationships_changed: BTreeSet<(ObjectId, ObjectId, bool)> =
                BTreeSet::new();
            let mut cycle_starts = BTreeSet::new();

            for operation in transaction.operations {
                apply_operation(
                    &mut candidate,
                    transaction.context,
                    operation,
                    &transaction.expected,
                    &mut changed,
                    &mut created,
                    &mut relationships_changed,
                    &mut cycle_starts,
                )?;
            }

            validate_parent_changes(&candidate, &relationships_changed, &cycle_starts)?;
            validate_created_type_index(&candidate, &created)?;
            if changed.iter().any(|object| {
                candidate
                    .objects
                    .get(object)
                    .is_some_and(|record| record.header.type_id == TYPE_DESCRIPTOR_TYPE)
            }) {
                validate_dynamic_types(&candidate, &self.types)?;
            }

            let mut versions = BTreeMap::new();
            for &object in &changed {
                let record = candidate
                    .objects
                    .get_mut(&object)
                    .ok_or(OmsError::NotFound(object))?;
                if let Some(deadline) = tombstone_reap_deadline(record) {
                    new_tombstone_deadlines.push(deadline);
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

        if let Some(backend) = &self.persistence {
            if backend.supports_delta_records() {
                let delta = encode_snapshot_delta(&candidate, &batch_changed)?;
                backend.store_delta(&delta)?;
                self.performance
                    .delta_records
                    .fetch_add(1, Ordering::Relaxed);
                self.performance.delta_bytes.fetch_add(
                    u64::try_from(delta.len()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
            } else {
                let snapshot = encode_snapshot(&candidate)?;
                self.performance
                    .full_snapshot_encodes
                    .fetch_add(1, Ordering::Relaxed);
                self.performance.full_snapshot_bytes.fetch_add(
                    u64::try_from(snapshot.len()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                backend.store(&snapshot)?;
            }
            self.performance
                .persisted_batches
                .fetch_add(1, Ordering::Relaxed);
        }

        if locked.len() == 1 {
            if let LockedShard::Write(target) = &mut locked[0].1 {
                **target = candidate;
            } else {
                return Err(OmsError::InvalidOperation(
                    "transaction did not acquire its target shard for writing",
                ));
            }
        } else {
            let mut partitioned = write_shards
                .iter()
                .map(|&index| (index, ShardState::default()))
                .collect::<BTreeMap<_, _>>();
            for (object, record) in candidate.objects {
                let index = self.directory.locate(object).get() as usize;
                if let Some(state) = partitioned.get_mut(&index) {
                    state
                        .by_type
                        .entry(record.header.type_id)
                        .or_default()
                        .insert(object);
                    state.objects.insert(object, record);
                }
            }
            for (index, guard) in &mut locked {
                if let (Some(state), LockedShard::Write(target)) =
                    (partitioned.remove(index), guard)
                {
                    **target = state;
                }
            }
        }
        for deadline in &new_tombstone_deadlines {
            self.next_tombstone_reap_unix_ms
                .fetch_min(*deadline, Ordering::AcqRel);
        }
        if !new_tombstone_deadlines.is_empty() {
            self.wake_tombstone_reaper();
        }
        // All write guards remain held until every shard contains its new
        // state, so readers see either the old global state or the new one.

        Ok(results)
    }

    fn shard(&self, object: ObjectId) -> &RwLock<ShardState> {
        &self.shards[self.directory.locate(object).get() as usize]
    }

    fn tombstone_reaper_wait(&self) -> Option<Duration> {
        let deadline = self.next_tombstone_reap_unix_ms.load(Ordering::Acquire);
        if deadline == u64::MAX {
            return None;
        }
        let Ok(now) = unix_time_millis() else {
            return Some(TOMBSTONE_REAPER_RETRY);
        };
        Some(Duration::from_millis(deadline.saturating_sub(now)))
    }

    fn wake_tombstone_reaper(&self) {
        if let Ok(active) = self.reaper_wakeup.lock() {
            if let Some(wakeup) = active.as_ref() {
                let _ = wakeup.send(());
            }
        }
    }
}

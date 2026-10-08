#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitResult {
    pub transaction_id: TransactionId,
    pub versions: BTreeMap<ObjectId, ObjectVersion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OmsStats {
    pub shard_count: u32,
    pub object_count: usize,
    pub active_count: usize,
    pub tombstoned_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OmsPerformanceStats {
    pub commit_batches: u64,
    pub transactions: u64,
    pub persisted_batches: u64,
    pub delta_records: u64,
    pub delta_bytes: u64,
    pub full_snapshot_encodes: u64,
    pub full_snapshot_bytes: u64,
    pub total_commit_nanos: u64,
    pub commit_p50_nanos: u64,
    pub commit_p95_nanos: u64,
    pub commit_p99_nanos: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GcAnalysis {
    pub objects_scanned: u64,
    pub active_objects: u64,
    pub tombstones: u64,
    pub tombstones_waiting_for_retention: u64,
    pub objects_compactable: u64,
    pub live_payload_bytes: u64,
    pub dead_payload_bytes: u64,
    pub payload_bytes_reclaimable: u64,
    pub store_bytes_before: u64,
    pub wal_bytes_before: u64,
    pub estimated_store_bytes_after: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GcReport {
    pub objects_scanned: u64,
    pub active_objects: u64,
    pub tombstones: u64,
    pub tombstones_waiting_for_retention: u64,
    pub objects_compacted: u64,
    pub live_payload_bytes: u64,
    pub dead_payload_bytes: u64,
    pub payload_bytes_reclaimed: u64,
    pub store_bytes_before: u64,
    pub store_bytes_after: u64,
    pub wal_bytes_before: u64,
    pub wal_bytes_after: u64,
    pub bytes_reclaimed: u64,
    pub gc_duration_millis: u128,
}

pub trait ObjectManager {
    /// Reads an Object after validating the caller's capability.
    ///
    /// # Errors
    ///
    /// Returns the same lookup and authorization errors as the Hosted manager.
    fn read(&self, context: AccessContext, object: ObjectId) -> Result<ObjectView, OmsError>;

    /// Reads Object metadata after validating Inspect capability.
    ///
    /// # Errors
    ///
    /// Returns the same lookup and authorization errors as the Hosted manager.
    fn inspect(&self, context: AccessContext, object: ObjectId) -> Result<ObjectHeader, OmsError>;

    /// Starts a transaction under the supplied security subject.
    fn begin(&self, context: AccessContext) -> Transaction;

    /// Atomically validates and publishes the transaction.
    ///
    /// # Errors
    ///
    /// Returns validation, version conflict, or authorization errors.
    fn commit(&self, transaction: Transaction) -> Result<CommitResult, OmsError>;

    /// Lists Object headers visible through Inspect capability.
    ///
    /// # Errors
    ///
    /// Returns an error if any shard cannot be read or access is denied.
    fn list(&self, context: AccessContext) -> Result<Vec<ObjectHeader>, OmsError>;
}

/// Persistence interface shared with Hosted stores. Native currently boots
/// with the in-memory backend and does not claim durable storage.
pub trait SnapshotBackend: core::fmt::Debug {
    /// Loads the latest complete image, if present.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot read its image.
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError>;

    /// Loads a checkpoint and transaction records using the compatibility path.
    ///
    /// # Errors
    ///
    /// Returns an error when recovery data cannot be loaded.
    fn load_recovery(&self) -> Result<SnapshotRecovery, OmsError> {
        Ok(SnapshotRecovery {
            checkpoint: self.load()?,
            updates: Vec::new(),
        })
    }

    /// Stores one complete image.
    ///
    /// # Errors
    ///
    /// Returns an error when the image cannot be stored.
    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError>;
    fn supports_delta_records(&self) -> bool {
        false
    }
    /// Stores one transaction record.
    ///
    /// # Errors
    ///
    /// Returns an error when delta records are unsupported or cannot be stored.
    fn store_delta(&self, _delta: &[u8]) -> Result<(), OmsError> {
        Err(OmsError::InvalidOperation(
            "backend does not support transaction after-image records",
        ))
    }
    /// Replaces the stored checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the checkpoint cannot be stored.
    fn checkpoint(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        self.store(snapshot)
    }
    /// Returns current durable usage statistics.
    ///
    /// # Errors
    ///
    /// Returns an error if usage information is unavailable.
    fn storage_usage(&self) -> Result<StorageUsage, OmsError> {
        Ok(StorageUsage::default())
    }
    /// Compacts the store into a complete image.
    ///
    /// # Errors
    ///
    /// Returns an error when the compacted image cannot be stored.
    fn compact(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        self.checkpoint(snapshot)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageUsage {
    pub store_bytes: u64,
    pub wal_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SnapshotRecovery {
    pub checkpoint: Option<Vec<u8>>,
    pub updates: Vec<Vec<u8>>,
}

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

/// Lightweight process-local commit counters for measuring Object Store cost.
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

/// Read-only estimate of reclaimable Tombstone payload and durable storage.
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

/// Result of one exclusive, type-agnostic Tombstone compaction pass.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageUsage {
    pub store_bytes: u64,
    pub wal_bytes: u64,
}

/// Checkpoint plus atomic after-image records returned during recovery.
#[derive(Debug, Clone, Default)]
pub struct SnapshotRecovery {
    pub checkpoint: Option<Vec<u8>>,
    pub updates: Vec<Vec<u8>>,
}

/// Internal kernel maintenance service that reclaims payloads from Objects
/// retired for at least seven days. It performs a sweep immediately, then
/// waits for the next expiry while the system runtime is active.
#[derive(Debug)]
pub struct TombstoneReaper {
    shutdown: Sender<()>,
    worker: Option<JoinHandle<()>>,
    manager: std::sync::Weak<InMemoryObjectManager>,
}

impl TombstoneReaper {
    /// Starts the background retirement cleanup service.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the kernel cannot start the worker thread.
    pub fn start(manager: &Arc<InMemoryObjectManager>) -> Result<Self, OmsError> {
        let (shutdown, receiver) = mpsc::channel();
        {
            let mut active = manager
                .reaper_wakeup
                .lock()
                .map_err(|_| OmsError::TemporarilyUnavailable)?;
            if active.is_some() {
                return Err(OmsError::InvalidOperation(
                    "a Tombstone reaper is already running",
                ));
            }
            *active = Some(shutdown.clone());
        }
        let manager_weak = Arc::downgrade(manager);
        let worker_manager = Arc::clone(manager);
        let worker = match thread::Builder::new()
            .name("ousject-tombstone-reaper".to_owned())
            .spawn(move || tombstone_reaper_loop(worker_manager, receiver))
        {
            Ok(worker) => worker,
            Err(error) => {
                if let Some(manager) = manager_weak.upgrade() {
                    if let Ok(mut active) = manager.reaper_wakeup.lock() {
                        active.take();
                    }
                }
                return Err(storage_error(error));
            }
        };
        Ok(Self {
            shutdown,
            worker: Some(worker),
            manager: manager_weak,
        })
    }
}

impl Drop for TombstoneReaper {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(manager) = self.manager.upgrade() {
            if let Ok(mut active) = manager.reaper_wakeup.lock() {
                active.take();
            }
        }
    }
}

// These values are deliberately moved into the worker so its lifetime owns
// both the manager lease and the shutdown receiver.
#[allow(clippy::needless_pass_by_value)]
fn tombstone_reaper_loop(manager: Arc<InMemoryObjectManager>, shutdown: Receiver<()>) {
    loop {
        if let Err(error) = manager.reap_expired_tombstones() {
            eprintln!("Ousject tombstone cleanup failed: {error}");
        }
        match manager.tombstone_reaper_wait() {
            None => match shutdown.recv() {
                Ok(()) | Err(_) => return,
            },
            Some(wait) => {
                let wait = if wait.is_zero() {
                    TOMBSTONE_REAPER_RETRY
                } else {
                    wait
                };
                match shutdown.recv_timeout(wait) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }
    }
}

pub trait ObjectManager: Send + Sync {
    /// Reads an object using the caller's access context.
    ///
    /// # Errors
    ///
    /// Returns an OMS error if lookup or authorization fails.
    fn read(&self, context: AccessContext, object: ObjectId) -> Result<ObjectView, OmsError>;

    /// Inspects object metadata using the caller's access context.
    ///
    /// # Errors
    ///
    /// Returns an OMS error if lookup or authorization fails.
    fn inspect(&self, context: AccessContext, object: ObjectId) -> Result<ObjectHeader, OmsError>;

    fn begin(&self, context: AccessContext) -> Transaction;

    /// Atomically commits a transaction.
    ///
    /// # Errors
    ///
    /// Returns an OMS error if validation or publication fails.
    fn commit(&self, transaction: Transaction) -> Result<CommitResult, OmsError>;

    /// Lists objects visible through the `Inspect` capability.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::TemporarilyUnavailable`] if a shard lock is poisoned.
    fn list(&self, context: AccessContext) -> Result<Vec<ObjectHeader>, OmsError>;
}

pub trait SnapshotBackend: Send + Sync + std::fmt::Debug {
    /// Loads the latest durable snapshot, or `None` for an empty store.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the backend cannot read its state.
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError>;

    /// Loads a checkpoint and durable transaction after-images. Backends that
    /// only store full snapshots use the compatibility default.
    ///
    /// # Errors
    ///
    /// Returns a storage error if durable recovery data cannot be read.
    fn load_recovery(&self) -> Result<SnapshotRecovery, OmsError> {
        Ok(SnapshotRecovery {
            checkpoint: self.load()?,
            updates: Vec::new(),
        })
    }

    /// Atomically replaces the durable snapshot.
    ///
    /// # Errors
    ///
    /// Returns a storage error if durability cannot be confirmed.
    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError>;

    /// Whether this backend can durably store structured after-image records.
    fn supports_delta_records(&self) -> bool {
        false
    }

    /// Durably appends one atomic Object Store after-image record.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the record cannot be made durable.
    fn store_delta(&self, _delta: &[u8]) -> Result<(), OmsError> {
        Err(OmsError::InvalidOperation(
            "backend does not support transaction after-image records",
        ))
    }

    /// Forces a durable full checkpoint when the backend supports one.
    ///
    /// # Errors
    ///
    /// Returns a storage error if durability cannot be confirmed.
    fn checkpoint(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        self.store(snapshot)
    }

    /// Returns physical checkpoint and WAL sizes when the backend exposes them.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the backend cannot inspect its durable files.
    fn storage_usage(&self) -> Result<StorageUsage, OmsError> {
        Ok(StorageUsage::default())
    }

    /// Durably switches to a compacted complete image.
    ///
    /// Backends without a specialized generation protocol may use their
    /// ordinary checkpoint operation.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the compacted image cannot be made durable.
    fn compact(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        self.checkpoint(snapshot)
    }
}

#[derive(Debug, Default)]
struct JournalRecords {
    sequence: u64,
    records: Vec<(u64, Vec<u8>)>,
}

#[derive(Debug, Default)]
struct GroupCommitState {
    sender: Option<Sender<GroupCommitRequest>>,
    workers: Vec<JoinHandle<()>>,
}

#[derive(Debug)]
struct GroupCommitRequest {
    delta: Vec<u8>,
    completion: mpsc::SyncSender<Result<(), OmsError>>,
}

#[derive(Debug)]
struct StoreLease {
    path: PathBuf,
    owner: String,
}

#[derive(Debug)]
struct ActiveWal {
    generation: u64,
    file: File,
}

impl Drop for StoreLease {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path).is_ok_and(|contents| contents.trim() == self.owner) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileSnapshotBackend {
    path: PathBuf,
    lease: Arc<Mutex<Option<StoreLease>>>,
    wal_bytes_since_checkpoint: Arc<AtomicU64>,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    latest_state: Arc<Mutex<Option<ShardState>>>,
    journal: Arc<Mutex<JournalRecords>>,
    checkpoint_worker_running: Arc<AtomicBool>,
    group_commit: Arc<Mutex<GroupCommitState>>,
    active_wal: Arc<Mutex<Option<ActiveWal>>>,
    generation_switch: Arc<Mutex<()>>,
    generation: Arc<AtomicU64>,
    requires_reopen: Arc<AtomicBool>,
}

impl FileSnapshotBackend {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            lease: Arc::new(Mutex::new(None)),
            wal_bytes_since_checkpoint: Arc::new(AtomicU64::new(0)),
            latest: Arc::new(Mutex::new(None)),
            latest_state: Arc::new(Mutex::new(None)),
            journal: Arc::new(Mutex::new(JournalRecords::default())),
            checkpoint_worker_running: Arc::new(AtomicBool::new(false)),
            group_commit: Arc::new(Mutex::new(GroupCommitState::default())),
            active_wal: Arc::new(Mutex::new(None)),
            generation_switch: Arc::new(Mutex::new(())),
            generation: Arc::new(AtomicU64::new(0)),
            requires_reopen: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn manifest_path(&self) -> PathBuf {
        self.path.with_extension("manifest")
    }

    fn snapshot_path(&self, generation: u64) -> PathBuf {
        generation_path(&self.path, generation, "oms")
    }

    fn wal_path_for(&self, generation: u64) -> PathBuf {
        generation_path(&self.path, generation, "wal")
    }

    fn active_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn switch_generation(
        &self,
        snapshot: &[u8],
        latest: &mut Option<Vec<u8>>,
    ) -> Result<(), OmsError> {
        self.switch_generation_with_records(snapshot, &[], latest)
    }

    fn switch_generation_with_records(
        &self,
        snapshot: &[u8],
        records: &[Vec<u8>],
        latest: &mut Option<Vec<u8>>,
    ) -> Result<(), OmsError> {
        let old_generation = self.active_generation();
        let new_generation = self.prepare_generation_snapshot(old_generation, snapshot)?;
        self.publish_prepared_generation(old_generation, new_generation, snapshot, records, latest)
    }

    fn prepare_generation_snapshot(
        &self,
        old_generation: u64,
        snapshot: &[u8],
    ) -> Result<u64, OmsError> {
        let new_generation = old_generation
            .checked_add(1)
            .ok_or_else(|| OmsError::Storage("Object Store generation overflow".to_owned()))?;
        // A generation newer than the manifest can only be an interrupted,
        // uncommitted attempt. Replace it before preparing the next switch.
        remove_file_if_exists(&self.wal_path_for(new_generation))?;
        persist_file_snapshot(&self.snapshot_path(new_generation), snapshot)?;
        Ok(new_generation)
    }

    fn publish_prepared_generation(
        &self,
        old_generation: u64,
        new_generation: u64,
        snapshot: &[u8],
        records: &[Vec<u8>],
        latest: &mut Option<Vec<u8>>,
    ) -> Result<(), OmsError> {
        let new_wal = self.wal_path_for(new_generation);
        let wal_bytes = append_wal_records(&new_wal, records)?;
        let new_wal_handle = open_wal_append(&new_wal)?;

        let manifest = encode_generation_manifest(new_generation);
        let manifest_directory_synced =
            persist_generation_manifest(&self.manifest_path(), &manifest)?;

        self.publish_generation(new_generation, snapshot, wal_bytes, latest);
        *self
            .active_wal
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)? = Some(ActiveWal {
            generation: new_generation,
            file: new_wal_handle,
        });
        if !manifest_directory_synced {
            // A rename is visible, but its directory entry could not be
            // confirmed durable. Either manifest may survive a crash; retain
            // both complete generations so either recovery path is valid.
            self.requires_reopen.store(true, Ordering::Release);
            return Ok(());
        }
        // The new manifest and snapshot are durable. Reclaim older generations
        // only after that switch point; cleanup failures leave harmless orphans.
        let _ = remove_file_if_exists(&self.snapshot_path(old_generation));
        let _ = remove_file_if_exists(&self.wal_path_for(old_generation));
        let _ = sync_parent_directory(&self.path);
        Ok(())
    }

    fn publish_generation(
        &self,
        generation: u64,
        snapshot: &[u8],
        wal_bytes: u64,
        latest: &mut Option<Vec<u8>>,
    ) {
        self.generation.store(generation, Ordering::Release);
        latest.replace(snapshot.to_vec());
        self.wal_bytes_since_checkpoint
            .store(wal_bytes, Ordering::Release);
    }

    fn schedule_background_checkpoint(&self) {
        if self
            .checkpoint_worker_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let backend = self.clone();
        if std::thread::Builder::new()
            .name("ousject-store-checkpoint".to_owned())
            .spawn(move || {
                let result = backend.background_checkpoint();
                backend
                    .checkpoint_worker_running
                    .store(false, Ordering::Release);
                if result.is_ok() && backend.checkpoint_due() {
                    backend.schedule_background_checkpoint();
                }
            })
            .is_err()
        {
            self.checkpoint_worker_running
                .store(false, Ordering::Release);
        }
    }

    fn background_checkpoint(&self) -> Result<(), OmsError> {
        loop {
            let _generation_switch = self
                .generation_switch
                .lock()
                .map_err(|_| OmsError::TemporarilyUnavailable)?;
            let generation = self.active_generation();
            let (state, sequence) = {
                let latest_state = self
                    .latest_state
                    .lock()
                    .map_err(|_| OmsError::TemporarilyUnavailable)?;
                let state = latest_state
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| OmsError::Storage("Object Store is not loaded".to_owned()))?;
                let journal = self
                    .journal
                    .lock()
                    .map_err(|_| OmsError::TemporarilyUnavailable)?;
                (state, journal.sequence)
            };
            let snapshot = encode_snapshot(&state)?;
            let new_generation = self.prepare_generation_snapshot(generation, &snapshot)?;

            let _latest_state = self
                .latest_state
                .lock()
                .map_err(|_| OmsError::TemporarilyUnavailable)?;
            let mut latest = self
                .latest
                .lock()
                .map_err(|_| OmsError::TemporarilyUnavailable)?;
            let mut journal = self
                .journal
                .lock()
                .map_err(|_| OmsError::TemporarilyUnavailable)?;
            if generation != self.active_generation() {
                continue;
            }
            let tail = journal
                .records
                .iter()
                .filter(|(record_sequence, _)| *record_sequence > sequence)
                .map(|(_, payload)| payload.clone())
                .collect::<Vec<_>>();
            self.publish_prepared_generation(
                generation,
                new_generation,
                &snapshot,
                &tail,
                &mut latest,
            )?;
            journal
                .records
                .retain(|(record_sequence, _)| *record_sequence > sequence);
            if !self.checkpoint_due_from_snapshot(Some(&snapshot)) {
                return Ok(());
            }
        }
    }

    fn checkpoint_due(&self) -> bool {
        let Ok(latest) = self.latest.lock() else {
            return false;
        };
        self.checkpoint_due_from_snapshot(latest.as_deref())
    }

    fn checkpoint_due_from_snapshot(&self, checkpoint: Option<&[u8]>) -> bool {
        self.wal_bytes_since_checkpoint.load(Ordering::Acquire)
            >= Self::checkpoint_threshold(checkpoint)
    }

    fn checkpoint_threshold(checkpoint: Option<&[u8]>) -> u64 {
        checkpoint.map_or(CHECKPOINT_WAL_BYTES, |snapshot| {
            u64::try_from(snapshot.len())
                .unwrap_or(u64::MAX)
                .saturating_div(4)
                .clamp(1, CHECKPOINT_WAL_BYTES)
        })
    }

    fn queue_delta(&self, delta: &[u8]) -> Result<(), OmsError> {
        const IDLE_WINDOW: Duration = Duration::from_millis(1);
        let (completion, result) = mpsc::sync_channel(1);
        let mut request = Some(GroupCommitRequest {
            delta: delta.to_vec(),
            completion,
        });
        let mut state = self
            .group_commit
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;

        if let Some(sender) = state.sender.as_ref() {
            if let Err(error) = sender.send(request.take().expect("request is present")) {
                state.sender = None;
                request = Some(error.0);
            }
        }
        if state.sender.is_none() {
            let (sender, receiver) = mpsc::channel();
            let backend = self.clone();
            let worker = std::thread::Builder::new()
                .name("ousject-wal-group-commit".to_owned())
                .spawn(move || backend.group_commit_worker(&receiver, IDLE_WINDOW))
                .map_err(|error| OmsError::Storage(error.to_string()))?;
            state.sender = Some(sender.clone());
            state.workers.push(worker);
            if let Err(error) = sender.send(request.take().expect("request is present")) {
                state.sender = None;
                let failed = error.0;
                let _ = failed.completion.send(Err(OmsError::TemporarilyUnavailable));
                return Err(OmsError::TemporarilyUnavailable);
            }
        }
        drop(state);
        let outcome = result
            .recv()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let workers = {
            let mut state = self
                .group_commit
                .lock()
                .map_err(|_| OmsError::TemporarilyUnavailable)?;
            if state.sender.is_none() {
                std::mem::take(&mut state.workers)
            } else {
                Vec::new()
            }
        };
        for worker in workers {
            let _ = worker.join();
        }
        outcome
    }

    fn group_commit_worker(&self, receiver: &Receiver<GroupCommitRequest>, idle_window: Duration) {
        const MAX_RECORDS: usize = 64;
        const MAX_BATCH_BYTES: usize = 1024 * 1024;

        let mut pending = None;
        loop {
            let first = if let Some(request) = pending.take() {
                request
            } else {
                match receiver.recv_timeout(idle_window) {
                    Ok(request) => request,
                    Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {
                        let Ok(mut state) = self.group_commit.lock() else {
                            return;
                        };
                        // Producers use the same mutex while enqueueing.
                        // Recheck under it to avoid stranding a late request.
                        match receiver.try_recv() {
                            Ok(request) => request,
                            Err(
                                mpsc::TryRecvError::Empty
                                | mpsc::TryRecvError::Disconnected,
                            ) => {
                                state.sender = None;
                                return;
                            }
                        }
                    }
                }
            };

            let mut batch = vec![first];
            let mut batch_bytes = batch[0].delta.len();
            let deadline = Instant::now() + idle_window;
            while batch.len() < MAX_RECORDS && batch_bytes < MAX_BATCH_BYTES {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match receiver.recv_timeout(remaining) {
                    Ok(request) => {
                        batch_bytes = batch_bytes.saturating_add(request.delta.len());
                        batch.push(request);
                    }
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
                }
            }

            let deltas = batch
                .iter()
                .map(|request| request.delta.as_slice())
                .collect::<Vec<_>>();
            let outcome = self.persist_delta_group(&deltas);

            let should_exit = {
                let Ok(mut state) = self.group_commit.lock() else {
                    for request in batch {
                        let _ = request
                            .completion
                            .send(Err(OmsError::TemporarilyUnavailable));
                    }
                    return;
                };
                match receiver.try_recv() {
                    Ok(request) => {
                        pending = Some(request);
                        false
                    }
                    Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {
                        state.sender = None;
                        true
                    }
                }
            };
            for request in batch {
                let _ = request.completion.send(outcome.clone());
            }
            if should_exit {
                return;
            }
        }
    }

    fn persist_delta_group(&self, deltas: &[&[u8]]) -> Result<(), OmsError> {
        if self.requires_reopen.load(Ordering::Acquire) {
            return Err(OmsError::Storage(
                "generation switch durability is uncertain; reopen the Object Store".to_owned(),
            ));
        }
        self.ensure_lease()?;
        let payload = encode_group_wal_payload(deltas)?;
        let record_bytes = u64::try_from(payload.len())
            .map_err(|_| OmsError::Storage("WAL record is too large".to_owned()))?
            .saturating_add(20);
        let mut latest_state = self
            .latest_state
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut candidate = latest_state
            .as_ref()
            .cloned()
            .ok_or_else(|| OmsError::Storage("Object Store recovery was not loaded".to_owned()))?;
        for delta in deltas {
            apply_snapshot_delta(&mut candidate, delta)?;
        }
        let latest = self
            .latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut journal = self
            .journal
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        if let Err(error) = self.append_active_wal(&payload) {
            self.requires_reopen.store(true, Ordering::Release);
            return Err(error);
        }
        *latest_state = Some(candidate);
        journal.sequence = journal.sequence.saturating_add(1);
        let sequence = journal.sequence;
        journal.records.push((sequence, payload));
        let pending_bytes = self
            .wal_bytes_since_checkpoint
            .fetch_add(record_bytes, Ordering::Relaxed)
            .saturating_add(record_bytes);
        let checkpoint_threshold = Self::checkpoint_threshold(latest.as_deref());
        drop(journal);
        drop(latest);
        drop(latest_state);
        if pending_bytes >= checkpoint_threshold {
            self.schedule_background_checkpoint();
        }
        Ok(())
    }

    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    fn ensure_lease(&self) -> Result<(), OmsError> {
        let mut lease = self
            .lease
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        if lease.is_some() {
            return Ok(());
        }
        if let Some(parent) = self
            .path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(storage_error)?;
        }
        let path = self.lock_path();
        let owner = format!("{} {}", std::process::id(), ObjectId::new());
        let mut file = match create_lock_file(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if stale_process_lock(&path)? {
                    match fs::remove_file(&path) {
                        Ok(()) => create_lock_file(&path).map_err(|retry| {
                            if retry.kind() == std::io::ErrorKind::AlreadyExists {
                                OmsError::StoreInUse(path.display().to_string())
                            } else {
                                storage_error(retry)
                            }
                        })?,
                        Err(remove) if remove.kind() == std::io::ErrorKind::NotFound => {
                            create_lock_file(&path).map_err(storage_error)?
                        }
                        Err(remove) => return Err(storage_error(remove)),
                    }
                } else {
                    return Err(OmsError::StoreInUse(path.display().to_string()));
                }
            }
            Err(error) => return Err(storage_error(error)),
        };
        writeln!(file, "{owner}").map_err(storage_error)?;
        file.sync_all().map_err(storage_error)?;
        *lease = Some(StoreLease { path, owner });
        Ok(())
    }

    fn append_active_wal(&self, payload: &[u8]) -> Result<(), OmsError> {
        let generation = self.active_generation();
        let mut active_wal = self
            .active_wal
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        if active_wal
            .as_ref()
            .is_none_or(|active| active.generation != generation)
        {
            *active_wal = Some(ActiveWal {
                generation,
                file: open_wal_append(&self.wal_path_for(generation))?,
            });
        }
        let active = active_wal
            .as_mut()
            .ok_or_else(|| OmsError::Storage("active WAL handle is missing".to_owned()))?;
        let needs_directory_sync = active.file.metadata().map_err(storage_error)?.len() == 0;
        append_wal_handle(&mut active.file, payload)?;
        if needs_directory_sync {
            sync_parent_directory(&self.wal_path_for(generation))?;
        }
        Ok(())
    }
}

fn create_lock_file(path: &Path) -> Result<File, std::io::Error> {
    OpenOptions::new().create_new(true).write(true).open(path)
}

fn stale_process_lock(path: &Path) -> Result<bool, OmsError> {
    let contents = fs::read_to_string(path).map_err(storage_error)?;
    let pid = contents
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<u32>().ok());
    let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) else {
        return Ok(true);
    };

    #[cfg(unix)]
    {
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
            Err(nix::errno::Errno::ESRCH) => Ok(true),
            Ok(()) | Err(_) => Ok(false),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        Ok(false)
    }
}

impl SnapshotBackend for FileSnapshotBackend {
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError> {
        let recovery = self.load_recovery()?;
        if recovery.updates.is_empty() {
            return Ok(recovery.checkpoint);
        }
        let mut state = recovery.checkpoint.map_or_else(
            || Ok(ShardState::default()),
            |bytes| decode_snapshot(&bytes),
        )?;
        for update in recovery.updates {
            apply_snapshot_delta(&mut state, &update)?;
        }
        encode_snapshot(&state).map(Some)
    }

    fn load_recovery(&self) -> Result<SnapshotRecovery, OmsError> {
        self.ensure_lease()?;
        let manifest_generation = read_generation_manifest(&self.manifest_path())?;
        let generation = manifest_generation.unwrap_or(0);
        let snapshot_path = self.snapshot_path(generation);
        if manifest_generation.is_some() && !snapshot_path.exists() {
            return Err(corruption(
                "active Object Store generation is missing its checkpoint",
            ));
        }
        let snapshot = if snapshot_path.exists() {
            Some(fs::read(&snapshot_path).map_err(storage_error)?)
        } else {
            None
        };
        let recovered = load_wal_records(&self.wal_path_for(generation), snapshot)?;
        let mut current_state = recovered
            .latest
            .as_deref()
            .map_or_else(|| Ok(ShardState::default()), decode_snapshot)?;
        for update in &recovered.updates {
            apply_snapshot_delta(&mut current_state, update)?;
        }
        self.generation.store(generation, Ordering::Release);
        *self
            .active_wal
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)? = Some(ActiveWal {
            generation,
            file: open_wal_append(&self.wal_path_for(generation))?,
        });
        self.wal_bytes_since_checkpoint.store(
            file_size(&self.wal_path_for(generation))?,
            Ordering::Release,
        );
        self.latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?
            .clone_from(&recovered.latest);
        *self
            .latest_state
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)? = Some(current_state);
        let mut journal = self
            .journal
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        journal.sequence = 0;
        journal.records.clear();
        for payload in recovered.committed_records {
            journal.sequence = journal.sequence.saturating_add(1);
            let sequence = journal.sequence;
            journal.records.push((sequence, payload));
        }
        let orphan_generation = generation.saturating_add(1);
        for candidate in 0..=orphan_generation {
            let _ = remove_file_if_exists(&temporary_path(&self.snapshot_path(candidate)));
            let _ = remove_file_if_exists(&temporary_path(&self.wal_path_for(candidate)));
        }
        let _ = remove_file_if_exists(&temporary_path(&self.manifest_path()));
        for obsolete in 0..generation {
            let _ = remove_file_if_exists(&self.snapshot_path(obsolete));
            let _ = remove_file_if_exists(&self.wal_path_for(obsolete));
        }
        let _ = remove_file_if_exists(&self.snapshot_path(orphan_generation));
        let _ = remove_file_if_exists(&self.wal_path_for(orphan_generation));
        let _ = sync_parent_directory(&self.path);
        Ok(SnapshotRecovery {
            checkpoint: recovered.latest,
            updates: recovered.updates,
        })
    }

    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        if self.requires_reopen.load(Ordering::Acquire) {
            return Err(OmsError::Storage(
                "generation switch durability is uncertain; reopen the Object Store".to_owned(),
            ));
        }
        self.ensure_lease()?;
        let decoded = decode_snapshot(snapshot)?;
        let mut latest_state = self
            .latest_state
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut journal = self
            .journal
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let payload = encode_reset_wal_payload(snapshot)?;
        let record_bytes = u64::try_from(payload.len())
            .map_err(|_| OmsError::Storage("WAL record is too large".to_owned()))?
            .saturating_add(20);
        if let Err(error) = self.append_active_wal(&payload) {
            // A failed append may have written a partial or complete frame.
            // Do not append again until recovery has scanned and repaired it.
            self.requires_reopen.store(true, Ordering::Release);
            return Err(error);
        }
        *latest = Some(snapshot.to_vec());
        *latest_state = Some(decoded);
        journal.sequence = journal.sequence.saturating_add(1);
        let sequence = journal.sequence;
        journal.records.push((sequence, payload));

        let pending_bytes = self
            .wal_bytes_since_checkpoint
            .fetch_add(record_bytes, Ordering::Relaxed);
        let checkpoint_threshold = Self::checkpoint_threshold(latest.as_deref());
        drop(journal);
        drop(latest);
        drop(latest_state);
        if pending_bytes.saturating_add(record_bytes) >= checkpoint_threshold {
            self.schedule_background_checkpoint();
        }
        Ok(())
    }

    fn supports_delta_records(&self) -> bool {
        true
    }

    fn store_delta(&self, delta: &[u8]) -> Result<(), OmsError> {
        if self.requires_reopen.load(Ordering::Acquire) {
            return Err(OmsError::Storage(
                "generation switch durability is uncertain; reopen the Object Store".to_owned(),
            ));
        }
        self.ensure_lease()?;
        self.queue_delta(delta)
    }

    fn checkpoint(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        if self.requires_reopen.load(Ordering::Acquire) {
            return Err(OmsError::Storage(
                "generation switch durability is uncertain; reopen the Object Store".to_owned(),
            ));
        }
        self.ensure_lease()?;
        let decoded = decode_snapshot(snapshot)?;
        let _generation_switch = self
            .generation_switch
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut latest_state = self
            .latest_state
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut journal = self
            .journal
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        // A reset record is a complete independent generation. If power is
        // lost after this flush, recovery can use it with either the old or
        // the newly renamed checkpoint. Only after the new checkpoint and its
        // directory entry are durable may the covered WAL be discarded.
        self.switch_generation(snapshot, &mut latest)?;
        *latest_state = Some(decoded);
        journal.sequence = 0;
        journal.records.clear();
        Ok(())
    }

    fn storage_usage(&self) -> Result<StorageUsage, OmsError> {
        self.ensure_lease()?;
        let generation = self.active_generation();
        let mut usage = StorageUsage::default();
        for candidate in [generation, generation.saturating_add(1)] {
            usage.store_bytes = usage
                .store_bytes
                .saturating_add(file_size(&self.snapshot_path(candidate))?);
            usage.wal_bytes = usage
                .wal_bytes
                .saturating_add(file_size(&self.wal_path_for(candidate))?);
            usage.store_bytes = usage
                .store_bytes
                .saturating_add(file_size(&temporary_path(&self.snapshot_path(candidate)))?);
            usage.wal_bytes = usage
                .wal_bytes
                .saturating_add(file_size(&temporary_path(&self.wal_path_for(candidate)))?);
        }
        usage.store_bytes = usage
            .store_bytes
            .saturating_add(file_size(&self.manifest_path())?)
            .saturating_add(file_size(&temporary_path(&self.manifest_path()))?);
        Ok(usage)
    }

    fn compact(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        if self.requires_reopen.load(Ordering::Acquire) {
            return Err(OmsError::Storage(
                "generation switch durability is uncertain; reopen the Object Store".to_owned(),
            ));
        }
        self.ensure_lease()?;
        let decoded = decode_snapshot(snapshot)?;
        let _generation_switch = self
            .generation_switch
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut latest_state = self
            .latest_state
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let mut journal = self
            .journal
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        self.switch_generation(snapshot, &mut latest)?;
        *latest_state = Some(decoded);
        journal.sequence = 0;
        journal.records.clear();
        Ok(())
    }
}

fn file_size(path: &Path) -> Result<u64, OmsError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(storage_error(error)),
    }
}

fn generation_path(path: &Path, generation: u64, extension: &str) -> PathBuf {
    if generation == 0 {
        if extension == "oms" {
            path.to_path_buf()
        } else {
            path.with_extension(extension)
        }
    } else {
        path.with_extension(format!("{extension}.g{generation}"))
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary_name = path.as_os_str().to_os_string();
    temporary_name.push(".tmp");
    PathBuf::from(temporary_name)
}

fn encode_generation_manifest(generation: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(20);
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.extend_from_slice(&generation.to_le_bytes());
    let checksum = wal_checksum(&bytes);
    bytes.extend_from_slice(&checksum.to_le_bytes());
    bytes
}

fn read_generation_manifest(path: &Path) -> Result<Option<u64>, OmsError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage_error(error)),
    };
    if bytes.len() != 20 || &bytes[..4] != MANIFEST_MAGIC {
        return Err(corruption("invalid Object Store generation manifest"));
    }
    let expected = u64::from_le_bytes(
        bytes[12..20]
            .try_into()
            .map_err(|_| corruption("truncated Object Store manifest checksum"))?,
    );
    if wal_checksum(&bytes[..12]) != expected {
        return Err(corruption(
            "Object Store generation manifest checksum mismatch",
        ));
    }
    Ok(Some(u64::from_le_bytes(bytes[4..12].try_into().map_err(
        |_| corruption("truncated Object Store manifest generation"),
    )?)))
}

fn remove_file_if_exists(path: &Path) -> Result<(), OmsError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_error(error)),
    }
}

fn sync_parent_directory(path: &Path) -> Result<(), OmsError> {
    if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(storage_error)?;
    }
    Ok(())
}

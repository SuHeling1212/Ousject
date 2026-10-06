use im::{OrdMap, OrdSet};

#[derive(Debug, Default, Clone)]
struct ShardState {
    objects: OrdMap<ObjectId, ObjectRecord>,
    by_type: OrdMap<TypeId, OrdSet<ObjectId>>,
}

#[derive(Debug)]
struct PerformanceCounters {
    commit_batches: AtomicU64,
    transactions: AtomicU64,
    persisted_batches: AtomicU64,
    delta_records: AtomicU64,
    delta_bytes: AtomicU64,
    full_snapshot_encodes: AtomicU64,
    full_snapshot_bytes: AtomicU64,
    total_commit_nanos: AtomicU64,
    latency_buckets: [AtomicU64; 64],
}

impl Default for PerformanceCounters {
    fn default() -> Self {
        Self {
            commit_batches: AtomicU64::new(0),
            transactions: AtomicU64::new(0),
            persisted_batches: AtomicU64::new(0),
            delta_records: AtomicU64::new(0),
            delta_bytes: AtomicU64::new(0),
            full_snapshot_encodes: AtomicU64::new(0),
            full_snapshot_bytes: AtomicU64::new(0),
            total_commit_nanos: AtomicU64::new(0),
            latency_buckets: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl PerformanceCounters {
    fn record_batch(&self, transaction_count: u64, elapsed: Duration) {
        self.commit_batches.fetch_add(1, Ordering::Relaxed);
        self.transactions
            .fetch_add(transaction_count, Ordering::Relaxed);
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        self.total_commit_nanos.fetch_add(nanos, Ordering::Relaxed);
        let bucket = nanos.max(1).ilog2() as usize;
        self.latency_buckets[bucket.min(63)].fetch_add(1, Ordering::Relaxed);
    }

    fn percentile(&self, percentile: u64, samples: u64) -> u64 {
        if samples == 0 {
            return 0;
        }
        let rank = samples
            .saturating_mul(percentile)
            .saturating_add(99)
            .saturating_div(100);
        let mut seen = 0_u64;
        for (bucket, count) in self.latency_buckets.iter().enumerate() {
            seen = seen.saturating_add(count.load(Ordering::Relaxed));
            if seen >= rank {
                return 1_u64.checked_shl(bucket as u32).unwrap_or(u64::MAX);
            }
        }
        u64::MAX
    }
}

struct CommitMetricsGuard<'a> {
    counters: &'a PerformanceCounters,
    transaction_count: u64,
    started: Instant,
}

impl<'a> CommitMetricsGuard<'a> {
    fn new(counters: &'a PerformanceCounters, transaction_count: usize) -> Self {
        Self {
            counters,
            transaction_count: u64::try_from(transaction_count).unwrap_or(u64::MAX),
            started: Instant::now(),
        }
    }
}

impl Drop for CommitMetricsGuard<'_> {
    fn drop(&mut self) {
        self.counters
            .record_batch(self.transaction_count, self.started.elapsed());
    }
}

enum LockedShard<'a> {
    Read(RwLockReadGuard<'a, ShardState>),
    Write(RwLockWriteGuard<'a, ShardState>),
}

impl LockedShard<'_> {
    fn state(&self) -> &ShardState {
        match self {
            Self::Read(state) => state,
            Self::Write(state) => state,
        }
    }
}

#[derive(Debug)]
pub struct InMemoryObjectManager {
    directory: FixedDirectory,
    shards: Vec<RwLock<ShardState>>,
    persistence: Option<Arc<dyn SnapshotBackend>>,
    types: TypeRegistry,
    next_tombstone_reap_unix_ms: AtomicU64,
    reaper_wakeup: Mutex<Option<Sender<()>>>,
    performance: PerformanceCounters,
}

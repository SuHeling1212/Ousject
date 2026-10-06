use oms_runtime::{
    AccessContext, CreateObject, FileSnapshotBackend, InMemoryObjectManager, SnapshotBackend,
};
use oms_types::{ObjectId, OmsError, SubjectId, TypeId};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Default)]
struct MeasuringBackend {
    latest: Mutex<Option<Vec<u8>>>,
    writes: AtomicU64,
    bytes: AtomicU64,
}

impl SnapshotBackend for MeasuringBackend {
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError> {
        Ok(self
            .latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?
            .clone())
    }

    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(
            u64::try_from(snapshot.len())
                .map_err(|_| OmsError::Storage("snapshot is too large".to_owned()))?,
            Ordering::Relaxed,
        );
        *self
            .latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)? = Some(snapshot.to_vec());
        Ok(())
    }
}

impl MeasuringBackend {
    fn reset_measurements(&self) {
        self.writes.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let commits = arguments
        .next()
        .map_or(Ok(1_000_u64), |value| value.parse())?;
    let state_bytes = arguments
        .next()
        .map_or(Ok(4_096_usize), |value| value.parse())?;
    let object_count = arguments
        .next()
        .map_or(Ok(100_u64), |value| value.parse())?;
    if commits == 0 || state_bytes == 0 || object_count == 0 {
        return Err("commits, state-bytes and object-count must be non-zero".into());
    }

    let backend = Arc::new(MeasuringBackend::default());
    let manager = InMemoryObjectManager::open_with_backend(backend.clone())?;
    let context = AccessContext::new(SubjectId::new());
    let mut object = None;
    for _ in 0..object_count {
        let request = CreateObject::new(TypeId::new(), vec![0_u8; state_bytes]);
        object.get_or_insert(request.id);
        let mut create = manager.begin(context);
        create.create(request);
        manager.commit(create)?;
    }
    let object = object.ok_or("benchmark did not create an Object")?;
    backend.reset_measurements();

    let started = Instant::now();
    let mut latencies = Vec::with_capacity(usize::try_from(commits)?);
    for sequence in 0..commits {
        let view = manager.read(context, object)?;
        let mut state = vec![0_u8; state_bytes];
        state[0] = sequence.to_le_bytes()[0];
        let mut transaction = manager.begin(context);
        transaction
            .expect(object, view.header().version)
            .update_state(object, state);
        let commit_started = Instant::now();
        manager.commit(transaction)?;
        latencies.push(commit_started.elapsed().as_nanos());
    }
    let elapsed = started.elapsed();
    let logical_bytes = commits.saturating_mul(u64::try_from(state_bytes)?);
    let backend_bytes = backend.bytes.load(Ordering::Relaxed);
    let per_second =
        f64::from(u32::try_from(commits.min(u64::from(u32::MAX)))?) / elapsed.as_secs_f64();
    let amplification =
        f64::from(u32::try_from(backend_bytes)?) / f64::from(u32::try_from(logical_bytes)?);

    println!("commits={commits}");
    println!("state_bytes={state_bytes}");
    println!("resident_objects={object_count}");
    println!("elapsed_ms={}", elapsed.as_millis());
    println!("commits_per_second={per_second:.2}");
    latencies.sort_unstable();
    println!("commit_p50_ns={}", percentile(&latencies, 50));
    println!("commit_p95_ns={}", percentile(&latencies, 95));
    println!("commit_p99_ns={}", percentile(&latencies, 99));
    println!("backend_writes={}", backend.writes.load(Ordering::Relaxed));
    println!("logical_changed_bytes={logical_bytes}");
    println!("backend_snapshot_bytes={backend_bytes}");
    println!("write_amplification={amplification:.2}");
    println!("object={object}");

    run_durable_file_benchmark(commits, state_bytes, context)?;
    Ok(())
}

fn run_durable_file_benchmark(
    commits: u64,
    state_bytes: usize,
    context: AccessContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("ousject-wal-benchmark-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let file_backend = Arc::new(FileSnapshotBackend::new(&path));
    let file_manager = InMemoryObjectManager::open_with_backend(file_backend.clone())?;
    let mut target = None;
    for _ in 0..64 {
        let request = CreateObject::new(TypeId::new(), vec![0_u8; state_bytes]);
        target.get_or_insert(request.id);
        let mut transaction = file_manager.begin(context);
        transaction.create(request);
        file_manager.commit(transaction)?;
    }
    let target = target.ok_or("file benchmark did not create an Object")?;
    file_manager.checkpoint()?;
    let durable_started = Instant::now();
    let mut durable_latencies = Vec::with_capacity(usize::try_from(commits)?);
    for sequence in 0..commits {
        let view = file_manager.read(context, target)?;
        let mut state = vec![0_u8; state_bytes];
        state[0] = sequence.to_le_bytes()[0];
        let mut transaction = file_manager.begin(context);
        transaction
            .expect(target, view.header().version)
            .update_state(target, state);
        let commit_started = Instant::now();
        file_manager.commit(transaction)?;
        durable_latencies.push(commit_started.elapsed().as_nanos());
    }
    let durable_elapsed = durable_started.elapsed();
    let usage = file_backend.storage_usage()?;
    durable_latencies.sort_unstable();
    println!("durable_file_commits={commits}");
    println!("durable_file_elapsed_ms={}", durable_elapsed.as_millis());
    println!(
        "durable_file_commits_per_second={:.2}",
        f64::from(u32::try_from(commits.min(u64::from(u32::MAX)))?) / durable_elapsed.as_secs_f64()
    );
    println!(
        "durable_commit_p50_ns={}",
        percentile(&durable_latencies, 50)
    );
    println!(
        "durable_commit_p95_ns={}",
        percentile(&durable_latencies, 95)
    );
    println!(
        "durable_commit_p99_ns={}",
        percentile(&durable_latencies, 99)
    );
    println!("checkpoint_bytes={}", usage.store_bytes);
    println!("incremental_wal_bytes={}", usage.wal_bytes);
    drop(file_manager);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

fn percentile(sorted: &[u128], percentage: usize) -> u128 {
    let index = sorted
        .len()
        .saturating_mul(percentage)
        .saturating_add(99)
        .saturating_div(100)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted[index]
}

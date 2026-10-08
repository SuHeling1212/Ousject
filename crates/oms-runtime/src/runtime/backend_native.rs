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

/// Persistence interface shared with Hosted and Native OMS instances.
///
/// Native backends must return from a write only after the device confirms
/// the required durability boundary; queue submission alone is insufficient.
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

#[cfg(test)]
#[allow(clippy::arc_with_non_send_sync)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};
    use oms_types::{CORE_TEXT_TYPE, ObjectId, SYSTEM_SUBJECT, Value, seed_id_generator};

    use super::{BlockDevice, BlockSnapshotBackend, InMemoryObjectManager, SnapshotBackend};
    use crate::runtime::{AccessContext, CreateObject, OmsError};

    #[derive(Debug, Default)]
    struct MemorySnapshotBackend {
        snapshot: RefCell<Option<Vec<u8>>>,
        reject_store: Cell<bool>,
    }

    impl SnapshotBackend for MemorySnapshotBackend {
        fn load(&self) -> Result<Option<Vec<u8>>, OmsError> {
            Ok(self.snapshot.borrow().clone())
        }

        fn store(&self, snapshot: &[u8]) -> Result<(), OmsError> {
            if self.reject_store.get() {
                return Err(OmsError::Storage("test durable write failure".into()));
            }
            *self.snapshot.borrow_mut() = Some(snapshot.to_vec());
            Ok(())
        }
    }

    #[derive(Debug)]
    struct MemoryBlockDevice {
        block_size: usize,
        working: RefCell<Vec<u8>>,
        durable: RefCell<Vec<u8>>,
        flush_count: Cell<usize>,
        fail_flush_at: Cell<Option<usize>>,
    }

    impl MemoryBlockDevice {
        fn new(block_size: usize, blocks: usize) -> Self {
            let bytes = alloc::vec![0; block_size * blocks];
            Self {
                block_size,
                working: RefCell::new(bytes.clone()),
                durable: RefCell::new(bytes),
                flush_count: Cell::new(0),
                fail_flush_at: Cell::new(None),
            }
        }

        fn simulate_crash(&self) {
            *self.working.borrow_mut() = self.durable.borrow().clone();
        }
    }

    impl BlockDevice for MemoryBlockDevice {
        fn block_size(&self) -> u32 {
            u32::try_from(self.block_size).unwrap()
        }

        fn block_count(&self) -> u64 {
            u64::try_from(self.working.borrow().len() / self.block_size).unwrap()
        }

        fn read_blocks(&self, first_block: u64, output: &mut [u8]) -> Result<(), OmsError> {
            let offset = usize::try_from(first_block)
                .unwrap()
                .checked_mul(self.block_size)
                .ok_or(OmsError::InvalidOperation("memory block offset overflow"))?;
            let end = offset
                .checked_add(output.len())
                .ok_or(OmsError::InvalidOperation("memory block range overflow"))?;
            let bytes = self.working.borrow();
            output.copy_from_slice(
                bytes
                    .get(offset..end)
                    .ok_or(OmsError::InvalidOperation("memory block read out of range"))?,
            );
            Ok(())
        }

        fn write_blocks(&self, first_block: u64, data: &[u8]) -> Result<(), OmsError> {
            let offset = usize::try_from(first_block)
                .unwrap()
                .checked_mul(self.block_size)
                .ok_or(OmsError::InvalidOperation("memory block offset overflow"))?;
            let end = offset
                .checked_add(data.len())
                .ok_or(OmsError::InvalidOperation("memory block range overflow"))?;
            let mut bytes = self.working.borrow_mut();
            bytes
                .get_mut(offset..end)
                .ok_or(OmsError::InvalidOperation("memory block write out of range"))?
                .copy_from_slice(data);
            Ok(())
        }

        fn flush(&self) -> Result<(), OmsError> {
            let next = self.flush_count.get() + 1;
            self.flush_count.set(next);
            if self.fail_flush_at.get() == Some(next) {
                return Err(OmsError::Storage("simulated power loss at flush".into()));
            }
            *self.durable.borrow_mut() = self.working.borrow().clone();
            Ok(())
        }
    }

    #[test]
    fn native_commit_persists_before_publish_and_recovers_same_object() {
        let _ = seed_id_generator(0x4e41_5449_5645);
        let backend = Arc::new(MemorySnapshotBackend::default());
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let manager = InMemoryObjectManager::open_with_backend(backend.clone())
            .expect("open empty Native OMS");
        let value = Value::Text("durable world".into());
        let request = CreateObject::new(CORE_TEXT_TYPE, value.encode().unwrap())
            .with_id(ObjectId::from_u128(0x4e41_5449_5645_0000_0000_0000_0000_0001));
        let object = request.id;
        let mut transaction = manager.begin(system);
        transaction.create(request);
        manager.commit(transaction).expect("durable OMS commit");

        let recovered = InMemoryObjectManager::open_with_backend(backend)
            .expect("recover Native OMS");
        let record = recovered.read(system, object).expect("recover Object");
        assert_eq!(Value::decode(record.state()).unwrap(), value);
    }

    #[test]
    fn failed_native_durable_write_does_not_publish_candidate() {
        let _ = seed_id_generator(0x4e41_5449_5646);
        let backend = Arc::new(MemorySnapshotBackend {
            reject_store: Cell::new(true),
            ..MemorySnapshotBackend::default()
        });
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let manager = InMemoryObjectManager::open_with_backend(backend)
            .expect("open empty Native OMS");
        let request = CreateObject::new(
            CORE_TEXT_TYPE,
            Value::Text("must not publish".into()).encode().unwrap(),
        )
        .with_id(ObjectId::from_u128(0x4e41_5449_5646_0000_0000_0000_0000_0001));
        let object = request.id;
        let mut transaction = manager.begin(system);
        transaction.create(request);
        assert!(manager.commit(transaction).is_err());
        assert!(matches!(manager.read(system, object), Err(OmsError::NotFound(_))));
    }

    #[test]
    fn block_snapshot_recovers_the_last_flushed_generation() {
        let device = Arc::new(MemoryBlockDevice::new(512, 64));
        let backend = BlockSnapshotBackend::new(device.clone()).expect("valid block geometry");
        let generation_one = b"committed generation one";
        let generation_two = b"committed generation two";
        backend.store(generation_one).expect("commit first image");

        // Flushes one and two persist the inactive-slot invalidation and
        // payload. Failure of flush three leaves the commit header volatile.
        device.fail_flush_at.set(Some(6));
        assert!(backend.store(generation_two).is_err());
        device.simulate_crash();
        assert_eq!(
            backend.load().unwrap().as_deref(),
            Some(generation_one.as_slice())
        );

        device.fail_flush_at.set(None);
        backend.store(generation_two).expect("commit second image");
        assert_eq!(
            backend.load().unwrap().as_deref(),
            Some(generation_two.as_slice())
        );
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

/// Raw fixed-size block operations required by the Native OMS snapshot store.
/// Implementations must return from `flush` only after prior writes are durable.
pub trait BlockDevice: core::fmt::Debug {
    /// Returns the device's fixed logical block size in bytes.
    fn block_size(&self) -> u32;
    /// Returns the number of addressable logical blocks.
    fn block_count(&self) -> u64;
    /// Reads whole blocks into a caller-provided buffer.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid ranges or device I/O failure.
    fn read_blocks(&self, first_block: u64, output: &mut [u8]) -> Result<(), OmsError>;
    /// Writes whole blocks from a caller-provided buffer.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid ranges or device I/O failure.
    fn write_blocks(&self, first_block: u64, data: &[u8]) -> Result<(), OmsError>;
    /// Makes every preceding write durable on the storage medium.
    ///
    /// # Errors
    ///
    /// Returns an error if the device cannot confirm durability.
    fn flush(&self) -> Result<(), OmsError>;
}

const BLOCK_SNAPSHOT_MAGIC: &[u8; 8] = b"OMSBK01\0";
const BLOCK_HEADER_BYTES: usize = 40;

/// Crash-consistent two-slot snapshot backend over a raw Native block device.
///
/// Each update invalidates the inactive slot, writes and flushes its payload,
/// then publishes a checksummed generation header and flushes again. Recovery
/// selects the newest valid generation; an incomplete inactive slot leaves
/// the previous generation intact.
#[derive(Debug)]
pub struct BlockSnapshotBackend {
    device: Arc<dyn BlockDevice>,
    block_size: usize,
    slot_blocks: u64,
}

impl BlockSnapshotBackend {
    /// Creates a snapshot backend over the first two equal regions of a disk.
    ///
    /// # Errors
    ///
    /// Returns an error when the geometry cannot hold two headers and
    /// payloads or the device block size is unsupported.
    pub fn new(device: Arc<dyn BlockDevice>) -> Result<Self, OmsError> {
        let block_size = usize::try_from(device.block_size())
            .map_err(|_| OmsError::InvalidOperation("block size is unsupported"))?;
        let slot_blocks = device.block_count() / 2;
        if block_size < BLOCK_HEADER_BYTES || slot_blocks < 2 {
            return Err(OmsError::InvalidOperation(
                "block device is too small for a dual-slot OMS snapshot",
            ));
        }
        Ok(Self {
            device,
            block_size,
            slot_blocks,
        })
    }

    fn read_slot(&self, slot: u64) -> Result<SlotState, OmsError> {
        let first_block = slot
            .checked_mul(self.slot_blocks)
            .ok_or(OmsError::InvalidOperation("snapshot slot offset overflow"))?;
        let mut header = vec![0; self.block_size];
        self.device.read_blocks(first_block, &mut header)?;
        if header.iter().all(|byte| *byte == 0) {
            return Ok(SlotState::Empty);
        }
        if header.get(..8) != Some(BLOCK_SNAPSHOT_MAGIC.as_slice())
            || checksum(&header[..32]) != read_u64(&header[32..40])
        {
            return Ok(SlotState::Torn);
        }
        let generation = read_u64(&header[8..16]);
        let payload_len = usize::try_from(read_u64(&header[16..24]))
            .map_err(|_| OmsError::Corruption("Native OMS snapshot length overflow".into()))?;
        if generation == 0 {
            return Ok(SlotState::Torn);
        }
        let payload_capacity = usize::try_from(self.slot_blocks - 1)
            .ok()
            .and_then(|blocks| blocks.checked_mul(self.block_size))
            .ok_or(OmsError::InvalidOperation("snapshot slot capacity overflow"))?;
        if payload_len == 0 || payload_len > payload_capacity {
            return Err(OmsError::Corruption(
                "Native OMS snapshot length exceeds its slot".into(),
            ));
        }
        let block_count = payload_len
            .checked_add(self.block_size - 1)
            .ok_or(OmsError::Corruption("Native OMS payload size overflow".into()))?
            / self.block_size;
        let read_len = block_count
            .checked_mul(self.block_size)
            .ok_or(OmsError::Corruption("Native OMS payload size overflow".into()))?;
        let mut payload = vec![0; read_len];
        self.device.read_blocks(first_block + 1, &mut payload)?;
        payload.truncate(payload_len);
        if checksum(&payload) != read_u64(&header[24..32]) {
            return Err(OmsError::Corruption(
                "Native OMS snapshot checksum mismatch".into(),
            ));
        }
        Ok(SlotState::Valid(generation, payload))
    }

    fn latest(&self) -> Result<Option<(u64, u64, Vec<u8>)>, OmsError> {
        let first = self.read_slot(0)?;
        let second = self.read_slot(1)?;
        match (first, second) {
            (SlotState::Valid(a_generation, a), SlotState::Valid(b_generation, b)) => {
                if a_generation >= b_generation {
                    Ok(Some((0, a_generation, a)))
                } else {
                    Ok(Some((1, b_generation, b)))
                }
            }
            (SlotState::Valid(generation, bytes), _) => Ok(Some((0, generation, bytes))),
            (_, SlotState::Valid(generation, bytes)) => Ok(Some((1, generation, bytes))),
            (SlotState::Empty, SlotState::Empty) => Ok(None),
            _ => Err(OmsError::Corruption(
                "Native OMS has no valid snapshot generation".into(),
            )),
        }
    }
}

enum SlotState {
    Empty,
    Torn,
    Valid(u64, Vec<u8>),
}

impl SnapshotBackend for BlockSnapshotBackend {
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError> {
        Ok(self.latest()?.map(|(_, _, bytes)| bytes))
    }

    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        let payload_capacity = usize::try_from(self.slot_blocks - 1)
            .ok()
            .and_then(|blocks| blocks.checked_mul(self.block_size))
            .ok_or(OmsError::InvalidOperation("snapshot slot capacity overflow"))?;
        if snapshot.is_empty() || snapshot.len() > payload_capacity {
            return Err(OmsError::Storage(
                "Native OMS snapshot does not fit in the block image".into(),
            ));
        }
        let current = self.latest()?;
        let (target, generation) = match current {
            None => (0, 1),
            Some((slot, generation, _)) => (
                1 - slot,
                generation.checked_add(1).ok_or(OmsError::Storage(
                    "Native OMS snapshot generation exhausted".into(),
                ))?,
            ),
        };
        let first_block = target
            .checked_mul(self.slot_blocks)
            .ok_or(OmsError::InvalidOperation("snapshot slot offset overflow"))?;

        // Keep the active slot untouched until the new payload and header are
        // durable. A torn inactive generation is ignored during recovery.
        self.device
            .write_blocks(first_block, &vec![0; self.block_size])?;
        self.device.flush()?;

        let block_count = snapshot
            .len()
            .checked_add(self.block_size - 1)
            .ok_or(OmsError::Storage("Native OMS payload size overflow".into()))?
            / self.block_size;
        let mut payload = vec![0; block_count * self.block_size];
        payload[..snapshot.len()].copy_from_slice(snapshot);
        self.device.write_blocks(first_block + 1, &payload)?;
        self.device.flush()?;

        let mut header = vec![0; self.block_size];
        header[..8].copy_from_slice(BLOCK_SNAPSHOT_MAGIC);
        header[8..16].copy_from_slice(&generation.to_le_bytes());
        header[16..24].copy_from_slice(
            &u64::try_from(snapshot.len())
                .map_err(|_| OmsError::Storage("Native OMS snapshot is too large".into()))?
                .to_le_bytes(),
        );
        header[24..32].copy_from_slice(&checksum(snapshot).to_le_bytes());
        let header_checksum = checksum(&header[..32]);
        header[32..40].copy_from_slice(&header_checksum.to_le_bytes());
        self.device.write_blocks(first_block, &header)?;
        self.device.flush()
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn read_u64(bytes: &[u8]) -> u64 {
    let mut value = [0; 8];
    value.copy_from_slice(bytes);
    u64::from_le_bytes(value)
}

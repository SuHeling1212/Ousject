fn persist_file_snapshot(path: &Path, bytes: &[u8]) -> Result<(), OmsError> {
    if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(storage_error)?;
    }
    let temporary = temporary_path(path);
    let mut file = File::create(&temporary).map_err(storage_error)?;
    file.write_all(bytes).map_err(storage_error)?;
    file.sync_all().map_err(storage_error)?;
    drop(file);
    fs::rename(&temporary, path).map_err(storage_error)?;
    sync_parent_directory(path)
}

fn persist_generation_manifest(path: &Path, bytes: &[u8]) -> Result<bool, OmsError> {
    if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(storage_error)?;
    }
    let temporary = temporary_path(path);
    let mut file = File::create(&temporary).map_err(storage_error)?;
    file.write_all(bytes).map_err(storage_error)?;
    file.sync_all().map_err(storage_error)?;
    drop(file);
    fs::rename(&temporary, path).map_err(storage_error)?;
    // `false` still means the atomic rename is visible. The caller keeps the
    // old generation intact unless the directory entry is confirmed durable.
    Ok(sync_parent_directory(path).is_ok())
}

fn append_wal_handle(file: &mut File, payload: &[u8]) -> Result<(), OmsError> {
    let length = u64::try_from(payload.len())
        .map_err(|_| OmsError::Storage("WAL record is too large".to_owned()))?;
    file.write_all(WAL_MAGIC).map_err(storage_error)?;
    file.write_all(&length.to_le_bytes())
        .map_err(storage_error)?;
    file.write_all(payload).map_err(storage_error)?;
    file.write_all(&wal_checksum(payload).to_le_bytes())
        .map_err(storage_error)?;
    file.sync_data().map_err(storage_error)
}

fn open_wal_append(path: &Path) -> Result<File, OmsError> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(storage_error)
}

fn append_wal_records(path: &Path, payloads: &[Vec<u8>]) -> Result<u64, OmsError> {
    if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(storage_error)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(storage_error)?;
    let mut written = 0_u64;
    for payload in payloads {
        let length = u64::try_from(payload.len())
            .map_err(|_| OmsError::Storage("WAL record is too large".to_owned()))?;
        file.write_all(WAL_MAGIC).map_err(storage_error)?;
        file.write_all(&length.to_le_bytes())
            .map_err(storage_error)?;
        file.write_all(payload).map_err(storage_error)?;
        file.write_all(&wal_checksum(payload).to_le_bytes())
            .map_err(storage_error)?;
        written = written.saturating_add(length.saturating_add(20));
    }
    file.sync_all().map_err(storage_error)?;
    Ok(written)
}

fn encode_delta_wal_payload(delta: &[u8]) -> Result<Vec<u8>, OmsError> {
    let mut payload = Vec::with_capacity(delta.len().saturating_add(1));
    payload.push(4);
    payload.extend_from_slice(delta);
    compress_wal_payload(payload)
}

fn encode_group_wal_payload(deltas: &[&[u8]]) -> Result<Vec<u8>, OmsError> {
    if deltas.is_empty() {
        return Err(OmsError::InvalidOperation("WAL group cannot be empty"));
    }
    if deltas.len() == 1 {
        return encode_delta_wal_payload(deltas[0]);
    }
    let mut payload = Vec::new();
    payload.push(5);
    snapshot_u32(&mut payload, snapshot_len(deltas.len())?);
    for delta in deltas {
        snapshot_u64(
            &mut payload,
            u64::try_from(delta.len())
                .map_err(|_| OmsError::Storage("WAL delta is too large".to_owned()))?,
        );
        payload.extend_from_slice(delta);
    }
    compress_wal_payload(payload)
}

fn decode_group_wal_payload(body: &[u8]) -> Result<Vec<Vec<u8>>, OmsError> {
    let mut reader = WalDeltaReader::new(body);
    let count = usize::try_from(reader.u32()?)
        .map_err(|_| corruption("WAL group transaction count is too large"))?;
    if count == 0 || count > MAX_SNAPSHOT_ITEMS {
        return Err(corruption("WAL group transaction count is invalid"));
    }
    let mut deltas = Vec::with_capacity(count);
    for _ in 0..count {
        let length = usize::try_from(reader.u64()?)
            .map_err(|_| corruption("WAL group delta is too large"))?;
        deltas.push(reader.take(length)?.to_vec());
    }
    if !reader.is_empty() {
        return Err(corruption("WAL group has trailing bytes"));
    }
    Ok(deltas)
}

fn encode_reset_wal_payload(snapshot: &[u8]) -> Result<Vec<u8>, OmsError> {
    let mut payload = Vec::with_capacity(snapshot.len().saturating_add(1));
    payload.push(3);
    payload.extend_from_slice(snapshot);
    compress_wal_payload(payload)
}

struct LoadedWalRecords {
    latest: Option<Vec<u8>>,
    updates: Vec<Vec<u8>>,
    committed_records: Vec<Vec<u8>>,
}

fn load_wal_records(
    path: &Path,
    mut latest: Option<Vec<u8>>,
) -> Result<LoadedWalRecords, OmsError> {
    if !path.exists() {
        return Ok(LoadedWalRecords {
            latest,
            updates: Vec::new(),
            committed_records: Vec::new(),
        });
    }
    let bytes = fs::read(path).map_err(storage_error)?;
    let mut position = 0_usize;
    let mut payloads = Vec::new();
    while bytes.len().saturating_sub(position) >= 12 {
        if &bytes[position..position + 4] != WAL_MAGIC {
            return Err(corruption("invalid Object Store WAL magic"));
        }
        let length = u64::from_le_bytes(
            bytes[position + 4..position + 12]
                .try_into()
                .map_err(|_| corruption("truncated Object Store WAL length"))?,
        );
        let length = usize::try_from(length)
            .map_err(|_| corruption("Object Store WAL record is too large"))?;
        let Some(record_end) = position
            .checked_add(12)
            .and_then(|start| start.checked_add(length))
        else {
            return Err(corruption("Object Store WAL length overflow"));
        };
        let Some(checksum_end) = record_end.checked_add(8) else {
            return Err(corruption("Object Store WAL length overflow"));
        };
        if checksum_end > bytes.len() {
            break;
        }
        let payload = &bytes[position + 12..record_end];
        let expected = u64::from_le_bytes(
            bytes[record_end..checksum_end]
                .try_into()
                .map_err(|_| corruption("truncated Object Store WAL checksum"))?,
        );
        if wal_checksum(payload) != expected {
            return Err(corruption("Object Store WAL checksum mismatch"));
        }
        payloads.push(payload);
        position = checksum_end;
    }
    if position < bytes.len() {
        // A torn final frame was never a committed transaction. Remove it
        // before future appends, otherwise the next valid WAL frame would be
        // hidden behind a permanently malformed tail.
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(storage_error)?;
        file.set_len(
            u64::try_from(position)
                .map_err(|_| OmsError::Storage("WAL offset does not fit in u64".to_owned()))?,
        )
        .map_err(storage_error)?;
        file.sync_all().map_err(storage_error)?;
    }
    let mut updates = Vec::new();
    let mut committed_records = Vec::new();
    for payload in payloads {
        let expanded = if payload.first() == Some(&2) {
            decompress_wal_payload(&payload[1..])?
        } else {
            payload.to_vec()
        };
        match expanded.first() {
            Some(4) => {
                updates.push(expanded[1..].to_vec());
                committed_records.push(payload.to_vec());
            }
            Some(5) => {
                updates.extend(decode_group_wal_payload(&expanded[1..])?);
                committed_records.push(payload.to_vec());
            }
            _ => {
                latest = Some(apply_wal_payload(latest.as_deref(), payload)?);
                updates.clear();
                committed_records.clear();
            }
        }
    }
    Ok(LoadedWalRecords {
        latest,
        updates,
        committed_records,
    })
}

fn apply_wal_payload(previous: Option<&[u8]>, payload: &[u8]) -> Result<Vec<u8>, OmsError> {
    let Some((&kind, body)) = payload.split_first() else {
        return Err(corruption("empty Object Store WAL payload"));
    };
    if kind == 2 {
        let expanded = decompress_wal_payload(body)?;
        return apply_wal_payload(previous, &expanded);
    }
    if kind == 0 {
        return Ok(body.to_vec());
    }
    if kind == 3 {
        return Ok(body.to_vec());
    }
    if kind != 1 {
        return Err(corruption("invalid Object Store WAL payload kind"));
    }
    let base = previous.ok_or_else(|| corruption("WAL delta has no base snapshot"))?;
    let mut reader = WalDeltaReader::new(body);
    if reader.u64()? != wal_checksum(base) {
        return Err(corruption("WAL delta base checksum mismatch"));
    }
    let length = usize::try_from(reader.u64()?)
        .map_err(|_| corruption("WAL delta snapshot is too large"))?;
    if length != base.len() {
        return Err(corruption("WAL delta base length mismatch"));
    }
    let run_count = usize::try_from(reader.u32()?)
        .map_err(|_| corruption("WAL delta run count is too large"))?;
    let mut snapshot = base.to_vec();
    let mut previous_end = 0_usize;
    for _ in 0..run_count {
        let offset = usize::try_from(reader.u64()?)
            .map_err(|_| corruption("WAL delta offset is too large"))?;
        let bytes = reader.bytes()?;
        let end = offset
            .checked_add(bytes.len())
            .ok_or_else(|| corruption("WAL delta range overflow"))?;
        if offset < previous_end || end > snapshot.len() {
            return Err(corruption("WAL delta range is invalid"));
        }
        snapshot[offset..end].copy_from_slice(bytes);
        previous_end = end;
    }
    if !reader.is_empty() {
        return Err(corruption("trailing Object Store WAL delta data"));
    }
    Ok(snapshot)
}

fn decompress_wal_payload(body: &[u8]) -> Result<Vec<u8>, OmsError> {
    let mut reader = WalDeltaReader::new(body);
    let length = usize::try_from(reader.u64()?)
        .map_err(|_| corruption("compressed WAL payload is too large"))?;
    let mut expanded = Vec::with_capacity(length);
    while !reader.is_empty() {
        let count = usize::try_from(reader.u32()?)
            .map_err(|_| corruption("compressed WAL run is too large"))?;
        let byte = reader.take(1)?[0];
        if count == 0 || expanded.len().saturating_add(count) > length {
            return Err(corruption("compressed WAL run is invalid"));
        }
        expanded.resize(expanded.len() + count, byte);
    }
    if expanded.len() != length || expanded.first() == Some(&2) {
        return Err(corruption("compressed WAL payload length is invalid"));
    }
    Ok(expanded)
}

fn compress_wal_payload(payload: Vec<u8>) -> Result<Vec<u8>, OmsError> {
    let mut compressed = Vec::new();
    compressed.push(2);
    compressed.extend_from_slice(
        &u64::try_from(payload.len())
            .map_err(|_| OmsError::Storage("WAL payload is too large".to_owned()))?
            .to_le_bytes(),
    );
    let mut position = 0_usize;
    while position < payload.len() {
        let byte = payload[position];
        let start = position;
        while position < payload.len()
            && payload[position] == byte
            && position - start < u32::MAX as usize
        {
            position += 1;
        }
        compressed.extend_from_slice(
            &u32::try_from(position - start)
                .map_err(|_| OmsError::Storage("WAL compression run is too large".to_owned()))?
                .to_le_bytes(),
        );
        compressed.push(byte);
    }
    Ok(if compressed.len() < payload.len() {
        compressed
    } else {
        payload
    })
}

struct WalDeltaReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> WalDeltaReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], OmsError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| corruption("WAL delta length overflow"))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| corruption("truncated Object Store WAL delta"))?;
        self.position = end;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, OmsError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| corruption("truncated WAL delta u32"))?,
        ))
    }

    fn u64(&mut self) -> Result<u64, OmsError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| corruption("truncated WAL delta u64"))?,
        ))
    }

    fn bytes(&mut self) -> Result<&'a [u8], OmsError> {
        let length =
            usize::try_from(self.u32()?).map_err(|_| corruption("WAL delta run is too large"))?;
        self.take(length)
    }

    const fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
}

fn wal_checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn encode_snapshot(state: &ShardState) -> Result<Vec<u8>, OmsError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(RETIREMENT_TIME_EXTENSION);
    snapshot_u32(&mut bytes, snapshot_len(state.objects.len())?);
    for record in state.objects.values() {
        snapshot_u128(&mut bytes, record.header.id.as_u128());
        snapshot_u128(&mut bytes, record.header.type_id.as_u128());
        match record.header.parent_id {
            Some(parent) => {
                bytes.push(1);
                snapshot_u128(&mut bytes, parent.as_u128());
            }
            None => bytes.push(0),
        }
        snapshot_u64(&mut bytes, record.header.version.get());
        bytes.push(lifecycle_tag(record.header.lifecycle));
        snapshot_u64(&mut bytes, record.retired_at_unix_ms.unwrap_or_default());
        snapshot_bytes(&mut bytes, &record.state)?;

        snapshot_u32(&mut bytes, snapshot_len(record.children.len())?);
        for child in &record.children {
            snapshot_u128(&mut bytes, child.as_u128());
        }
        snapshot_u32(&mut bytes, snapshot_len(record.links.len())?);
        for (name, target) in &record.links {
            snapshot_string(&mut bytes, name)?;
            snapshot_u128(&mut bytes, target.as_u128());
        }
        snapshot_u16(&mut bytes, capability_bits(&record.capabilities));
        snapshot_u128(&mut bytes, record.policy.owner.as_u128());
        snapshot_u32(&mut bytes, snapshot_len(record.policy.grants.len())?);
        for (subject, capabilities) in &record.policy.grants {
            snapshot_u128(&mut bytes, subject.as_u128());
            snapshot_u16(&mut bytes, capability_bits(capabilities));
        }
    }
    Ok(bytes)
}

fn encode_snapshot_delta(
    state: &ShardState,
    changed: &BTreeSet<ObjectId>,
) -> Result<Vec<u8>, OmsError> {
    let mut delta = ShardState::default();
    for &object in changed {
        let record = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?;
        delta.objects.insert(object, record.clone());
        delta
            .by_type
            .entry(record.header.type_id)
            .or_default()
            .insert(object);
    }
    encode_snapshot(&delta)
}

fn apply_snapshot_delta(state: &mut ShardState, bytes: &[u8]) -> Result<(), OmsError> {
    let delta = decode_snapshot(bytes)?;
    for (object, record) in delta.objects {
        if state
            .objects
            .get(&object)
            .is_some_and(|existing| existing.header.type_id != record.header.type_id)
        {
            return Err(corruption("Object delta changes an Object's Type"));
        }
        state
            .by_type
            .entry(record.header.type_id)
            .or_default()
            .insert(object);
        state.objects.insert(object, record);
    }
    Ok(())
}

fn decode_snapshot(bytes: &[u8]) -> Result<ShardState, OmsError> {
    let mut reader = SnapshotReader::new(bytes);
    if reader.take(4)? != SNAPSHOT_MAGIC {
        return Err(corruption("invalid Object Store snapshot magic"));
    }
    let has_retirement_time = reader.peek(4)? == RETIREMENT_TIME_EXTENSION;
    if has_retirement_time {
        reader.take(4)?;
    }
    let count = reader.count()?;
    let mut objects = im::OrdMap::new();
    let mut by_type = im::OrdMap::<TypeId, im::OrdSet<ObjectId>>::new();
    for _ in 0..count {
        let id = ObjectId::from_u128(reader.u128()?);
        let type_id = TypeId::from_u128(reader.u128()?);
        let parent_id = match reader.u8()? {
            0 => None,
            1 => Some(ObjectId::from_u128(reader.u128()?)),
            _ => return Err(corruption("invalid parent marker")),
        };
        let version = ObjectVersion::new(reader.u64()?);
        let lifecycle = decode_lifecycle(reader.u8()?)?;
        let retired_at_unix_ms = if has_retirement_time {
            let timestamp = reader.u64()?;
            match (lifecycle, timestamp) {
                (LifecycleState::Tombstoned, 0) => {
                    return Err(corruption("Tombstone has no retirement timestamp"));
                }
                (LifecycleState::Tombstoned, timestamp) => Some(timestamp),
                (_, 0) => None,
                (_, _) => return Err(corruption("active Object has a retirement timestamp")),
            }
        } else if lifecycle == LifecycleState::Tombstoned {
            // An older unpublished snapshot has no age metadata. Start the
            // seven-day retention window at upgrade time rather than deleting
            // its payload immediately.
            Some(unix_time_millis()?)
        } else {
            None
        };
        let state = Arc::from(reader.bytes()?.to_vec());

        let mut children = BTreeSet::new();
        for _ in 0..reader.count()? {
            children.insert(ObjectId::from_u128(reader.u128()?));
        }
        let mut links = BTreeMap::new();
        for _ in 0..reader.count()? {
            let name = reader.string()?;
            let target = ObjectId::from_u128(reader.u128()?);
            if links.insert(name, target).is_some() {
                return Err(corruption("duplicate link name"));
            }
        }
        let capabilities = decode_capabilities(reader.u16()?)?;
        let owner = SubjectId::from_u128(reader.u128()?);
        let mut grants = BTreeMap::new();
        for _ in 0..reader.count()? {
            let subject = SubjectId::from_u128(reader.u128()?);
            let granted = decode_capabilities(reader.u16()?)?;
            if grants.insert(subject, granted).is_some() {
                return Err(corruption("duplicate policy subject"));
            }
        }
        let record = ObjectRecord {
            header: ObjectHeader {
                id,
                type_id,
                parent_id,
                version,
                lifecycle,
            },
            retired_at_unix_ms,
            state,
            children,
            links,
            capabilities,
            policy: AccessPolicy { owner, grants },
        };
        if objects.insert(id, record).is_some() {
            return Err(corruption("duplicate ObjectId"));
        }
        by_type.entry(type_id).or_default().insert(id);
    }
    if !reader.is_empty() {
        return Err(corruption("trailing Object Store snapshot data"));
    }
    Ok(ShardState { objects, by_type })
}

const fn lifecycle_tag(state: LifecycleState) -> u8 {
    match state {
        LifecycleState::Creating => 0,
        LifecycleState::Active => 1,
        LifecycleState::Suspended => 2,
        LifecycleState::Migrating => 3,
        LifecycleState::Terminating => 4,
        LifecycleState::Tombstoned => 5,
    }
}

fn decode_lifecycle(tag: u8) -> Result<LifecycleState, OmsError> {
    match tag {
        0 => Ok(LifecycleState::Creating),
        1 => Ok(LifecycleState::Active),
        2 => Ok(LifecycleState::Suspended),
        3 => Ok(LifecycleState::Migrating),
        4 => Ok(LifecycleState::Terminating),
        5 => Ok(LifecycleState::Tombstoned),
        _ => Err(corruption("invalid lifecycle tag")),
    }
}

fn capability_bits(capabilities: &BTreeSet<Capability>) -> u16 {
    capabilities.iter().fold(0, |bits, capability| {
        bits | match capability {
            Capability::ViewValue => 1 << 0,
            Capability::ReplaceValue => 1 << 1,
            Capability::Link => 1 << 2,
            Capability::Reparent => 1 << 3,
            Capability::Retire => 1 << 4,
            Capability::Inspect => 1 << 5,
            Capability::ManagePolicy => 1 << 6,
            Capability::CreateChild => 1 << 7,
            Capability::Invoke => 1 << 8,
        }
    })
}

fn decode_capabilities(bits: u16) -> Result<BTreeSet<Capability>, OmsError> {
    if bits & !0x1ff != 0 {
        return Err(corruption("unknown capability bits"));
    }
    let mappings = [
        (1 << 0, Capability::ViewValue),
        (1 << 1, Capability::ReplaceValue),
        (1 << 2, Capability::Link),
        (1 << 3, Capability::Reparent),
        (1 << 4, Capability::Retire),
        (1 << 5, Capability::Inspect),
        (1 << 6, Capability::ManagePolicy),
        (1 << 7, Capability::CreateChild),
        (1 << 8, Capability::Invoke),
    ];
    Ok(mappings
        .into_iter()
        .filter_map(|(mask, capability)| (bits & mask != 0).then_some(capability))
        .collect())
}

fn snapshot_len(value: usize) -> Result<u32, OmsError> {
    if value > MAX_SNAPSHOT_ITEMS {
        return Err(OmsError::Storage("snapshot item limit exceeded".to_owned()));
    }
    u32::try_from(value).map_err(|_| OmsError::Storage("snapshot is too large".to_owned()))
}

fn snapshot_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), OmsError> {
    snapshot_u32(bytes, snapshot_len(value.len())?);
    bytes.extend_from_slice(value);
    Ok(())
}

fn snapshot_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), OmsError> {
    snapshot_bytes(bytes, value.as_bytes())
}

fn snapshot_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn snapshot_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn snapshot_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn snapshot_u128(bytes: &mut Vec<u8>, value: u128) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

// `std::io::Result::map_err` supplies the owned error to this adapter.
#[allow(clippy::needless_pass_by_value)]
fn storage_error(error: std::io::Error) -> OmsError {
    OmsError::Storage(error.to_string())
}

fn corruption(message: &str) -> OmsError {
    OmsError::Corruption(message.to_owned())
}

struct SnapshotReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SnapshotReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], OmsError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| corruption("snapshot position overflow"))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| corruption("truncated Object Store snapshot"))?;
        self.position = end;
        Ok(value)
    }

    fn peek(&self, count: usize) -> Result<&'a [u8], OmsError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| corruption("snapshot position overflow"))?;
        self.bytes
            .get(self.position..end)
            .ok_or_else(|| corruption("truncated Object Store snapshot"))
    }

    fn u8(&mut self) -> Result<u8, OmsError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, OmsError> {
        let mut bytes = [0; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, OmsError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, OmsError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn u128(&mut self) -> Result<u128, OmsError> {
        let mut bytes = [0; 16];
        bytes.copy_from_slice(self.take(16)?);
        Ok(u128::from_le_bytes(bytes))
    }

    fn count(&mut self) -> Result<usize, OmsError> {
        let count = usize::try_from(self.u32()?)
            .map_err(|_| corruption("snapshot count is not supported"))?;
        if count > MAX_SNAPSHOT_ITEMS {
            return Err(corruption("snapshot item limit exceeded"));
        }
        Ok(count)
    }

    fn bytes(&mut self) -> Result<&'a [u8], OmsError> {
        let length = self.count()?;
        self.take(length)
    }

    fn string(&mut self) -> Result<String, OmsError> {
        let value = std::str::from_utf8(self.bytes()?)
            .map_err(|_| corruption("snapshot string is not UTF-8"))?;
        Ok(value.to_owned())
    }

    fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
}

use super::super::{
    BTreeMap, BTreeSet, DEVICE_BLOCK_STORAGE_TYPE, Mutex, ObjectId, ObjectProvider, OpenOptions,
    PathBuf, ProviderError, ProviderOutcome, Read, Seek, SeekFrom, Value, Write,
};
use super::adapter_error;

#[derive(Debug)]
pub(crate) struct HostBlockStorageProvider {
    file: Mutex<std::fs::File>,
}

impl HostBlockStorageProvider {
    pub(crate) fn open(path: PathBuf) -> Result<Self, std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl ObjectProvider for HostBlockStorageProvider {
    fn type_id(&self) -> oms_types::TypeId {
        DEVICE_BLOCK_STORAGE_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Block Storage Objects are published by hardware discovery",
        ))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        const BLOCK_SIZE: usize = 4096;
        match (capability, arguments) {
            ("input", [Value::Integer(maximum)]) => {
                let maximum = usize::try_from(*maximum)
                    .ok()
                    .filter(|value| *value > 0 && *value <= 16 * 1024 * 1024)
                    .ok_or(ProviderError::InvalidArguments(
                        "input size must be between 1 and 16777216",
                    ))?;
                let mut fields = storage_state(state)?;
                let offset = stream_offset(&fields)?;
                let mut file = self.file.lock().map_err(|_| ProviderError::Unavailable)?;
                file.seek(SeekFrom::Start(offset)).map_err(adapter_error)?;
                let mut bytes = vec![0_u8; maximum];
                let count = file.read(&mut bytes).map_err(adapter_error)?;
                bytes.truncate(count);
                update_stream_offset(&mut fields, offset, count)?;
                Ok(ProviderOutcome::result(Value::Bytes(bytes)).with_state(Value::Record(fields)))
            }
            ("output", [Value::Bytes(bytes)]) => {
                if bytes.len() > 16 * 1024 * 1024 {
                    return Err(ProviderError::InvalidArguments(
                        "output is limited to 16777216 Bytes",
                    ));
                }
                let mut fields = storage_state(state)?;
                let offset = stream_offset(&fields)?;
                let next_offset = offset
                    .checked_add(u64::try_from(bytes.len()).expect("slice length fits u64"))
                    .and_then(|value| i64::try_from(value).ok())
                    .ok_or(ProviderError::InvalidArguments(
                        "Block Storage stream offset overflow",
                    ))?;
                let mut file = self.file.lock().map_err(|_| ProviderError::Unavailable)?;
                file.seek(SeekFrom::Start(offset)).map_err(adapter_error)?;
                file.write_all(bytes).map_err(adapter_error)?;
                file.sync_all().map_err(adapter_error)?;
                fields.insert("offset".to_owned(), Value::Integer(next_offset));
                Ok(ProviderOutcome::result(Value::Integer(
                    i64::try_from(bytes.len()).expect("bounded binary write length fits i64"),
                ))
                .with_state(Value::Record(fields)))
            }
            ("load_block", [Value::Integer(index)]) => {
                let offset = block_offset(*index, BLOCK_SIZE)?;
                let mut file = self.file.lock().map_err(|_| ProviderError::Unavailable)?;
                file.seek(SeekFrom::Start(offset)).map_err(adapter_error)?;
                let mut block = vec![0_u8; BLOCK_SIZE];
                let mut count = 0;
                while count < block.len() {
                    match file.read(&mut block[count..]).map_err(adapter_error)? {
                        0 => break,
                        read => count += read,
                    }
                }
                Ok(ProviderOutcome::result(Value::Bytes(block)))
            }
            ("store_block", [Value::Integer(index), Value::Bytes(bytes)]) => {
                if bytes.len() != BLOCK_SIZE {
                    return Err(ProviderError::InvalidArguments(
                        "store_block requires exactly 4096 Bytes",
                    ));
                }
                let offset = block_offset(*index, BLOCK_SIZE)?;
                let mut file = self.file.lock().map_err(|_| ProviderError::Unavailable)?;
                file.seek(SeekFrom::Start(offset)).map_err(adapter_error)?;
                file.write_all(bytes).map_err(adapter_error)?;
                file.sync_all().map_err(adapter_error)?;
                Ok(ProviderOutcome::result(Value::Integer(
                    i64::try_from(bytes.len()).expect("block size fits i64"),
                )))
            }
            _ => Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        }
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["input", "output", "load_block", "store_block"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

fn storage_state(state: &Value) -> Result<BTreeMap<String, Value>, ProviderError> {
    match state {
        Value::Map(fields) | Value::Record(fields) => Ok(fields.clone()),
        _ => Err(ProviderError::InvalidArguments(
            "Block Storage state must be a Record",
        )),
    }
}

fn stream_offset(fields: &BTreeMap<String, Value>) -> Result<u64, ProviderError> {
    match fields.get("offset") {
        None => Ok(0),
        Some(Value::Integer(offset)) => u64::try_from(offset.to_owned()).map_err(|_| {
            ProviderError::InvalidArguments("Block Storage stream offset must be non-negative")
        }),
        _ => Err(ProviderError::InvalidArguments(
            "Block Storage stream offset must be an Integer",
        )),
    }
}

fn update_stream_offset(
    fields: &mut BTreeMap<String, Value>,
    offset: u64,
    count: usize,
) -> Result<(), ProviderError> {
    let next = offset
        .checked_add(u64::try_from(count).expect("read count fits u64"))
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(ProviderError::InvalidArguments(
            "Block Storage stream offset overflow",
        ))?;
    fields.insert("offset".to_owned(), Value::Integer(next));
    Ok(())
}

fn block_offset(index: i64, block_size: usize) -> Result<u64, ProviderError> {
    let index = u64::try_from(index)
        .map_err(|_| ProviderError::InvalidArguments("block index must be non-negative"))?;
    index
        .checked_mul(u64::try_from(block_size).expect("block size fits u64"))
        .ok_or_else(|| ProviderError::Adapter("block offset overflow".to_owned()))
}

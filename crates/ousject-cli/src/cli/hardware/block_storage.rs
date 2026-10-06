use super::super::{
    BTreeSet, DEVICE_BLOCK_STORAGE_TYPE, Mutex, ObjectId, ObjectProvider, OpenOptions, PathBuf,
    ProviderError, ProviderOutcome, Read, Seek, SeekFrom, Value, Write,
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
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        const BLOCK_SIZE: usize = 4096;
        match (capability, arguments) {
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
        ["load_block", "store_block"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

fn block_offset(index: i64, block_size: usize) -> Result<u64, ProviderError> {
    let index = u64::try_from(index)
        .map_err(|_| ProviderError::InvalidArguments("block index must be non-negative"))?;
    index
        .checked_mul(u64::try_from(block_size).expect("block size fits u64"))
        .ok_or_else(|| ProviderError::Adapter("block offset overflow".to_owned()))
}

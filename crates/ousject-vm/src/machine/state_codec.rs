
#[allow(clippy::too_many_lines)]
fn decode_process_state(bytes: &[u8]) -> Result<ProcessState, VmError> {
    let mut reader = StateReader::new(bytes);
    if reader.take(4)? != PROCESS_MAGIC {
        return Err(invalid_state("invalid Process state magic"));
    }
    let program = ObjectId::from_u128(reader.u128()?);
    let subject = SubjectId::from_u128(reader.u128()?);
    let token_position = reader.u32()?;
    let mut status = match reader.u8()? {
        0 => ProcessStatus::Running,
        1 => ProcessStatus::Suspended,
        2 => ProcessStatus::Halted,
        3 => ProcessStatus::Terminated,
        4 => ProcessStatus::Failed,
        5 => ProcessStatus::Ready,
        6 => ProcessStatus::Waiting,
        _ => return Err(invalid_state("invalid Process status")),
    };
    let mut stack = Vec::new();
    for _ in 0..reader.count()? {
        stack.push(decode_value(&mut reader)?);
    }
    let mut variables = BTreeMap::new();
    for _ in 0..reader.count()? {
        let name = reader.string()?;
        let object = ObjectId::from_u128(reader.u128()?);
        if variables.insert(name, object).is_some() {
            return Err(invalid_state("duplicate variable name"));
        }
    }
    let mut frames = Vec::new();
    for _ in 0..reader.count()? {
        let return_position = reader.u32()?;
        let stack_base = reader.u32()?;
        let mut locals = BTreeMap::new();
        for _ in 0..reader.count()? {
            let name = reader.string()?;
            let object = ObjectId::from_u128(reader.u128()?);
            if locals.insert(name, object).is_some() {
                return Err(invalid_state("duplicate local variable name"));
            }
        }
        let receiver = match reader.u8()? {
            0 => None,
            1 => Some(ObjectId::from_u128(reader.u128()?)),
            _ => return Err(invalid_state("invalid receiver marker")),
        };
        let class = match reader.u8()? {
            0 => None,
            1 => Some(reader.string()?),
            _ => return Err(invalid_state("invalid class marker")),
        };
        frames.push(CallFrame {
            return_position,
            stack_base,
            locals,
            receiver,
            class,
        });
    }
    let mut handlers = Vec::new();
    for _ in 0..reader.count()? {
        handlers.push(ExceptionHandler {
            catch_position: reader.u32()?,
            error_name: reader.string()?,
            frame_depth: reader.u32()?,
            stack_base: reader.u32()?,
        });
    }
    let (result, error, legacy_wake, ended_at_unix_ms) = if reader.is_empty() {
        (None, None, None, None)
    } else {
        if reader.take(4)? != PROCESS_RESULT_EXTENSION {
            return Err(invalid_state("unknown Process state extension"));
        }
        let result = decode_optional_value(&mut reader)?;
        let error = decode_optional_value(&mut reader)?;
        let (wake_at_unix_ms, ended_at_unix_ms) = if reader.is_empty() {
            (None, None)
        } else {
            if reader.take(4)? != PROCESS_RUNTIME_EXTENSION {
                return Err(invalid_state("unknown Process runtime extension"));
            }
            (
                decode_state_optional_u64(&mut reader)?,
                decode_optional_u64(&mut reader)?,
            )
        };
        (result, error, wake_at_unix_ms, ended_at_unix_ms)
    };
    let (wait_reason, lease_owner, lease_generation, lease_deadline_unix_ms) =
        if reader.is_empty() {
            (
                legacy_wake.map_or(WaitReason::None, |deadline_unix_ms| WaitReason::Timer {
                    timer: None,
                    deadline_unix_ms,
                }),
                None,
                0,
                None,
            )
        } else {
            if reader.take(4)? != PROCESS_SCHEDULER_EXTENSION {
                return Err(invalid_state("unknown Process scheduler extension"));
            }
            let wait_reason = decode_wait_reason(&mut reader)?;
            let lease_owner = match reader.u8()? {
                0 => None,
                1 => Some(ObjectId::from_u128(reader.u128()?)),
                _ => return Err(invalid_state("invalid Worker lease owner marker")),
            };
            (
                wait_reason,
                lease_owner,
                reader.u64()?,
                decode_optional_u64(&mut reader)?,
            )
        };
    if legacy_wake.is_some() && status == ProcessStatus::Suspended {
        status = ProcessStatus::Waiting;
    }
    if !reader.is_empty() {
        return Err(invalid_state("trailing Process state data"));
    }
    Ok(ProcessState {
        program,
        subject,
        token_position,
        stack,
        variables,
        status,
        wait_reason,
        lease_owner,
        lease_generation,
        lease_deadline_unix_ms,
        result,
        error,
        ended_at_unix_ms,
        frames,
        handlers,
    })
}

fn decode_wait_reason(reader: &mut StateReader<'_>) -> Result<WaitReason, VmError> {
    match reader.u8()? {
        0 => Ok(WaitReason::None),
        1 => {
            let timer = match reader.u8()? {
                0 => None,
                1 => Some(ObjectId::from_u128(reader.u128()?)),
                _ => return Err(invalid_state("invalid Timer wait Object marker")),
            };
            Ok(WaitReason::Timer {
                timer,
                deadline_unix_ms: reader.u64()?,
            })
        }
        2 => Ok(WaitReason::Ipc(ObjectId::from_u128(reader.u128()?))),
        3 => Ok(WaitReason::Effect(ObjectId::from_u128(reader.u128()?))),
        4 => Ok(WaitReason::Input(ObjectId::from_u128(reader.u128()?))),
        5 => Ok(WaitReason::Process(ObjectId::from_u128(reader.u128()?))),
        _ => Err(invalid_state("invalid Process wait reason")),
    }
}

fn decode_state_optional_u64(reader: &mut StateReader<'_>) -> Result<Option<u64>, VmError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.u64()?)),
        _ => Err(invalid_state("invalid optional integer marker")),
    }
}

fn decode_value_state(bytes: &[u8]) -> Result<Value, VmError> {
    Value::decode(bytes).map_err(VmError::from)
}

fn encode_value(bytes: &mut Vec<u8>, value: &Value) -> Result<(), VmError> {
    let encoded = value.encode()?;
    write_u32(bytes, state_len(encoded.len())?);
    bytes.extend_from_slice(&encoded);
    Ok(())
}

fn decode_value(reader: &mut StateReader<'_>) -> Result<Value, VmError> {
    Value::decode(reader.bytes()?).map_err(VmError::from)
}

fn write_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), VmError> {
    write_u32(bytes, state_len(value.len())?);
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u128(bytes: &mut Vec<u8>, value: u128) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn state_len(value: usize) -> Result<u32, VmError> {
    if value > MAX_STATE_ITEMS {
        return Err(invalid_state("VM state item limit exceeded"));
    }
    u32::try_from(value).map_err(|_| invalid_state("VM state is too large"))
}

fn invalid_state(message: &str) -> VmError {
    VmError::InvalidProcessState(message.to_owned())
}

struct StateReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> StateReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], VmError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| invalid_state("VM state position overflow"))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| invalid_state("truncated VM state"))?;
        self.position = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, VmError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, VmError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, VmError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn u128(&mut self) -> Result<u128, VmError> {
        let mut bytes = [0; 16];
        bytes.copy_from_slice(self.take(16)?);
        Ok(u128::from_le_bytes(bytes))
    }

    fn count(&mut self) -> Result<usize, VmError> {
        let value = usize::try_from(self.u32()?)
            .map_err(|_| invalid_state("VM state count is unsupported"))?;
        if value > MAX_STATE_ITEMS {
            return Err(invalid_state("VM state item limit exceeded"));
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<String, VmError> {
        let length = self.count()?;
        let value = std::str::from_utf8(self.take(length)?)
            .map_err(|_| invalid_state("VM state string is not UTF-8"))?;
        Ok(value.to_owned())
    }

    fn bytes(&mut self) -> Result<&'a [u8], VmError> {
        let length = self.count()?;
        self.take(length)
    }

    fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
}

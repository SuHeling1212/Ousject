fn encode_process_state(state: &ProcessState) -> Result<Vec<u8>, VmError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(PROCESS_MAGIC);
    write_u128(&mut bytes, state.program.as_u128());
    write_u128(&mut bytes, state.subject.as_u128());
    write_u32(&mut bytes, state.token_position);
    bytes.push(match state.status {
        ProcessStatus::Running => 0,
        ProcessStatus::Suspended => 1,
        ProcessStatus::Halted => 2,
        ProcessStatus::Terminated => 3,
        ProcessStatus::Failed => 4,
    });
    write_u32(&mut bytes, state_len(state.stack.len())?);
    for value in &state.stack {
        encode_value(&mut bytes, value)?;
    }
    write_u32(&mut bytes, state_len(state.variables.len())?);
    for (name, object) in &state.variables {
        write_string(&mut bytes, name)?;
        write_u128(&mut bytes, object.as_u128());
    }
    write_u32(&mut bytes, state_len(state.frames.len())?);
    for frame in &state.frames {
        write_u32(&mut bytes, frame.return_position);
        write_u32(&mut bytes, frame.stack_base);
        write_u32(&mut bytes, state_len(frame.locals.len())?);
        for (name, object) in &frame.locals {
            write_string(&mut bytes, name)?;
            write_u128(&mut bytes, object.as_u128());
        }
        match frame.receiver {
            Some(receiver) => {
                bytes.push(1);
                write_u128(&mut bytes, receiver.as_u128());
            }
            None => bytes.push(0),
        }
        match &frame.class {
            Some(class) => {
                bytes.push(1);
                write_string(&mut bytes, class)?;
            }
            None => bytes.push(0),
        }
    }
    write_u32(&mut bytes, state_len(state.handlers.len())?);
    for handler in &state.handlers {
        write_u32(&mut bytes, handler.catch_position);
        write_string(&mut bytes, &handler.error_name)?;
        write_u32(&mut bytes, handler.frame_depth);
        write_u32(&mut bytes, handler.stack_base);
    }
    bytes.extend_from_slice(PROCESS_RESULT_EXTENSION);
    encode_optional_value(&mut bytes, state.result.as_ref())?;
    encode_optional_value(&mut bytes, state.error.as_ref())?;
    bytes.extend_from_slice(PROCESS_RUNTIME_EXTENSION);
    encode_optional_u64(&mut bytes, state.wake_at_unix_ms);
    encode_optional_u64(&mut bytes, state.ended_at_unix_ms);
    Ok(bytes)
}

fn encode_optional_value(bytes: &mut Vec<u8>, value: Option<&Value>) -> Result<(), VmError> {
    match value {
        Some(value) => {
            bytes.push(1);
            encode_value(bytes, value)?;
        }
        None => bytes.push(0),
    }
    Ok(())
}

fn decode_optional_value(reader: &mut StateReader<'_>) -> Result<Option<Value>, VmError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(decode_value(reader)?)),
        _ => Err(invalid_state("invalid Process optional-value marker")),
    }
}

fn encode_optional_u64(bytes: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            bytes.push(1);
            write_u64(bytes, value);
        }
        None => bytes.push(0),
    }
}

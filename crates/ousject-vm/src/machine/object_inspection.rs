
#[allow(clippy::too_many_lines)]
fn object_essential_value(view: &oms_runtime::ObjectView) -> Result<Value, VmError> {
    if view.header().type_id != PROCESS_TYPE {
        return Ok(
            Value::decode(view.state()).unwrap_or_else(|_| Value::Bytes(view.state().to_vec()))
        );
    }

    let process = decode_process_state(view.state())?;
    let variables = process
        .variables
        .iter()
        .map(|(name, object)| (name.clone(), Value::Text(object.to_string())))
        .collect();
    let frames = process
        .frames
        .iter()
        .map(|frame| {
            Value::Record(BTreeMap::from([
                (
                    "return_position".to_owned(),
                    Value::Integer(i64::from(frame.return_position)),
                ),
                (
                    "stack_base".to_owned(),
                    Value::Integer(i64::from(frame.stack_base)),
                ),
                (
                    "locals".to_owned(),
                    Value::Record(
                        frame
                            .locals
                            .iter()
                            .map(|(name, object)| (name.clone(), Value::Text(object.to_string())))
                            .collect(),
                    ),
                ),
                (
                    "receiver".to_owned(),
                    frame
                        .receiver
                        .map_or(Value::Null, |object| Value::Text(object.to_string())),
                ),
                (
                    "class".to_owned(),
                    frame
                        .class
                        .as_ref()
                        .map_or(Value::Null, |class| Value::Text(class.clone())),
                ),
            ]))
        })
        .collect();
    let handlers = process
        .handlers
        .iter()
        .map(|handler| {
            Value::Record(BTreeMap::from([
                (
                    "catch_position".to_owned(),
                    Value::Integer(i64::from(handler.catch_position)),
                ),
                ("error".to_owned(), Value::Text(handler.error_name.clone())),
                (
                    "frame_depth".to_owned(),
                    Value::Integer(i64::from(handler.frame_depth)),
                ),
                (
                    "stack_base".to_owned(),
                    Value::Integer(i64::from(handler.stack_base)),
                ),
            ]))
        })
        .collect();
    Ok(Value::Record(BTreeMap::from([
        (
            "program".to_owned(),
            Value::Text(process.program.to_string()),
        ),
        (
            "subject".to_owned(),
            Value::Text(process.subject.to_string()),
        ),
        (
            "position".to_owned(),
            Value::Integer(i64::from(process.token_position)),
        ),
        (
            "status".to_owned(),
            Value::Text(process_status_name(process.status).to_owned()),
        ),
        (
            "wake_at_unix_ms".to_owned(),
            process.wake_at_unix_ms.map_or(Value::Null, |value| {
                Value::Integer(i64::try_from(value).unwrap_or(i64::MAX))
            }),
        ),
        (
            "ended_at_unix_ms".to_owned(),
            process.ended_at_unix_ms.map_or(Value::Null, |value| {
                Value::Integer(i64::try_from(value).unwrap_or(i64::MAX))
            }),
        ),
        ("stack".to_owned(), Value::Array(process.stack)),
        ("variables".to_owned(), Value::Record(variables)),
        ("frames".to_owned(), Value::Array(frames)),
        ("handlers".to_owned(), Value::Array(handlers)),
        (
            "result".to_owned(),
            process.result.clone().unwrap_or(Value::Null),
        ),
        (
            "error".to_owned(),
            process.error.clone().unwrap_or(Value::Null),
        ),
    ])))
}

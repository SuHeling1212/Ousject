fn is_catchable(error: &VmError) -> bool {
    match error {
        VmError::StackUnderflow
        | VmError::UndefinedVariable(_)
        | VmError::TypeError(_)
        | VmError::DivisionByZero
        | VmError::IndexOutOfBounds
        | VmError::MissingKey(_)
        | VmError::MissingProvider(_)
        | VmError::Provider(_) => true,
        VmError::Oms(error) => matches!(
            error,
            OmsError::NotFound(_)
                | OmsError::Denied { .. }
                | OmsError::InvalidLifecycle { .. }
                | OmsError::InvalidOperation(_)
                | OmsError::UnknownTypeName(_)
                | OmsError::UnknownType(_)
                | OmsError::TypeCreationDenied(_)
                | OmsError::ValueSchemaMismatch { .. }
                | OmsError::InvalidValue(_)
                | OmsError::InvalidName(_)
                | OmsError::NameNotFound { .. }
        ),
        VmError::Tf(_)
        | VmError::Value(_)
        | VmError::InvalidProcessState(_)
        | VmError::TokenPositionOutOfRange(_)
        | VmError::StepLimitExceeded(_)
        | VmError::WorkerLeaseBusy(_)
        | VmError::WorkerLeaseExpired(_) => false,
    }
}

fn error_value(error: &VmError) -> Value {
    let code = match error {
        VmError::Oms(_) => "object_error",
        VmError::StackUnderflow => "stack_underflow",
        VmError::UndefinedVariable(_) => "undefined_variable",
        VmError::TypeError(_) => "type_error",
        VmError::DivisionByZero => "division_by_zero",
        VmError::IndexOutOfBounds => "index_out_of_bounds",
        VmError::MissingKey(_) => "missing_key",
        VmError::MissingProvider(_) => "missing_provider",
        VmError::Provider(_) => "provider_error",
        VmError::Tf(_) => "tf_error",
        VmError::Value(_) => "value_error",
        VmError::InvalidProcessState(_) => "invalid_process_state",
        VmError::TokenPositionOutOfRange(_) => "token_position_out_of_range",
        VmError::StepLimitExceeded(_) => "step_limit_exceeded",
        VmError::WorkerLeaseBusy(_) => "worker_lease_busy",
        VmError::WorkerLeaseExpired(_) => "worker_lease_expired",
    };
    Value::Error {
        code: code.to_owned(),
        message: error.to_string(),
    }
}

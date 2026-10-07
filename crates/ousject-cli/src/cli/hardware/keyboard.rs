use super::super::{
    Arc, BTreeSet, DEVICE_KEYBOARD_TYPE, LinuxTerminal, ObjectId, ObjectProvider, ProviderError,
    ProviderOutcome, Value,
};

#[derive(Debug)]
pub(crate) struct HostKeyboardProvider {
    pub(crate) terminal: Arc<LinuxTerminal>,
}

impl ObjectProvider for HostKeyboardProvider {
    fn type_id(&self) -> oms_types::TypeId {
        DEVICE_KEYBOARD_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Keyboard Objects are published by hardware discovery",
        ))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        _arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        Err(ProviderError::Adapter(format!(
            "keyboard.{capability} requires a Process input lease"
        )))
    }

    fn invoke_for_process(
        &self,
        process: ObjectId,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        match (capability, arguments) {
            ("capture", []) => {
                self.terminal.capture_keyboard(process)?;
                Ok(ProviderOutcome::result(Value::Null))
            }
            ("release", []) => {
                self.terminal.release_keyboard(process)?;
                Ok(ProviderOutcome::result(Value::Null))
            }
            ("next_event", []) => self
                .terminal
                .take_key_event(process)?
                .map(ProviderOutcome::result)
                .ok_or(ProviderError::Pending),
            ("poll_event", []) => Ok(ProviderOutcome::result(
                self.terminal
                    .take_key_event(process)?
                    .unwrap_or(Value::Null),
            )),
            _ => Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        }
    }

    fn ephemeral_capabilities(&self) -> BTreeSet<String> {
        BTreeSet::from(["poll_events".to_owned()])
    }

    fn invoke_ephemeral_for_process(
        &self,
        process: ObjectId,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
    ) -> Result<ProviderOutcome, ProviderError> {
        let ("poll_events", [Value::Integer(maximum)]) = (capability, arguments) else {
            return Err(ProviderError::InvalidArguments(
                "keyboard.poll_events expects a maximum event count",
            ));
        };
        let maximum = usize::try_from(*maximum).map_err(|_| {
            ProviderError::InvalidArguments("event count must be between 1 and 256")
        })?;
        if !(1..=256).contains(&maximum) {
            return Err(ProviderError::InvalidArguments(
                "event count must be between 1 and 256",
            ));
        }
        let events = self.terminal.take_key_events(process, maximum)?;
        Ok(ProviderOutcome::result(Value::Array(events)))
    }

    fn process_ended(&self, process: ObjectId) {
        self.terminal.release_process_input(process);
    }

    fn capabilities(&self) -> BTreeSet<String> {
        [
            "capture",
            "release",
            "next_event",
            "poll_event",
            "poll_events",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

#![allow(clippy::wildcard_imports)]

use super::*;

const TIMER_STATUS_ARMED: &str = "armed";
const TIMER_STATUS_FIRED: &str = "fired";
const TIMER_STATUS_CANCELLED: &str = "cancelled";
const TIMER_INTERNAL_FIELD: &str = "internal_sleep";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TimerState {
    pub(super) status: TimerObjectStatus,
    pub(super) deadline_unix_ms: Option<u64>,
    pub(super) internal_sleep: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimerObjectStatus {
    Armed,
    Fired,
    Cancelled,
}

impl TimerState {
    fn value(self) -> Result<Value, VmError> {
        let deadline = self
            .deadline_unix_ms
            .map(|value| {
                i64::try_from(value)
                    .map(Value::Integer)
                    .map_err(|_| VmError::TypeError("Timer deadline is out of range"))
            })
            .transpose()?
            .unwrap_or(Value::Null);
        Ok(Value::Record(BTreeMap::from([
            (
                "status".to_owned(),
                Value::Text(self.status.name().to_owned()),
            ),
            ("deadline_unix_ms".to_owned(), deadline),
            (
                TIMER_INTERNAL_FIELD.to_owned(),
                Value::Bool(self.internal_sleep),
            ),
        ])))
    }

    pub(super) fn decode(value: &Value) -> Result<Self, VmError> {
        let (Value::Record(fields) | Value::Map(fields)) = value else {
            return Err(VmError::TypeError("Timer state must be a Record"));
        };
        let status = match fields.get("status") {
            Some(Value::Text(value)) if value == TIMER_STATUS_ARMED => TimerObjectStatus::Armed,
            Some(Value::Text(value)) if value == TIMER_STATUS_FIRED => TimerObjectStatus::Fired,
            Some(Value::Text(value)) if value == TIMER_STATUS_CANCELLED => {
                TimerObjectStatus::Cancelled
            }
            _ => return Err(VmError::TypeError("Timer has an invalid status")),
        };
        let deadline_unix_ms = match fields.get("deadline_unix_ms") {
            Some(Value::Integer(value)) => Some(
                u64::try_from(*value)
                    .map_err(|_| VmError::TypeError("Timer deadline is invalid"))?,
            ),
            Some(Value::Null) | None => None,
            _ => return Err(VmError::TypeError("Timer deadline is invalid")),
        };
        if (status == TimerObjectStatus::Armed) != deadline_unix_ms.is_some() {
            return Err(VmError::TypeError("Timer status and deadline disagree"));
        }
        let internal_sleep = match fields.get(TIMER_INTERNAL_FIELD) {
            Some(Value::Bool(value)) => *value,
            None => false,
            _ => return Err(VmError::TypeError("Timer internal marker is invalid")),
        };
        Ok(Self {
            status,
            deadline_unix_ms,
            internal_sleep,
        })
    }

    const fn name(self) -> &'static str {
        self.status.name()
    }
}

impl TimerObjectStatus {
    const fn name(self) -> &'static str {
        match self {
            Self::Armed => TIMER_STATUS_ARMED,
            Self::Fired => TIMER_STATUS_FIRED,
            Self::Cancelled => TIMER_STATUS_CANCELLED,
        }
    }
}

impl VirtualMachine {
    pub(super) fn prepare_timer_object(
        &self,
        initial: &Value,
        parent: ObjectId,
    ) -> Result<CreateObject, VmError> {
        let initial = match initial {
            Value::Map(fields) | Value::Record(fields) if fields.is_empty() => TimerState {
                status: TimerObjectStatus::Cancelled,
                deadline_unix_ms: None,
                internal_sleep: false,
            }
            .value()?,
            Value::Map(_) | Value::Record(_) => {
                TimerState::decode(initial)?;
                initial.clone()
            }
            _ => return Err(VmError::TypeError("Timer initial state must be a Record")),
        };
        self.manager
            .prepare_create(CreateSpec::new("core.timer", initial).with_parent(parent))
            .map_err(Into::into)
    }

    pub(super) fn prepare_sleep_timer(
        &self,
        process: ObjectId,
        deadline_unix_ms: u64,
    ) -> Result<CreateObject, VmError> {
        let state = TimerState {
            status: TimerObjectStatus::Armed,
            deadline_unix_ms: Some(deadline_unix_ms),
            internal_sleep: true,
        };
        self.manager
            .prepare_create(CreateSpec::new("core.timer", state.value()?).with_parent(process))
            .map_err(Into::into)
    }

    pub(super) fn invoke_timer(
        &self,
        process: ObjectId,
        process_state: &mut ProcessState,
        timer: ObjectId,
        capability: &str,
        arguments: &[Value],
        transaction: &mut Transaction,
    ) -> Result<(Value, Option<String>), VmError> {
        let view = self.manager.read(self.context, timer)?;
        let mut timer_state = TimerState::decode(&self.manager.value(self.context, timer)?)?;
        match (capability, arguments) {
            ("status", []) => Ok((Value::Text(timer_state.name().to_owned()), None)),
            ("arm", [Value::Integer(milliseconds)]) => {
                let milliseconds = u64::try_from(*milliseconds)
                    .map_err(|_| VmError::TypeError("Timer duration must not be negative"))?;
                let deadline_unix_ms = unix_time_millis()
                    .checked_add(milliseconds)
                    .ok_or(VmError::TypeError("Timer deadline is too large"))?;
                timer_state.status = TimerObjectStatus::Armed;
                timer_state.deadline_unix_ms = Some(deadline_unix_ms);
                timer_state.internal_sleep = false;
                transaction
                    .expect(timer, view.header().version)
                    .update_state(timer, timer_state.value()?.encode()?);
                Ok((Value::Null, None))
            }
            ("cancel", []) => {
                timer_state.status = TimerObjectStatus::Cancelled;
                timer_state.deadline_unix_ms = None;
                timer_state.internal_sleep = false;
                transaction
                    .expect(timer, view.header().version)
                    .update_state(timer, timer_state.value()?.encode()?);
                self.wake_timer_waiters(timer, &view, transaction)?;
                Ok((Value::Null, None))
            }
            ("wait", []) => {
                if timer_state.status != TimerObjectStatus::Armed {
                    return Ok((Value::Text(timer_state.name().to_owned()), None));
                }
                let deadline_unix_ms = timer_state
                    .deadline_unix_ms
                    .ok_or(VmError::TypeError("Armed Timer has no deadline"))?;
                if deadline_unix_ms <= unix_time_millis() {
                    timer_state.status = TimerObjectStatus::Fired;
                    timer_state.deadline_unix_ms = None;
                    transaction
                        .expect(timer, view.header().version)
                        .update_state(timer, timer_state.value()?.encode()?);
                    self.wake_timer_waiters(timer, &view, transaction)?;
                    return Ok((Value::Text(TIMER_STATUS_FIRED.to_owned()), None));
                }
                transaction
                    .expect(timer, view.header().version)
                    .set_link(timer, format!("$wait:{process}"), process)
                    .set_link(process, "$waiting_on", timer);
                process_state.status = ProcessStatus::Waiting;
                process_state.wait_reason = WaitReason::Timer {
                    timer: Some(timer),
                    deadline_unix_ms,
                };
                Ok((Value::Null, None))
            }
            _ => Err(VmError::TypeError(
                "Timer expects arm(milliseconds), wait(), cancel(), or status()",
            )),
        }
    }

    fn wake_timer_waiters(
        &self,
        timer: ObjectId,
        view: &oms_runtime::ObjectView,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        for (name, process) in view
            .links()
            .iter()
            .filter(|(name, _)| name.starts_with("$wait:"))
        {
            let process_view = self
                .manager
                .read(AccessContext::new(SYSTEM_SUBJECT), *process)?;
            let mut state = decode_process_state(process_view.state())?;
            if state.status == ProcessStatus::Waiting
                && matches!(state.wait_reason, WaitReason::Timer { timer: Some(id), .. } if id == timer)
            {
                state.status = ProcessStatus::Ready;
                state.wait_reason = WaitReason::None;
                if state.lease_owner.is_some() {
                    state.lease_generation = state
                        .lease_generation
                        .checked_add(1)
                        .ok_or_else(|| invalid_state("Worker lease generation overflow"))?;
                }
                state.lease_owner = None;
                state.lease_deadline_unix_ms = None;
                transaction
                    .expect(*process, process_view.header().version)
                    .update_state(*process, encode_process_state(&state)?);
                transaction.remove_link(*process, "$waiting_on");
            }
            transaction
                .expect(timer, view.header().version)
                .remove_link(timer, name.clone());
        }
        Ok(())
    }

    /// Fires every durable Timer whose deadline has passed and wakes its waiters.
    ///
    /// # Errors
    ///
    /// Returns an error if a Timer or waiter is malformed or the atomic wake
    /// transaction cannot be committed.
    pub fn fire_due_timers(&self) -> Result<usize, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let timers = self.manager.query(
            system,
            &ObjectQuery::new().with_type(oms_types::CORE_TIMER_TYPE),
        )?;
        let now = unix_time_millis();
        let mut fired = 0;
        for timer in timers {
            let view = self.manager.read(system, timer.id)?;
            let mut state = TimerState::decode(&Value::decode(view.state())?)?;
            if state.status != TimerObjectStatus::Armed
                || state.deadline_unix_ms.is_none_or(|deadline| deadline > now)
            {
                continue;
            }
            state.status = TimerObjectStatus::Fired;
            state.deadline_unix_ms = None;
            let mut transaction = self.manager.begin(system);
            transaction
                .expect(timer.id, view.header().version)
                .update_state(timer.id, state.value()?.encode()?);
            self.wake_timer_waiters(timer.id, &view, &mut transaction)?;
            if state.internal_sleep {
                if let Some(process) = view.links().get("$wait:").copied().or_else(|| {
                    view.links()
                        .iter()
                        .find_map(|(name, process)| name.starts_with("$wait:").then_some(*process))
                }) {
                    transaction.remove_link(process, "$timer");
                }
                transaction.tombstone(timer.id);
            }
            self.manager.commit(transaction)?;
            fired += 1;
        }
        Ok(fired)
    }
}

#![allow(clippy::wildcard_imports)]

use super::timer::TimerState;
use super::*;

struct EffectRecoveryDecision {
    update: Option<(ObjectId, oms_types::ObjectVersion, EffectRecord)>,
    audit: Option<(&'static str, ObjectId)>,
}

impl VirtualMachine {
    // A lease spans ordinary host scheduling pauses and bounded storage stalls.
    // It is still renewed at each execution-slice boundary, while the
    // persisted generation fences a replacement Worker after expiry.
    const WORKER_LEASE_MILLIS: u64 = 30_000;

    pub(super) fn claim_worker_lease(
        &self,
        process: ObjectId,
        owner: ObjectId,
    ) -> Result<Option<WorkerLease>, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, process)?;
        let mut state = decode_process_state(view.state())?;
        let now = unix_time_millis();
        if !matches!(state.status, ProcessStatus::Ready | ProcessStatus::Running)
            || state
                .lease_deadline_unix_ms
                .is_some_and(|deadline| deadline > now)
        {
            return Ok(None);
        }
        state.lease_generation = state
            .lease_generation
            .checked_add(1)
            .ok_or_else(|| invalid_state("Worker lease generation overflow"))?;
        let deadline_unix_ms = now
            .checked_add(Self::WORKER_LEASE_MILLIS)
            .ok_or_else(|| invalid_state("Worker lease deadline overflow"))?;
        state.status = ProcessStatus::Running;
        state.lease_owner = Some(owner);
        state.lease_deadline_unix_ms = Some(deadline_unix_ms);
        let mut transaction = self.manager.begin(system);
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state)?);
        self.manager.commit(transaction)?;
        Ok(Some(WorkerLease {
            owner,
            generation: state.lease_generation,
            deadline_unix_ms,
        }))
    }

    fn renew_worker_lease(
        &self,
        process: ObjectId,
        lease: WorkerLease,
    ) -> Result<Option<WorkerLease>, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, process)?;
        let mut state = decode_process_state(view.state())?;
        let now = unix_time_millis();
        if state.lease_owner != Some(lease.owner)
            || state.lease_generation != lease.generation
            || state
                .lease_deadline_unix_ms
                .is_none_or(|deadline| deadline <= now)
            || state.status != ProcessStatus::Running
        {
            return Ok(None);
        }
        let deadline_unix_ms = now
            .checked_add(Self::WORKER_LEASE_MILLIS)
            .ok_or_else(|| invalid_state("Worker lease deadline overflow"))?;
        state.lease_deadline_unix_ms = Some(deadline_unix_ms);
        let mut transaction = self.manager.begin(system);
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state)?);
        self.manager.commit(transaction)?;
        Ok(Some(WorkerLease {
            deadline_unix_ms,
            ..lease
        }))
    }

    fn release_worker_lease(&self, process: ObjectId, lease: WorkerLease) -> Result<(), VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, process)?;
        let mut state = decode_process_state(view.state())?;
        if state.lease_owner != Some(lease.owner) || state.lease_generation != lease.generation {
            return Ok(());
        }
        state.lease_owner = None;
        state.lease_deadline_unix_ms = None;
        if state.status == ProcessStatus::Running {
            state.status = ProcessStatus::Ready;
        }
        let mut transaction = self.manager.begin(system);
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state)?);
        match self.manager.commit(transaction) {
            Ok(_) | Err(OmsError::Conflict { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Restores runnable Process state after a Store restart and invalidates
    /// every lease left by a previous Worker generation.
    ///
    /// # Errors
    ///
    /// Returns an error if a Process or durable Timer cannot be decoded or
    /// recovery changes cannot be committed.
    pub fn recover_processes(&self) -> Result<Vec<ObjectId>, VmError> {
        self.fire_due_timers()?;
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let processes = self
            .manager
            .query(system, &ObjectQuery::new().with_type(PROCESS_TYPE))?;
        let mut ready = Vec::new();
        for process in processes {
            let view = self.manager.read(system, process.id)?;
            let mut state = decode_process_state(view.state())?;
            let effect_id = view.links().get("$effect").copied();
            let mut changed = false;
            let (effect_update, effect_audit) = if let Some(effect) = effect_id {
                changed = true;
                let decision = self.recover_linked_effect(&mut state, effect)?;
                (decision.update, decision.audit)
            } else {
                (None, None)
            };
            if effect_id.is_none() && state.status == ProcessStatus::Running {
                state.status = ProcessStatus::Ready;
                changed = true;
            }

            if state.lease_owner.is_some() || state.lease_deadline_unix_ms.is_some() {
                state.lease_owner = None;
                state.lease_deadline_unix_ms = None;
                state.lease_generation = state
                    .lease_generation
                    .checked_add(1)
                    .ok_or_else(|| invalid_state("Worker lease generation overflow"))?;
                changed = true;
            }

            if state.status == ProcessStatus::Waiting && state.wait_reason == WaitReason::None {
                if let Some(effect) = view.links().get("$effect").copied() {
                    let record = EffectRecord::decode(self.manager.read(system, effect)?.state())?;
                    state.wait_reason =
                        if record.capability == "read_line" || record.capability == "read_secret" {
                            WaitReason::Input(effect)
                        } else {
                            WaitReason::Effect(effect)
                        };
                    changed = true;
                } else if let Some(waiting_on) = view.links().get("$waiting_on").copied() {
                    let type_id = self.manager.inspect(system, waiting_on)?.type_id;
                    state.wait_reason = if type_id == CORE_CHANNEL_TYPE {
                        WaitReason::Ipc(waiting_on)
                    } else if type_id == oms_types::CORE_TIMER_TYPE {
                        let timer = TimerState::decode(&self.manager.value(system, waiting_on)?)?;
                        WaitReason::Timer {
                            timer: Some(waiting_on),
                            deadline_unix_ms: timer.deadline_unix_ms.unwrap_or(0),
                        }
                    } else if type_id == PROCESS_TYPE {
                        WaitReason::Process(waiting_on)
                    } else {
                        WaitReason::None
                    };
                    changed = true;
                }
            }

            if changed {
                let mut transaction = self.manager.begin(system);
                transaction.expect(process.id, view.header().version);
                if let Some((effect, version, record)) = effect_update {
                    transaction
                        .expect(effect, version)
                        .update_state(effect, record.encode().map_err(VmError::from)?);
                }
                transaction.update_state(process.id, encode_process_state(&state)?);
                if let Some((action, effect)) = effect_audit {
                    self.stage_audit_event(
                        state.subject,
                        action,
                        effect,
                        Value::Record(BTreeMap::new()),
                        &mut transaction,
                    )?;
                }
                self.manager.commit(transaction)?;
            }
            if matches!(
                state.status,
                ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
            ) {
                self.notify_process_ended(process.id)?;
            }
            if state.status == ProcessStatus::Ready {
                ready.push(process.id);
            }
        }
        Ok(ready)
    }

    fn recover_linked_effect(
        &self,
        state: &mut ProcessState,
        effect: ObjectId,
    ) -> Result<EffectRecoveryDecision, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, effect)?;
        let record = EffectRecord::decode(view.state())?;
        let interrupted = record.status == EffectStatus::Running
            || (record.status == EffectStatus::Pending && state.status == ProcessStatus::Running);
        let update = if interrupted {
            if record.recovery_policy == EffectRecoveryPolicy::RetryIdempotent {
                state.status = ProcessStatus::Ready;
                state.wait_reason = WaitReason::None;
                Some((effect, view.header().version, record.retry()))
            } else {
                state.status = ProcessStatus::Waiting;
                state.wait_reason = WaitReason::Effect(effect);
                Some((effect, view.header().version, record.unknown()))
            }
        } else {
            match record.status {
                EffectStatus::Unknown => {
                    state.status = ProcessStatus::Waiting;
                    state.wait_reason = WaitReason::Effect(effect);
                }
                EffectStatus::Pending | EffectStatus::Completed | EffectStatus::Failed => {
                    state.status = ProcessStatus::Ready;
                    state.wait_reason = WaitReason::None;
                }
                EffectStatus::Running => unreachable!("Running Effects are interrupted"),
            }
            None
        };
        let audit = update.as_ref().map(|(_, _, record)| match record.status {
            EffectStatus::Pending => ("effect.pending", effect),
            EffectStatus::Unknown => ("effect.unknown", effect),
            _ => unreachable!("recovery only sets Pending or Unknown"),
        });
        Ok(EffectRecoveryDecision { update, audit })
    }

    /// Reconnects a persisted Process to hardware discovered during this boot.
    ///
    /// # Errors
    ///
    /// Returns an error if no provider was discovered or the Link commit fails.
    pub fn reconnect_hardware(&self, process: ObjectId) -> Result<(), VmError> {
        let terminal = self
            .terminal_provider
            .ok_or(VmError::MissingProvider("terminal"))?;
        let view = self.manager.read(self.context, process)?;
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, view.header().version)
            .set_link(process, "terminal", terminal);
        self.manager.commit(transaction)?;
        Ok(())
    }

    /// Marks a Process suspended on a pending Provider Effect as runnable so
    /// the same idempotent Effect can be polled again.
    ///
    /// Returns `true` only when a pending Effect was found and the Process was
    /// made runnable. No new Effect is created.
    ///
    /// # Errors
    ///
    /// Returns an error when the Process or its persisted state cannot be
    /// read or atomically updated.
    pub fn poll_pending_effect(&self, process: ObjectId) -> Result<bool, VmError> {
        let view = self.manager.read(self.context, process)?;
        let mut state = decode_process_state(view.state())?;
        if state.status != ProcessStatus::Waiting {
            return Ok(false);
        }
        let Some(effect) = view.links().get("$effect").copied() else {
            return Ok(false);
        };
        if EffectRecord::decode(self.manager.read(self.context, effect)?.state())?.status
            == EffectStatus::Unknown
        {
            return Ok(false);
        }
        state.status = ProcessStatus::Ready;
        state.wait_reason = WaitReason::None;
        state.lease_owner = None;
        state.lease_deadline_unix_ms = None;
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state)?);
        self.manager.commit(transaction)?;
        Ok(true)
    }

    /// Returns the remaining delay for a timer-suspended Process, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the Process state cannot be read or decoded.
    pub fn time_until_wake(
        &self,
        process: ObjectId,
    ) -> Result<Option<std::time::Duration>, VmError> {
        let state = self.process_state(process)?;
        Ok(timer_deadline(&state.wait_reason).map(|deadline| {
            std::time::Duration::from_millis(deadline.saturating_sub(unix_time_millis()))
        }))
    }

    /// Makes a timer-suspended Process runnable once its persisted deadline is
    /// reached. The wakeup is an atomic Process-state update.
    ///
    /// # Errors
    ///
    /// Returns an error if the Process state cannot be read, encoded or saved.
    pub fn wake_due_timer(&self, process: ObjectId) -> Result<bool, VmError> {
        let view = self.manager.read(self.context, process)?;
        let mut state = decode_process_state(view.state())?;
        let WaitReason::Timer {
            timer,
            deadline_unix_ms,
        } = state.wait_reason
        else {
            return Ok(false);
        };
        if state.status != ProcessStatus::Waiting || deadline_unix_ms > unix_time_millis() {
            return Ok(false);
        }
        if timer.is_some() {
            self.fire_due_timers()?;
            let after = self.process_state(process)?;
            return Ok(after.status != ProcessStatus::Waiting);
        }
        state.status = ProcessStatus::Ready;
        state.wait_reason = WaitReason::None;
        state.lease_owner = None;
        state.lease_deadline_unix_ms = None;
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state)?);
        self.manager.commit(transaction)?;
        Ok(true)
    }

    /// Executes or resumes a Process for at most `step_limit` Tokens.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid Process state, invalid operations or OMS
    /// failures. Reaching the step limit releases the Worker lease and returns
    /// the Process to `Ready` for another scheduling turn.
    pub fn run(&self, process: ObjectId, step_limit: u64) -> Result<RunReport, VmError> {
        let mut total_steps = 0;
        let mut output = Vec::new();
        loop {
            let remaining = step_limit.saturating_sub(total_steps);
            let report = self.run_slice(process, remaining)?;
            total_steps = total_steps.saturating_add(report.steps);
            output.extend(report.output);

            let state = self.process_state(process)?;
            let WaitReason::Process(child) = state.wait_reason else {
                return Ok(RunReport {
                    process,
                    steps: total_steps,
                    status: state.status,
                    output,
                });
            };
            if state.status != ProcessStatus::Waiting {
                return Ok(RunReport {
                    process,
                    steps: total_steps,
                    status: state.status,
                    output,
                });
            }

            CooperativeScheduler::new(self).run_process_wait(process, child)?;
            let state = self.process_state(process)?;
            if state.status != ProcessStatus::Ready || total_steps >= step_limit {
                return Ok(RunReport {
                    process,
                    steps: total_steps,
                    status: state.status,
                    output,
                });
            }
        }
    }

    /// Executes one leased Process slice without driving any Process it waits
    /// for. The cooperative scheduler uses this primitive to keep leases
    /// scoped to one Process at a time.
    pub(super) fn run_slice(
        &self,
        process: ObjectId,
        step_limit: u64,
    ) -> Result<RunReport, VmError> {
        let current = self.process_state(process)?;
        if current.status == ProcessStatus::Waiting
            && timer_deadline(&current.wait_reason)
                .is_some_and(|deadline| deadline <= unix_time_millis())
        {
            self.wake_due_timer(process)?;
        }
        let current = self.process_state(process)?;
        if !matches!(
            current.status,
            ProcessStatus::Ready | ProcessStatus::Running
        ) {
            return Ok(RunReport {
                process,
                steps: 0,
                status: current.status,
                output: Vec::new(),
            });
        }
        self.providers.seal().map_err(VmError::from)?;
        let owner = ObjectId::new();
        let Some(mut lease) = self.claim_worker_lease(process, owner)? else {
            let state = self.process_state(process)?;
            return Ok(RunReport {
                process,
                steps: 0,
                status: state.status,
                output: Vec::new(),
            });
        };
        let mut output = Vec::new();
        let mut steps = 0;
        let mut renew = false;
        let execution = (|| loop {
            let mut state = self.process_state(process)?;
            if state.status != ProcessStatus::Running || steps >= step_limit {
                break Ok(());
            }
            if renew {
                let Some(refreshed) = self.renew_worker_lease(process, lease)? else {
                    return Err(VmError::WorkerLeaseExpired(process));
                };
                lease = refreshed;
                state = self.process_state(process)?;
                if state.status != ProcessStatus::Running {
                    break Ok(());
                }
            }
            let executor = self.for_subject(state.subject);
            let (line, executed) = executor.step(process, state, step_limit - steps, lease)?;
            if let Some(line) = line {
                output.push(line);
            }
            steps += executed;
            renew = true;
        })();
        let release = self.release_worker_lease(process, lease);
        execution?;
        release?;
        let state = self.process_state(process)?;
        Ok(RunReport {
            process,
            steps,
            status: state.status,
            output,
        })
    }

    pub(super) fn for_subject(&self, subject: SubjectId) -> Self {
        Self {
            manager: Arc::clone(&self.manager),
            context: AccessContext::new(subject),
            terminal_provider: self.terminal_provider,
            terminal_driver: self.terminal_driver.clone(),
            kernel_services: self.kernel_services.clone(),
            providers: Arc::clone(&self.providers),
            program_cache: Arc::clone(&self.program_cache),
            compilation_cache: Arc::clone(&self.compilation_cache),
            package_verification_cache: Arc::clone(&self.package_verification_cache),
            process_reaper: Arc::clone(&self.process_reaper),
        }
    }

    /// Reads and decodes Process execution state.
    ///
    /// # Errors
    ///
    /// Returns an error if OMS access or state decoding fails.
    pub fn process_state(&self, process: ObjectId) -> Result<ProcessState, VmError> {
        let view = self.manager.read(self.context, process)?;
        decode_process_state(view.state())
    }

    pub(super) fn program(&self, object: ObjectId) -> Result<Arc<Program>, VmError> {
        let view = self.manager.read(self.context, object)?;
        let version = view.header().version;
        let mut cache = self
            .program_cache
            .lock()
            .map_err(|_| VmError::InvalidProcessState("Program cache unavailable".to_owned()))?;
        if let Some(program) = cache.get(object, version) {
            return Ok(program);
        }
        let program = Arc::new(Program::decode(view.state())?);
        cache.insert(object, version, Arc::clone(&program));
        Ok(program)
    }

    /// Reads a Process Variable Object by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the variable is absent or its Object state is invalid.
    pub fn variable(&self, process: ObjectId, name: &str) -> Result<Value, VmError> {
        let state = self.process_state(process)?;
        let object = state
            .variables
            .get(name)
            .copied()
            .ok_or_else(|| VmError::UndefinedVariable(name.to_owned()))?;
        let view = self.manager.read(self.context, object)?;
        decode_value_state(view.state())
    }
}

#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    /// Reconnects a persisted Process to hardware discovered during this boot.
    ///
    /// # Errors
    ///
    /// Returns an error if no provider was discovered or the Link commit fails.
    pub fn reconnect_hardware(&self, process: ObjectId) -> Result<(), VmError> {
        let console = self
            .console_provider
            .ok_or(VmError::MissingProvider("console"))?;
        let view = self.manager.read(self.context, process)?;
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, view.header().version)
            .set_link(process, "console", console);
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
        if state.status != ProcessStatus::Suspended || !view.links().contains_key("$effect") {
            return Ok(false);
        }
        state.status = ProcessStatus::Running;
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
        Ok(state.wake_at_unix_ms.map(|deadline| {
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
        if state.status != ProcessStatus::Suspended
            || state
                .wake_at_unix_ms
                .is_none_or(|deadline| deadline > unix_time_millis())
        {
            return Ok(false);
        }
        state.status = ProcessStatus::Running;
        state.wake_at_unix_ms = None;
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
    /// failures. Reaching the step limit returns a report with `Running` status,
    /// so the Process can be resumed later.
    pub fn run(&self, process: ObjectId, step_limit: u64) -> Result<RunReport, VmError> {
        let mut output = Vec::new();
        let mut steps = 0;
        loop {
            let state = self.process_state(process)?;
            if state.status == ProcessStatus::Suspended
                && state
                    .wake_at_unix_ms
                    .is_some_and(|deadline| deadline <= unix_time_millis())
            {
                self.wake_due_timer(process)?;
                continue;
            }
            if state.status != ProcessStatus::Running {
                return Ok(RunReport {
                    process,
                    steps,
                    status: state.status,
                    output,
                });
            }
            if steps >= step_limit {
                return Ok(RunReport {
                    process,
                    steps,
                    status: state.status,
                    output,
                });
            }
            let executor = self.for_subject(state.subject);
            let (line, executed) = executor.step(process, state, step_limit - steps)?;
            if let Some(line) = line {
                output.push(line);
            }
            steps += executed;
        }
    }

    pub(super) fn for_subject(&self, subject: SubjectId) -> Self {
        Self {
            manager: Arc::clone(&self.manager),
            context: AccessContext::new(subject),
            console_provider: self.console_provider,
            console_driver: self.console_driver.clone(),
            kernel_services: self.kernel_services.clone(),
            providers: Arc::clone(&self.providers),
            program_cache: Arc::clone(&self.program_cache),
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
        if let Some((cached_version, program)) = cache.get(&object) {
            if *cached_version == version {
                return Ok(Arc::clone(program));
            }
        }
        let program = Arc::new(Program::decode(view.state())?);
        cache.insert(object, (version, Arc::clone(&program)));
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

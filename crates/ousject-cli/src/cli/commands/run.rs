use super::super::{
    AccessContext, CooperativeScheduler, ObjectQuery, Path, Program, SYSTEM_SUBJECT,
    compile_source_file, discover_host_hardware, error_text, grant_console_access, host_block_path,
    open_manager, option_subject, parse_object, parse_options, print_report, run_hosted_process,
};

pub(crate) fn command_run(arguments: &[String], tf_input: bool) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 1 {
        let input = if tf_input { "program.tf" } else { "source.px" };
        return Err(format!(
            "usage: ousject {} <{input}> [options]",
            if tf_input { "run-tf" } else { "run" }
        ));
    }
    let program = if tf_input {
        Program::decode(&std::fs::read(&positional[0]).map_err(error_text)?).map_err(error_text)?
    } else {
        compile_source_file(Path::new(&positional[0]))?
    };
    let block_path = host_block_path(&options);
    let manager = open_manager(options.state.as_deref())?;
    let subject = option_subject(&manager, &options)?;
    let vm = discover_host_hardware(manager, block_path)?;
    grant_console_access(vm.manager(), subject)?;
    let process = vm
        .create_process_as(&program, subject)
        .map_err(error_text)?;
    let report = run_hosted_process(&vm, process, options.steps)?;
    print_report(&report);
    eprintln!(
        "process={process} status={:?} steps={}",
        report.status, report.steps
    );
    Ok(())
}

pub(crate) fn command_resume(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    if positional.len() != 1 {
        return Err("usage: ousject resume <process-id> [options]".to_owned());
    }
    let process = parse_object(&positional[0])?;
    let block_path = host_block_path(&options);
    let manager = open_manager(options.state.as_deref())?;
    let vm = discover_host_hardware(manager, block_path)?;
    if options.session.is_some() {
        let subject = option_subject(vm.manager(), &options)?;
        if vm.process_state(process).map_err(error_text)?.subject != subject {
            return Err("the Session does not own this Process".to_owned());
        }
    }
    grant_console_access(
        vm.manager(),
        vm.process_state(process).map_err(error_text)?.subject,
    )?;
    vm.reconnect_hardware(process).map_err(error_text)?;
    let report = run_hosted_process(&vm, process, options.steps)?;
    print_report(&report);
    eprintln!(
        "process={process} status={:?} steps={}",
        report.status, report.steps
    );
    Ok(())
}

pub(crate) fn command_schedule(arguments: &[String]) -> Result<(), String> {
    let (positional, options) = parse_options(arguments)?;
    let block_path = host_block_path(&options);
    let manager = open_manager(options.state.as_deref())?;
    let subject = option_subject(&manager, &options)?;
    let vm = discover_host_hardware(manager, block_path)?;
    grant_console_access(vm.manager(), subject)?;
    let processes = if positional.is_empty() {
        vm.manager()
            .query(
                AccessContext::new(subject),
                &ObjectQuery::new().with_type(oms_types::CORE_PROCESS_TYPE),
            )
            .map_err(error_text)?
            .into_iter()
            .map(|header| header.id)
            .collect::<Vec<_>>()
    } else {
        positional
            .iter()
            .map(|value| parse_object(value))
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut scheduler = CooperativeScheduler::new(&vm);
    for process in processes {
        let state = vm.process_state(process).map_err(error_text)?;
        if subject != SYSTEM_SUBJECT && state.subject != subject {
            return Err(format!("Session does not own Process {process}"));
        }
        vm.reconnect_hardware(process).map_err(error_text)?;
        scheduler.enqueue(process);
    }
    let report = scheduler.run(options.steps).map_err(error_text)?;
    println!("steps={}", report.total_steps);
    for process in report.processes {
        println!(
            "process={} status={:?} steps={}",
            process.process, process.status, process.steps
        );
    }
    Ok(())
}

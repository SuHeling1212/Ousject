mod compile;
mod object;
mod run;
mod system;

pub(super) use compile::command_compile;
pub(super) use object::{
    command_check, command_inspect, command_list, command_object_bind, command_object_create,
    command_object_policy, command_object_query, command_object_resolve, command_object_unbind,
    command_object_value, command_tf_dump, command_type_register, command_types,
};
pub(super) use run::{command_resume, command_run, command_schedule};
pub(super) use system::{command_boot, command_system_install};

pub(crate) fn run_cli() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "help".to_owned());
    let rest = arguments.collect::<Vec<_>>();
    match command.as_str() {
        "compile" => command_compile(&rest),
        "system-install" => command_system_install(&rest),
        "boot" => command_boot(&rest),
        "run" => command_run(&rest, false),
        "run-tf" => command_run(&rest, true),
        "resume" => command_resume(&rest),
        "schedule" => command_schedule(&rest),
        "inspect" => command_inspect(&rest),
        "list" => command_list(&rest),
        "check" => command_check(&rest),
        "types" => command_types(&rest),
        "type-register" => command_type_register(&rest),
        "object-create" => command_object_create(&rest),
        "object-value" => command_object_value(&rest),
        "object-query" => command_object_query(&rest),
        "object-bind" => command_object_bind(&rest),
        "object-unbind" => command_object_unbind(&rest),
        "object-resolve" => command_object_resolve(&rest),
        "object-grant" => command_object_policy(&rest, true),
        "object-revoke" => command_object_policy(&rest, false),
        "tf-dump" => command_tf_dump(&rest),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => Err(format!("unknown command '{command}'; use 'ousject help'")),
    }
}

fn print_help() {
    println!(
        "Ousject system MVP\n\n\
         Commands:\n\
           ousject system-install <system-directory> --local [--state <path>]\n\
           ousject boot [--state <path>]\n\
           ousject compile <source.px> <output.tf>\n\
           ousject run <source.px> [--state <path>] [--steps <count>]\n\
           ousject run-tf <program.tf> [--state <path>] [--steps <count>]\n\
           ousject resume <process-id> [--state <path>] [--steps <count>]\n\
           ousject schedule [process-id ...] [--state <path>] [--steps <count>]\n\
           ousject inspect <object-id> [--state <path>]\n\
           ousject list [--state <path>]\n\
           ousject check [--state <path>]\n\
           ousject types [--state <path>]\n\
           ousject type-register <name> <schema> <public|provider-only> [capability,...] [--state <path>]\n\
           ousject object-create <type-name> <initial-value> [--state <path>]\n\
           ousject object-value <object-id> [--state <path>]\n\
           ousject object-query <type-name> [--capability <name>] [--state <path>]\n\
           ousject object-bind <namespace-id> <name> <target-id> [--state <path>]\n\
           ousject object-unbind <namespace-id> <name> [--state <path>]\n\
           ousject object-resolve <root-id> <path> [--state <path>]\n\
           ousject object-grant <object-id> <subject-id> <capability> [--state <path>]\n\
           ousject object-revoke <object-id> <subject-id> <capability> [--state <path>]\n\
           ousject tf-dump <program.tf>\n\n\
         Use --memory to run without persistent state. Normal commands require --session <token>;\n\
         --local is an explicit development/recovery authority and is never implied."
    );
}

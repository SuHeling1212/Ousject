#![allow(clippy::wildcard_imports)]

use super::*;
use tf_format::Token;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end Package lifecycle test checks faulted transaction boundaries"
)]
fn package_build_import_install_upgrade_rollback_and_restore_are_atomic() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_terminal(manager);
    let source = r#"
modules = object.find("modules")
packages = object.find("packages")
module_v1 = modules.install("greetings_v1", "1.0.0", "func greeting() { return \"hello\" }", [])
module_v2 = modules.install("greetings_v2", "2.0.0", "func greeting() { return \"bonjour\" }", [])
dependency_module = modules.install("shared_helper", "1.0.0", "func helper() { return 7 }", [])
dependency_package = packages.build({namespace: "sample", name: "shared", version: "1.0.0", modules: {shared_helper: dependency_module}})
package_v1 = packages.build({namespace: "sample", name: "greetings", version: "1.0.0", modules: {greetings_v1: module_v1}, dependencies: [dependency_package], exports: {greeting: {module: "greetings_v1", function: "greeting", arguments: 0}}})
package_v2 = packages.build({namespace: "sample", name: "greetings", version: "2.0.0", modules: {greetings_v2: module_v2}, dependencies: [dependency_package], exports: {greeting: {module: "greetings_v2", function: "greeting", arguments: 0}}})
package_bytes = packages.export(package_v1)
imported_package = packages.import(package_bytes)
installation_id = packages.install(package_v1)
dependency_installation = packages.require("sample/shared/1.0.0")
installation = object.find(installation_id)
installation_valid = installation.verify()
export_process_id = installation.greeting()
export_process = object.find(export_process_id)
export_status = export_process.wait()
export_result = export_process.result
data_id = installation.data()
data = object.find(data_id)
data.replace({theme: "kept"})
upgraded_id = installation.upgrade(package_v2)
upgraded = object.find(upgraded_id)
rolled_back_id = upgraded.rollback(installation_id)
rolled_back = object.find(rolled_back_id)
rolled_back.uninstall()
restored_id = packages.restore(installation_id)
restored = object.find(restored_id)
restored_data_id = restored.data()
restored_data = object.find(restored_data_id)
restored_theme = restored_data.theme
active_id = packages.require("sample/greetings")
"#;
    let program = compile(source).unwrap();
    let upgrade_position = program
        .tokens
        .iter()
        .position(|token| matches!(token, Token::ObjectCall { method, .. } if method == "upgrade"))
        .unwrap();
    let rollback_position = program
        .tokens
        .iter()
        .position(|token| matches!(token, Token::ObjectCall { method, .. } if method == "rollback"))
        .unwrap();
    assert!(upgrade_position < rollback_position);

    let process = vm.create_process(&program).unwrap();
    assert_eq!(
        vm.run(process, u64::try_from(upgrade_position).unwrap())
            .unwrap()
            .status,
        ProcessStatus::Ready
    );
    let before_upgrade = vm.process_state(process).unwrap();
    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(
        vm.process_state(process).unwrap().token_position,
        before_upgrade.token_position
    );
    assert_eq!(
        vm.manager()
            .query(
                AccessContext::new(SYSTEM_SUBJECT),
                &ObjectQuery::new().with_type(oms_types::CORE_PACKAGE_INSTALLATION_TYPE),
            )
            .unwrap()
            .len(),
        2
    );

    let to_rollback = rollback_position - upgrade_position;
    assert_eq!(
        vm.run(process, u64::try_from(to_rollback).unwrap())
            .unwrap()
            .status,
        ProcessStatus::Ready
    );
    let before_rollback = vm.process_state(process).unwrap();
    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(
        vm.process_state(process).unwrap().token_position,
        before_rollback.token_position
    );

    let final_run = vm.run(process, 10_000);
    if let Err(error) = &final_run {
        let state = vm.process_state(process).unwrap();
        let position = usize::try_from(state.token_position).unwrap();
        panic!(
            "final Package operation failed at Token {position} ({:?}): {error}",
            program.tokens.get(position)
        );
    }
    assert_eq!(final_run.unwrap().status, ProcessStatus::Halted);
    assert_eq!(
        vm.variable(process, "imported_package"),
        vm.variable(process, "package_v1")
    );
    assert_eq!(
        vm.variable(process, "installation_valid"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        vm.variable(process, "export_status"),
        Ok(Value::Text("halted".to_owned()))
    );
    assert_eq!(
        vm.variable(process, "export_result"),
        Ok(Value::Text("hello".to_owned()))
    );
    assert_eq!(
        vm.variable(process, "restored_theme"),
        Ok(Value::Text("kept".to_owned()))
    );
    assert_eq!(
        vm.variable(process, "active_id"),
        vm.variable(process, "restored_id")
    );
    let Value::Text(package) = vm.variable(process, "package_v1").unwrap() else {
        panic!("expected Package ID");
    };
    let Value::Text(dependency_package) = vm.variable(process, "dependency_package").unwrap()
    else {
        panic!("expected dependency Package ID");
    };
    let system = AccessContext::new(SYSTEM_SUBJECT);
    let Value::Record(package_state) = vm
        .manager()
        .value(system, package.parse().unwrap())
        .unwrap()
    else {
        panic!("expected Package state");
    };
    let Value::Record(manifest) = &package_state["manifest"] else {
        panic!("expected Package Manifest");
    };
    let Value::Array(dependencies) = &manifest["dependencies"] else {
        panic!("expected Package dependency locks");
    };
    assert_eq!(dependencies.len(), 1);
    let Value::Record(dependency_lock) = &dependencies[0] else {
        panic!("expected dependency lock");
    };
    let Value::Record(dependency_state) = vm
        .manager()
        .value(system, dependency_package.parse().unwrap())
        .unwrap()
    else {
        panic!("expected dependency Package state");
    };
    assert_eq!(
        dependency_lock["coordinate"],
        Value::Text("sample/shared/1.0.0".to_owned())
    );
    assert_eq!(dependency_lock["sha256"], dependency_state["sha256"]);
    vm.manager().health_check().unwrap();
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "single Application test checks Package and Program identity across upgrade"
)]
fn package_application_remains_bound_to_its_program_and_sha_after_upgrade() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_terminal(manager);
    let source = r#"
packages = object.find("packages")
compiler = object.find("compiler")
modules = object.find("modules")
runner_module = modules.install("runner_lib", "1.0.0", "func helper() { return 1 }", [])
program_v1 = compiler.compile("func main() { return 41 }")
program_v2 = compiler.compile("func main() { return 42 }")
package_v1 = packages.build({namespace: "sample", name: "runner", version: "1.0.0", modules: {runner_lib: runner_module}, entry: program_v1})
package_v2 = packages.build({namespace: "sample", name: "runner", version: "2.0.0", modules: {runner_lib: runner_module}, entry: program_v2})
installation_v1 = packages.install(package_v1)
installation = object.find(installation_v1)
data_id = installation.data()
data = object.find(data_id)
data.replace({theme: "kept"})
application_process = installation.run()
upgraded_installation = installation.upgrade(package_v2)
default_installation = packages.require("sample/runner")
"#;
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 10_000);
    if let Err(error) = &report {
        let state = vm.process_state(process).unwrap();
        let position = usize::try_from(state.token_position).unwrap();
        panic!(
            "Application Package operation failed at Token {position} ({:?}): {error}",
            compile(source).unwrap().tokens.get(position)
        );
    }
    assert_eq!(report.unwrap().status, ProcessStatus::Halted);

    let Value::Text(installation) = vm.variable(process, "installation_v1").unwrap() else {
        panic!("expected original Installation ID");
    };
    let installation: ObjectId = installation.parse().unwrap();
    let Value::Text(package) = vm.variable(process, "package_v1").unwrap() else {
        panic!("expected original Package ID");
    };
    let package: ObjectId = package.parse().unwrap();
    let Value::Text(application_process) = vm.variable(process, "application_process").unwrap()
    else {
        panic!("expected Application Process ID");
    };
    let application_process: ObjectId = application_process.parse().unwrap();
    let Value::Text(program_v1) = vm.variable(process, "program_v1").unwrap() else {
        panic!("expected original Program ID");
    };
    let program_v1: ObjectId = program_v1.parse().unwrap();

    assert_eq!(
        vm.variable(process, "default_installation"),
        vm.variable(process, "upgraded_installation")
    );
    assert_eq!(
        vm.run(application_process, 10_000).unwrap().status,
        ProcessStatus::Halted
    );
    let process_state = vm.process_state(application_process).unwrap();
    assert_ne!(process_state.subject, SYSTEM_SUBJECT);
    assert_eq!(process_state.result, Some(Value::Integer(41)));

    let system = AccessContext::new(SYSTEM_SUBJECT);
    let instance = vm
        .manager()
        .query(
            system,
            &ObjectQuery::new()
                .with_type(oms_types::CORE_PACKAGE_INSTANCE_TYPE)
                .with_parent(installation),
        )
        .unwrap()
        .into_iter()
        .next()
        .expect("Application Instance should remain attached to v1");
    let Value::Record(instance_state) =
        Value::decode(vm.manager().read(system, instance.id).unwrap().state()).unwrap()
    else {
        panic!("expected Package Instance state");
    };
    let Value::Record(package_state) = vm.manager().value(system, package).unwrap() else {
        panic!("expected original Package state");
    };
    let Value::Text(package_sha) = &package_state["sha256"] else {
        panic!("expected original Package SHA-256");
    };
    assert_eq!(instance_state["package"], Value::Text(package.to_string()));
    assert_eq!(instance_state["sha256"], Value::Text(package_sha.clone()));

    let Value::Text(instance_program) = &instance_state["program"] else {
        panic!("expected bound Program ID");
    };
    let instance_program: ObjectId = instance_program.parse().unwrap();
    assert_eq!(process_state.program, instance_program);
    let Value::Record(manifest) = &package_state["manifest"] else {
        panic!("expected Package Manifest");
    };
    let Value::Record(entry) = &manifest["entry"] else {
        panic!("expected Application entry");
    };
    let Value::Bytes(program_bytes) = &entry["program"] else {
        panic!("expected encoded Program");
    };
    assert_eq!(
        vm.manager().read(system, instance_program).unwrap().state(),
        program_bytes
    );
    assert_ne!(process_state.program, program_v1);
}

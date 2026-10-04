use oms_runtime::InMemoryObjectManager;
use ousject_vm::{ProcessStatus, VirtualMachine};
use praxis_compiler::compile;
use std::sync::Arc;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let iterations = std::env::args()
        .nth(1)
        .map_or(Ok(10_000_i64), |value| value.parse())?;
    if iterations <= 0 {
        return Err("iterations must be positive".into());
    }
    let source = format!("count = 0\nwhile count < {iterations} {{ count++ }}");
    let program = compile(&source)?;
    let manager = Arc::new(InMemoryObjectManager::new(4)?);
    let vm = VirtualMachine::new(manager);
    let process = vm.create_process(&program)?;
    let started = Instant::now();
    let report = vm.run(process, u64::MAX)?;
    let elapsed = started.elapsed();
    if report.status != ProcessStatus::Halted {
        return Err("benchmark Process did not halt".into());
    }
    println!("iterations={iterations}");
    println!("tokens={}", report.steps);
    println!("elapsed_ms={}", elapsed.as_millis());
    println!(
        "tokens_per_second={:.2}",
        f64::from(u32::try_from(report.steps)?) / elapsed.as_secs_f64()
    );
    Ok(())
}

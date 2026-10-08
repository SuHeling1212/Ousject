use super::super::{Path, compile_source_file, error_text};

pub(crate) fn command_compile(arguments: &[String]) -> Result<(), String> {
    if arguments.len() != 2 {
        return Err("usage: ousject compile <source.px> <output.tf>".to_owned());
    }
    let program = compile_source_file(Path::new(&arguments[0]))?;
    let bytes = program.encode().map_err(error_text)?;
    std::fs::write(&arguments[1], bytes).map_err(error_text)?;
    println!(
        "compiled {} tokens to {}",
        program.tokens.len(),
        arguments[1]
    );
    Ok(())
}

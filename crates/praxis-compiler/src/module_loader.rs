use crate::CompileError;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::parser::TopLevelMode;
use std::collections::BTreeSet;
use tf_format::Program;

/// Compiles Praxis source into a validated TF program.
///
/// The MVP subset supports variables, integer/string/bool/null literals,
/// arithmetic, short-circuit Boolean operators, comparisons, `if`/`else if`,
/// `while`, loop control, postfix increment/decrement and Object capability calls.
///
/// # Errors
///
/// Returns a positioned error for invalid source or a program too large for TF.
pub fn compile(source: &str) -> Result<Program, CompileError> {
    if source.lines().any(|line| parse_directive(line).is_some()) {
        return Err(CompileError {
            position: 0,
            message: "import/include requires compile_with_loader or the ousject CLI".to_owned(),
        });
    }
    compile_expanded(source, false)
}

/// Compiles a complete executable Praxis program. The root source must contain
/// exactly one parameterless `main()` and may contain only declarations at top level.
///
/// # Errors
///
/// Returns a positioned error when the source violates the executable program
/// structure or contains invalid Praxis syntax.
pub fn compile_program(source: &str) -> Result<Program, CompileError> {
    if source.lines().any(|line| parse_directive(line).is_some()) {
        return Err(CompileError {
            position: 0,
            message: "import/include requires compile_program_with_loader or the ousject CLI"
                .to_owned(),
        });
    }
    validate_source(source, TopLevelMode::Program)?;
    compile_expanded_program(source)
}

/// Compiles one interactive submission and echoes bare expression results.
///
/// # Errors
///
/// Returns the same positioned errors as [`compile`].
pub fn compile_interactive(source: &str) -> Result<Program, CompileError> {
    if source.lines().any(|line| parse_directive(line).is_some()) {
        return Err(CompileError {
            position: 0,
            message: "import/include requires compile_with_loader or the ousject CLI".to_owned(),
        });
    }
    compile_expanded(source, true)
}

/// Compiles an interactive submission, expanding its Object-backed modules
/// before adding expression-result output.
///
/// # Errors
///
/// Returns a positioned compiler error, loader error or module-cycle error.
pub fn compile_interactive_with_loader(
    source: &str,
    mut loader: impl FnMut(&str) -> Result<String, String>,
) -> Result<Program, CompileError> {
    compile_interactive_with_contextual_loader(source, |name, _importer| {
        loader(name).map(|source| (source, name.to_owned()))
    })
}

/// Compiles an interactive submission while passing the importing module's
/// identity to the loader. The returned identity becomes the importer for
/// nested imports, allowing callers to enforce exact dependency locks.
///
/// # Errors
///
/// Returns a positioned compiler error, loader error or module-cycle error.
pub fn compile_interactive_with_contextual_loader(
    source: &str,
    loader: impl FnMut(&str, Option<&str>) -> Result<(String, String), String>,
) -> Result<Program, CompileError> {
    let expanded = expand_interactive_with_contextual_loader(source, loader)?;
    compile_interactive_expanded(&expanded)
}

/// Expands imports for an interactive submission without compiling its tokens.
/// Callers can validate a compilation cache entry against resolved dependencies
/// before compiling the expanded source.
///
/// # Errors
///
/// Returns a positioned error, loader error or module-cycle error.
pub fn expand_interactive_with_contextual_loader(
    source: &str,
    mut loader: impl FnMut(&str, Option<&str>) -> Result<(String, String), String>,
) -> Result<String, CompileError> {
    let mut imported = BTreeSet::new();
    let mut stack = Vec::new();
    expand_source(source, None, &mut loader, &mut imported, &mut stack)
}

/// Compiles an already expanded interactive source fragment.
///
/// # Errors
///
/// Returns a positioned syntax or TF validation error.
pub fn compile_interactive_expanded(source: &str) -> Result<Program, CompileError> {
    compile_expanded(source, true)
}

/// Compiles Praxis after recursively expanding top-level `import` and `include` directives.
/// `import` loads a module once, while `include` expands it at every occurrence.
///
/// # Errors
///
/// Returns a positioned compiler error or a loader/cycle error at position zero.
pub fn compile_with_loader(
    source: &str,
    mut loader: impl FnMut(&str) -> Result<String, String>,
) -> Result<Program, CompileError> {
    compile_with_contextual_loader(source, |name, _importer| {
        loader(name).map(|source| (source, name.to_owned()))
    })
}

/// Compiles a complete executable Praxis program after recursively expanding
/// imported source modules.
///
/// # Errors
///
/// Returns a positioned compiler error, loader error or module-cycle error.
pub fn compile_program_with_loader(
    source: &str,
    mut loader: impl FnMut(&str) -> Result<String, String>,
) -> Result<Program, CompileError> {
    compile_program_with_contextual_loader(source, |name, _importer| {
        loader(name).map(|source| (source, name.to_owned()))
    })
}

/// Context-aware form of [`compile_program_with_loader`].
///
/// # Errors
///
/// Returns a positioned compiler error, loader error or module-cycle error.
pub fn compile_program_with_contextual_loader(
    source: &str,
    mut loader: impl FnMut(&str, Option<&str>) -> Result<(String, String), String>,
) -> Result<Program, CompileError> {
    validate_source(&without_top_level_directives(source), TopLevelMode::Program)?;
    let mut imported = BTreeSet::new();
    let mut stack = Vec::new();
    let expanded = expand_source(source, None, &mut loader, &mut imported, &mut stack)?;
    compile_expanded_program(&expanded)
}

/// Compiles Praxis source while passing the importing module identity to the
/// loader. The returned identity becomes the importer for nested imports.
///
/// # Errors
///
/// Returns a positioned compiler error, loader error or module-cycle error.
pub fn compile_with_contextual_loader(
    source: &str,
    mut loader: impl FnMut(&str, Option<&str>) -> Result<(String, String), String>,
) -> Result<Program, CompileError> {
    let mut imported = BTreeSet::new();
    let mut stack = Vec::new();
    let expanded = expand_source(source, None, &mut loader, &mut imported, &mut stack)?;
    compile_expanded(&expanded, false)
}

fn compile_expanded(source: &str, interactive: bool) -> Result<Program, CompileError> {
    let lexemes = Lexer::new(source).scan()?;
    Parser::new(lexemes, interactive).compile()
}

fn compile_expanded_program(source: &str) -> Result<Program, CompileError> {
    let mut program = compile_expanded(source, false)?;
    if !matches!(program.tokens.pop(), Some(tf_format::Token::Halt)) {
        return Err(CompileError {
            position: 0,
            message: "internal executable program terminator is missing".to_owned(),
        });
    }
    program.tokens.push(tf_format::Token::CallFunction {
        name: "main".to_owned(),
        arguments: 0,
    });
    program.tokens.push(tf_format::Token::Pop);
    program.tokens.push(tf_format::Token::Halt);
    program.validate().map_err(|error| CompileError {
        position: 0,
        message: error.to_string(),
    })?;
    Ok(program)
}

fn validate_source(source: &str, mode: TopLevelMode) -> Result<(), CompileError> {
    let lexemes = Lexer::new(source).scan()?;
    Parser::validating(lexemes, mode).compile().map(|_| ())
}

fn without_top_level_directives(source: &str) -> String {
    let mut output = String::new();
    let mut brace_depth = 0_i64;
    for line in source.lines() {
        if brace_depth == 0 && parse_directive(line).is_some() {
            output.push('\n');
        } else {
            output.push_str(line);
            output.push('\n');
        }
        brace_depth += brace_delta(line);
        if brace_depth < 0 {
            brace_depth = 0;
        }
    }
    output
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectiveKind {
    Import,
    Include,
}

fn parse_directive(line: &str) -> Option<(DirectiveKind, String)> {
    let trimmed = line.trim();
    let (kind, rest) = if let Some(rest) = trimmed.strip_prefix("import") {
        (DirectiveKind::Import, rest)
    } else {
        (DirectiveKind::Include, trimmed.strip_prefix("include")?)
    };
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let quoted = rest.strip_prefix('"')?;
    let end = quoted.find('"')?;
    let path = &quoted[..end];
    let trailing = quoted[end + 1..].trim();
    if path.is_empty()
        || !(trailing.is_empty()
            || trailing == ";"
            || trailing.starts_with("//")
            || trailing
                .strip_prefix(';')
                .is_some_and(|value| value.trim_start().starts_with("//")))
    {
        return None;
    }
    Some((kind, path.to_owned()))
}

fn expand_source(
    source: &str,
    parent_module: Option<&str>,
    loader: &mut impl FnMut(&str, Option<&str>) -> Result<(String, String), String>,
    imported: &mut BTreeSet<String>,
    stack: &mut Vec<String>,
) -> Result<String, CompileError> {
    let mut output = String::new();
    let mut brace_depth = 0_i64;
    for line in source.lines() {
        if brace_depth == 0 {
            if let Some((kind, path)) = parse_directive(line) {
                if stack.contains(&path) {
                    return Err(CompileError {
                        position: 0,
                        message: format!("cyclic import/include involving '{path}'"),
                    });
                }
                if kind == DirectiveKind::Import && !imported.insert(path.clone()) {
                    output.push('\n');
                    continue;
                }
                stack.push(path.clone());
                let (module_source, module_identity) =
                    loader(&path, parent_module).map_err(|message| CompileError {
                        position: 0,
                        message: format!("cannot load '{path}': {message}"),
                    })?;
                validate_source(
                    &without_top_level_directives(&module_source),
                    TopLevelMode::Module,
                )?;
                let expanded = expand_source(
                    &module_source,
                    Some(&module_identity),
                    loader,
                    imported,
                    stack,
                )?;
                stack.pop();
                output.push_str(&expanded);
                output.push('\n');
                continue;
            }
        }
        output.push_str(line);
        output.push('\n');
        brace_depth += brace_delta(line);
        if brace_depth < 0 {
            brace_depth = 0;
        }
    }
    Ok(output)
}

fn brace_delta(line: &str) -> i64 {
    let mut delta = 0_i64;
    let mut quoted = false;
    let mut escaped = false;
    let bytes = line.as_bytes();
    let mut position = 0;
    while position < bytes.len() {
        let byte = bytes[position];
        if !quoted && byte == b'/' && bytes.get(position + 1) == Some(&b'/') {
            break;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        } else if byte == b'{' {
            delta += 1;
        } else if byte == b'}' {
            delta -= 1;
        }
        position += 1;
    }
    delta
}

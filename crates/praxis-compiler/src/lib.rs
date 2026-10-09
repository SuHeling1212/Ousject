//! Compiler for the executable Praxis subset used by the system MVP.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::borrow::ToOwned;
use alloc::string::String;

mod error;
mod lexer;
mod module_loader;
mod parser;

pub use error::CompileError;
pub use module_loader::{
    compile, compile_interactive, compile_interactive_expanded,
    compile_interactive_with_contextual_loader, compile_interactive_with_loader, compile_program,
    compile_program_with_contextual_loader, compile_program_with_loader,
    compile_with_contextual_loader, compile_with_loader, expand_interactive_with_contextual_loader,
};

/// Version of the compiler crate used when building compilation cache keys.
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests;

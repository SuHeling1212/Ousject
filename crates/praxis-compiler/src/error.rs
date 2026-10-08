use alloc::string::String;
use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub position: usize,
    pub message: String,
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Praxis error at byte {}: {}",
            self.position, self.message
        )
    }
}

#[cfg(feature = "std")]
impl std::error::Error for CompileError {}

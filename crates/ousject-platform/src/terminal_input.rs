//! Bounded, non-blocking UTF-8 line discipline for Terminal transports.

use alloc::string::String;
use core::str::Utf8Error;

use crate::{PlatformError, TerminalTransport};

const INPUT_POLL_BUDGET: usize = 64;
const BACKSPACE_ECHO: &[u8] = b"\x08 \x08";
const INTERRUPT_ECHO: &[u8] = b"^C\r\n";

/// A completed line or an interactive interrupt received from the transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalInputEvent {
    Line(String),
    Interrupt,
}

/// Failures reported by [`TerminalInputBuffer::poll`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalInputError {
    Transport(PlatformError),
    LineTooLong,
    InvalidUtf8,
}

impl From<PlatformError> for TerminalInputError {
    fn from(error: PlatformError) -> Self {
        Self::Transport(error)
    }
}

impl From<Utf8Error> for TerminalInputError {
    fn from(_: Utf8Error) -> Self {
        Self::InvalidUtf8
    }
}

/// Fixed-capacity line editor that never waits for input to arrive.
///
/// Call `poll` from a scheduler turn. It consumes at most 64 bytes and returns
/// `Ok(None)` when no complete line or interrupt is available. Secret input is
/// buffered identically but is never echoed.
#[derive(Clone, Debug)]
pub struct TerminalInputBuffer<const MAX_BYTES: usize = 1024> {
    bytes: [u8; MAX_BYTES],
    length: usize,
    discard_line: bool,
    skip_lf: bool,
}

impl<const MAX_BYTES: usize> Default for TerminalInputBuffer<MAX_BYTES> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const MAX_BYTES: usize> TerminalInputBuffer<MAX_BYTES> {
    /// Creates an empty line editor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_BYTES],
            length: 0,
            discard_line: false,
            skip_lf: false,
        }
    }

    /// Returns the number of bytes currently buffered for the active line.
    #[must_use]
    pub const fn buffered_bytes(&self) -> usize {
        self.length
    }

    /// Polls the transport for one line or Ctrl+C without blocking.
    ///
    /// Input is bounded by `MAX_BYTES`. Backspace removes one complete UTF-8
    /// code point, CR/LF terminates the line, and one LF after CR is consumed
    /// as the second half of CRLF. `echo` should be false for secret input.
    ///
    /// # Errors
    ///
    /// Returns a transport error, `LineTooLong` after discarding the rest of
    /// an overlong line, or `InvalidUtf8` for a completed malformed line.
    pub fn poll(
        &mut self,
        transport: &mut impl TerminalTransport,
        echo: bool,
    ) -> Result<Option<TerminalInputEvent>, TerminalInputError> {
        for _ in 0..INPUT_POLL_BUDGET {
            let mut byte = [0_u8; 1];
            if transport.input(&mut byte)? == 0 {
                break;
            }
            if let Some(event) = self.accept(byte[0], transport, echo)? {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }

    fn accept(
        &mut self,
        byte: u8,
        transport: &mut impl TerminalTransport,
        echo: bool,
    ) -> Result<Option<TerminalInputEvent>, TerminalInputError> {
        if self.skip_lf {
            self.skip_lf = false;
            if byte == b'\n' {
                return Ok(None);
            }
        }

        if matches!(byte, b'\r' | b'\n') {
            let was_overlong = self.discard_line;
            self.discard_line = false;
            if byte == b'\r' {
                self.skip_lf = true;
            }
            if was_overlong {
                self.length = 0;
                return Err(TerminalInputError::LineTooLong);
            }
            if echo {
                transport.output(b"\r\n")?;
            }
            let length = self.length;
            self.length = 0;
            let line = core::str::from_utf8(&self.bytes[..length])?.into();
            return Ok(Some(TerminalInputEvent::Line(line)));
        }

        if byte == 0x03 {
            self.length = 0;
            self.discard_line = false;
            if echo {
                transport.output(INTERRUPT_ECHO)?;
            }
            return Ok(Some(TerminalInputEvent::Interrupt));
        }

        if self.discard_line {
            return Ok(None);
        }

        if matches!(byte, 0x08 | 0x7f) {
            if self.length > 0 {
                let mut start = self.length - 1;
                while start > 0 && self.bytes[start] & 0xc0 == 0x80 {
                    start -= 1;
                }
                self.length = start;
                if echo {
                    transport.output(BACKSPACE_ECHO)?;
                }
            }
            return Ok(None);
        }

        if byte < 0x20 && byte != b'\t' {
            return Ok(None);
        }
        if self.length == MAX_BYTES {
            self.length = 0;
            self.discard_line = true;
            return Ok(None);
        }
        self.bytes[self.length] = byte;
        self.length += 1;
        if echo {
            transport.output(&[byte])?;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    use super::{TerminalInputBuffer, TerminalInputError, TerminalInputEvent};
    use crate::{PlatformError, TerminalTransport};

    #[derive(Debug, Default)]
    struct FakeTerminal {
        input: VecDeque<u8>,
        output: Vec<u8>,
    }

    impl FakeTerminal {
        fn with_input(input: &[u8]) -> Self {
            Self {
                input: input.iter().copied().collect(),
                output: Vec::new(),
            }
        }
    }

    impl TerminalTransport for FakeTerminal {
        fn input(&mut self, output: &mut [u8]) -> Result<usize, PlatformError> {
            let count = output.len().min(self.input.len());
            for slot in output.iter_mut().take(count) {
                *slot = self.input.pop_front().expect("counted byte");
            }
            Ok(count)
        }

        fn output(&mut self, bytes: &[u8]) -> Result<(), PlatformError> {
            self.output.extend_from_slice(bytes);
            Ok(())
        }
    }

    #[test]
    fn poll_edits_utf8_by_code_point_and_echoes_backspace() {
        let mut terminal = FakeTerminal::with_input("你好吗".as_bytes());
        terminal.input.push_back(0x08);
        terminal.input.extend(b"?\r\n");
        let mut line = TerminalInputBuffer::<32>::new();

        assert_eq!(
            line.poll(&mut terminal, true).expect("poll input"),
            Some(TerminalInputEvent::Line("你好?".into()))
        );
        assert_eq!(
            terminal.output,
            ["你好吗".as_bytes(), b"\x08 \x08", b"?\r\n"].concat()
        );
        assert_eq!(line.poll(&mut terminal, true).expect("consume CRLF"), None);
    }

    #[test]
    fn secret_lines_are_never_echoed() {
        let mut terminal = FakeTerminal::with_input(b"s3cret\r");
        let mut line = TerminalInputBuffer::<32>::new();
        assert_eq!(
            line.poll(&mut terminal, false).expect("poll secret"),
            Some(TerminalInputEvent::Line("s3cret".into()))
        );
        assert_eq!(terminal.output.len(), 0);
    }

    #[test]
    fn input_poll_is_bounded_and_preserves_partial_utf8_lines() {
        let mut terminal = FakeTerminal::with_input(&[b'a'; 90]);
        let mut line = TerminalInputBuffer::<128>::new();
        assert_eq!(line.poll(&mut terminal, false).expect("first poll"), None);
        assert_eq!(line.buffered_bytes(), 64);
        assert_eq!(line.poll(&mut terminal, false).expect("second poll"), None);
        assert_eq!(line.buffered_bytes(), 90);
        terminal.input.push_back(b'\n');
        assert_eq!(
            line.poll(&mut terminal, false).expect("complete line"),
            Some(TerminalInputEvent::Line("a".repeat(90)))
        );
    }

    #[test]
    fn overlong_line_is_discarded_until_newline_then_recovers() {
        let mut terminal = FakeTerminal::with_input(b"abcdef\ngood\n");
        let mut line = TerminalInputBuffer::<4>::new();
        assert_eq!(
            line.poll(&mut terminal, false),
            Err(TerminalInputError::LineTooLong)
        );
        assert_eq!(
            line.poll(&mut terminal, false).expect("read next line"),
            Some(TerminalInputEvent::Line("good".into()))
        );
    }

    #[test]
    fn malformed_utf8_line_is_discarded_and_editor_recovers() {
        let mut terminal = FakeTerminal::with_input(&[0xff, b'\r', b'o', b'k', b'\n']);
        let mut line = TerminalInputBuffer::<8>::new();
        assert_eq!(
            line.poll(&mut terminal, false),
            Err(TerminalInputError::InvalidUtf8)
        );
        assert_eq!(line.buffered_bytes(), 0);
        assert_eq!(
            line.poll(&mut terminal, false)
                .expect("poll after invalid line"),
            Some(TerminalInputEvent::Line("ok".into()))
        );
    }

    #[test]
    fn interrupt_aborts_an_overlong_line() {
        let mut terminal = FakeTerminal::with_input(b"12345\x03ok\n");
        let mut line = TerminalInputBuffer::<4>::new();
        assert_eq!(
            line.poll(&mut terminal, false)
                .expect("poll overflow and interrupt"),
            Some(TerminalInputEvent::Interrupt)
        );
        assert_eq!(
            line.poll(&mut terminal, false).expect("poll fresh line"),
            Some(TerminalInputEvent::Line("ok".into()))
        );
    }

    #[test]
    fn control_c_returns_a_distinct_interrupt_event() {
        let mut terminal = FakeTerminal::with_input(b"unfinished\x03next\n");
        let mut line = TerminalInputBuffer::<32>::new();
        assert_eq!(
            line.poll(&mut terminal, false).expect("read interrupt"),
            Some(TerminalInputEvent::Interrupt)
        );
        assert_eq!(
            line.poll(&mut terminal, false)
                .expect("read following line"),
            Some(TerminalInputEvent::Line("next".into()))
        );
    }
}

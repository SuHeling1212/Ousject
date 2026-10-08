//! In-memory terminal screen model for Terminal Objects.
//!
//! This implements the control sequences currently emitted by Praxis programs
//! (cursor movement, erase, SGR, scrolling, and the common alternate-screen
//! modes). It is deliberately a screen model, not a host display driver.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::str;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct TerminalStyle {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub blink: bool,
    pub underline: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
    pub foreground: Option<TerminalColor>,
    pub background: Option<TerminalColor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCell {
    pub text: String,
    /// Zero marks the continuation column of a wide character.
    pub width: u8,
    pub style: TerminalStyle,
}

/// Complete renderer input for one incremental Terminal update.
#[derive(Debug, Clone)]
pub struct TerminalRenderView<'a> {
    pub columns: usize,
    pub rows: usize,
    pub cells: &'a [Vec<TerminalCell>],
    pub dirty_rows: Vec<usize>,
    pub cursor: (usize, usize),
    pub modes: TerminalModes,
    pub title: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalModes {
    pub cursor_visible: bool,
    pub screen: TerminalScreenModes,
    pub input: TerminalInputModes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalScreenModes {
    pub alternate_screen: bool,
    pub autowrap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalInputModes {
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_tracking: MouseTracking,
    pub sgr_mouse: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlyphPosition {
    alternate: bool,
    row: usize,
    column: usize,
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self {
            text: String::new(),
            width: 1,
            style: TerminalStyle::default(),
        }
    }
}

#[derive(Debug, Clone)]
struct Buffer {
    cells: Vec<Vec<TerminalCell>>,
    row: usize,
    column: usize,
    saved_cursor: (usize, usize),
    scroll_top: usize,
    scroll_bottom: usize,
    wrap_pending: bool,
}

impl Buffer {
    fn new(columns: usize, rows: usize) -> Self {
        Self {
            cells: blank_cells(columns, rows),
            row: 0,
            column: 0,
            saved_cursor: (0, 0),
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            wrap_pending: false,
        }
    }

    fn columns(&self) -> usize {
        self.cells.first().map_or(0, Vec::len)
    }

    fn rows(&self) -> usize {
        self.cells.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserState {
    Ground,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

/// Mutable terminal screen state maintained independently of any physical display.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct TerminalScreen {
    main: Buffer,
    alternate: Option<Buffer>,
    in_alternate: bool,
    parser: ParserState,
    sequence: Vec<u8>,
    utf8_pending: Vec<u8>,
    style: TerminalStyle,
    title: String,
    cursor_visible: bool,
    autowrap: bool,
    bracketed_paste: bool,
    application_cursor: bool,
    mouse_tracking: MouseTracking,
    sgr_mouse: bool,
    alternate_saved_cursor: Option<(usize, usize)>,
    mode_saved_cursor: Option<(usize, usize)>,
    last_printed: Option<GlyphPosition>,
    dirty_rows: BTreeSet<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseTracking {
    #[default]
    Disabled,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

impl TerminalScreen {
    #[must_use]
    pub fn new(columns: usize, rows: usize) -> Self {
        let columns = columns.clamp(1, 512);
        let rows = rows.clamp(1, 256);
        Self {
            main: Buffer::new(columns, rows),
            alternate: None,
            in_alternate: false,
            parser: ParserState::Ground,
            sequence: Vec::new(),
            utf8_pending: Vec::new(),
            style: TerminalStyle::default(),
            title: String::new(),
            cursor_visible: true,
            autowrap: true,
            bracketed_paste: false,
            application_cursor: false,
            mouse_tracking: MouseTracking::Disabled,
            sgr_mouse: false,
            alternate_saved_cursor: None,
            mode_saved_cursor: None,
            last_printed: None,
            dirty_rows: (0..rows).collect(),
        }
    }

    fn active(&self) -> &Buffer {
        if self.in_alternate {
            self.alternate.as_ref().unwrap_or(&self.main)
        } else {
            &self.main
        }
    }

    fn active_mut(&mut self) -> &mut Buffer {
        if self.in_alternate {
            self.alternate.as_mut().unwrap_or(&mut self.main)
        } else {
            &mut self.main
        }
    }

    #[must_use]
    pub fn columns(&self) -> usize {
        self.active().columns()
    }

    #[must_use]
    pub fn rows(&self) -> usize {
        self.active().rows()
    }

    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        (self.active().row, self.active().column)
    }

    #[must_use]
    pub fn is_alternate_screen(&self) -> bool {
        self.in_alternate
    }

    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.application_cursor
    }

    #[must_use]
    pub fn mouse_tracking(&self) -> MouseTracking {
        self.mouse_tracking
    }

    #[must_use]
    pub fn sgr_mouse(&self) -> bool {
        self.sgr_mouse
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn cell(&self, row: usize, column: usize) -> Option<&TerminalCell> {
        self.active().cells.get(row)?.get(column)
    }

    /// Returns the display text for a row, omitting trailing blank cells.
    #[must_use]
    pub fn row_text(&self, row: usize) -> Option<String> {
        let cells = self.active().cells.get(row)?;
        let end = cells
            .iter()
            .rposition(|cell| !cell.text.is_empty())
            .map_or(0, |index| index + 1);
        Some(
            cells[..end]
                .iter()
                .map(|cell| {
                    if cell.width == 0 {
                        ""
                    } else if cell.text.is_empty() {
                        " "
                    } else {
                        cell.text.as_str()
                    }
                })
                .collect(),
        )
    }

    /// Returns rows changed since the previous call.
    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        core::mem::take(&mut self.dirty_rows).into_iter().collect()
    }

    /// Marks every row dirty, for example when this Screen becomes the active
    /// host-rendered Terminal again after a child exits.
    pub fn mark_all_dirty(&mut self) {
        self.dirty_rows = (0..self.rows()).collect();
    }

    /// Borrows styled cells and consumes the accumulated damage set.
    pub fn render_view(&mut self) -> TerminalRenderView<'_> {
        let dirty_rows = self.take_dirty_rows();
        let buffer = self.active();
        TerminalRenderView {
            columns: buffer.columns(),
            rows: buffer.rows(),
            cells: &buffer.cells,
            dirty_rows,
            cursor: (buffer.row, buffer.column),
            modes: TerminalModes {
                cursor_visible: self.cursor_visible,
                screen: TerminalScreenModes {
                    alternate_screen: self.in_alternate,
                    autowrap: self.autowrap,
                },
                input: TerminalInputModes {
                    application_cursor: self.application_cursor,
                    bracketed_paste: self.bracketed_paste,
                    mouse_tracking: self.mouse_tracking,
                    sgr_mouse: self.sgr_mouse,
                },
            },
            title: &self.title,
        }
    }

    /// Resizes both screen buffers and marks every row dirty.
    pub fn resize(&mut self, columns: usize, rows: usize) {
        let columns = columns.clamp(1, 512);
        let rows = rows.clamp(1, 256);
        resize_buffer(&mut self.main, columns, rows);
        if let Some(alternate) = &mut self.alternate {
            resize_buffer(alternate, columns, rows);
        }
        self.last_printed = None;
        self.dirty_rows = (0..rows).collect();
    }

    /// Feeds arbitrary stream chunks. Parser state survives chunk boundaries.
    pub fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.parser {
                ParserState::Ground => self.ground_byte(byte),
                ParserState::Escape => self.escape_byte(byte),
                ParserState::Csi => self.csi_byte(byte),
                ParserState::Osc => self.osc_byte(byte),
                ParserState::OscEscape => self.osc_escape_byte(byte),
            }
        }
    }

    fn ground_byte(&mut self, byte: u8) {
        match byte {
            0x1b => {
                self.flush_incomplete_utf8();
                self.parser = ParserState::Escape;
            }
            b'\r' => {
                self.last_printed = None;
                let buffer = self.active_mut();
                buffer.column = 0;
                buffer.wrap_pending = false;
            }
            b'\n' | 0x0b | 0x0c => {
                self.last_printed = None;
                self.line_feed();
            }
            0x08 => {
                self.last_printed = None;
                let buffer = self.active_mut();
                buffer.column = buffer.column.saturating_sub(1);
                buffer.wrap_pending = false;
            }
            0x09 => {
                self.last_printed = None;
                let buffer = self.active_mut();
                let stop = (buffer.column / 8 + 1) * 8;
                buffer.column = stop.min(buffer.columns().saturating_sub(1));
                buffer.wrap_pending = false;
            }
            0x00..=0x1f | 0x7f => {}
            _ => self.utf8_byte(byte),
        }
    }

    fn utf8_byte(&mut self, byte: u8) {
        self.utf8_pending.push(byte);
        loop {
            match str::from_utf8(&self.utf8_pending) {
                Ok(text) => {
                    let chars = text.chars().collect::<Vec<_>>();
                    self.utf8_pending.clear();
                    for character in chars {
                        self.put_character(character);
                    }
                    return;
                }
                Err(error) if error.error_len().is_none() => return,
                Err(error) => {
                    let valid = error.valid_up_to();
                    let bad_len = error.error_len().unwrap_or(1);
                    if valid > 0 {
                        let text =
                            alloc::string::String::from_utf8_lossy(&self.utf8_pending[..valid])
                                .into_owned();
                        for character in text.chars() {
                            self.put_character(character);
                        }
                    }
                    self.utf8_pending.drain(..valid.saturating_add(bad_len));
                    self.put_character('\u{fffd}');
                    if self.utf8_pending.is_empty() {
                        return;
                    }
                }
            }
        }
    }

    fn flush_incomplete_utf8(&mut self) {
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.clear();
            self.put_character('\u{fffd}');
        }
    }

    fn escape_byte(&mut self, byte: u8) {
        self.parser = ParserState::Ground;
        match byte {
            b'[' => {
                self.sequence.clear();
                self.parser = ParserState::Csi;
            }
            b']' => {
                self.sequence.clear();
                self.parser = ParserState::Osc;
            }
            b'7' => {
                let buffer = self.active_mut();
                buffer.saved_cursor = (buffer.row, buffer.column);
            }
            b'8' => {
                self.last_printed = None;
                let buffer = self.active_mut();
                (buffer.row, buffer.column) = buffer.saved_cursor;
                clamp_cursor(buffer);
            }
            b'D' => {
                self.last_printed = None;
                self.line_feed();
            }
            b'E' => {
                self.last_printed = None;
                self.active_mut().column = 0;
                self.line_feed();
            }
            b'M' => {
                self.last_printed = None;
                self.reverse_index();
            }
            b'c' => {
                self.last_printed = None;
                self.reset();
            }
            _ => {}
        }
    }

    fn csi_byte(&mut self, byte: u8) {
        if (0x40..=0x7e).contains(&byte) {
            let sequence = core::mem::take(&mut self.sequence);
            self.parser = ParserState::Ground;
            self.execute_csi(&sequence, byte);
        } else if self.sequence.len() < 128 {
            self.sequence.push(byte);
        } else {
            self.sequence.clear();
            self.parser = ParserState::Ground;
        }
    }

    fn osc_byte(&mut self, byte: u8) {
        match byte {
            0x07 => self.finish_osc(),
            0x1b => self.parser = ParserState::OscEscape,
            _ if self.sequence.len() < 4096 => self.sequence.push(byte),
            _ => {
                self.sequence.clear();
                self.parser = ParserState::Ground;
            }
        }
    }

    fn osc_escape_byte(&mut self, byte: u8) {
        if byte == b'\\' {
            self.finish_osc();
        } else {
            self.sequence.push(0x1b);
            self.osc_byte(byte);
        }
    }

    fn finish_osc(&mut self) {
        self.parser = ParserState::Ground;
        if let Ok(sequence) = str::from_utf8(&self.sequence) {
            if let Some((command, title)) = sequence.split_once(';') {
                if matches!(command, "0" | "1" | "2") {
                    self.title = title.to_owned();
                }
            }
        }
        self.sequence.clear();
    }

    fn execute_csi(&mut self, bytes: &[u8], final_byte: u8) {
        let Ok(sequence) = str::from_utf8(bytes) else {
            return;
        };
        let private = sequence.starts_with('?');
        let params = sequence.trim_start_matches(['?', '>']);
        let values = params
            .split(';')
            .map(|part| part.parse::<usize>().unwrap_or(0))
            .collect::<Vec<_>>();
        let first = values.first().copied().unwrap_or(0);
        if private && matches!(final_byte, b'h' | b'l') {
            let enable = final_byte == b'h';
            for value in values {
                match value {
                    1 => self.application_cursor = enable,
                    25 => self.cursor_visible = enable,
                    7 => self.autowrap = enable,
                    47 | 1047 | 1049 => self.alternate_screen(value, enable),
                    1048 => self.save_or_restore_mode_cursor(enable),
                    1000 => {
                        self.mouse_tracking = if enable {
                            MouseTracking::PressRelease
                        } else {
                            MouseTracking::Disabled
                        };
                    }
                    1002 => {
                        self.mouse_tracking = if enable {
                            MouseTracking::ButtonMotion
                        } else {
                            MouseTracking::Disabled
                        };
                    }
                    1003 => {
                        self.mouse_tracking = if enable {
                            MouseTracking::AnyMotion
                        } else {
                            MouseTracking::Disabled
                        };
                    }
                    1006 => self.sgr_mouse = enable,
                    2004 => self.bracketed_paste = enable,
                    _ => {}
                }
            }
            return;
        }
        if final_byte != b'm' {
            self.last_printed = None;
        }
        match final_byte {
            b'A' => self.move_cursor(0, -movement(first)),
            b'B' | b'e' => self.move_cursor(0, movement(first)),
            b'C' | b'a' => self.move_cursor(movement(first), 0),
            b'D' => self.move_cursor(-movement(first), 0),
            b'E' => {
                self.move_cursor(0, movement(first));
                self.active_mut().column = 0;
            }
            b'F' => {
                self.move_cursor(0, -movement(first));
                self.active_mut().column = 0;
            }
            b'G' | b'`' => self.set_column(positive(first) - 1),
            b'd' => self.set_row(positive(first) - 1),
            b'H' | b'f' => {
                let row = positive(values.first().copied().unwrap_or(0)) - 1;
                let column = positive(values.get(1).copied().unwrap_or(0)) - 1;
                self.set_cursor(row, column);
            }
            b'J' => self.erase_display(first),
            b'K' => self.erase_line(first),
            b'@' => self.insert_chars(positive(first)),
            b'P' => self.delete_chars(positive(first)),
            b'X' => self.erase_chars(positive(first)),
            b'L' => self.insert_lines(positive(first)),
            b'M' => self.delete_lines(positive(first)),
            b'S' => self.scroll_up(positive(first)),
            b'T' => self.scroll_down(positive(first)),
            b'm' => self.sgr(&values),
            b's' => {
                let buffer = self.active_mut();
                buffer.saved_cursor = (buffer.row, buffer.column);
            }
            b'u' => {
                let buffer = self.active_mut();
                (buffer.row, buffer.column) = buffer.saved_cursor;
                clamp_cursor(buffer);
            }
            b'r' => self.set_scroll_region(&values),
            _ => {}
        }
    }

    fn put_character(&mut self, character: char) {
        if self.extend_last_grapheme(character) {
            return;
        }
        let mut text = character.to_string();
        let measured_width = UnicodeWidthStr::width(text.as_str());
        if measured_width == 0 {
            text.insert(0, '\u{25cc}');
        }
        let mut width =
            u8::try_from(UnicodeWidthStr::width(text.as_str()).clamp(1, 2)).unwrap_or(2);
        let should_wrap = {
            let buffer = self.active();
            buffer.wrap_pending && self.autowrap
        };
        if should_wrap {
            self.active_mut().column = 0;
            self.line_feed();
        } else if self.active().wrap_pending {
            self.active_mut().wrap_pending = false;
        }
        let needs_wide_wrap = {
            let buffer = self.active();
            self.autowrap && usize::from(width) == 2 && buffer.column + 1 >= buffer.columns()
        };
        if needs_wide_wrap {
            self.active_mut().column = 0;
            self.line_feed();
        }
        let (row, column, columns) = {
            let buffer = self.active();
            (buffer.row, buffer.column, buffer.columns())
        };
        if width == 2 && column + 1 >= columns {
            width = 1;
        }
        let style = self.style.clone();
        self.clear_glyph_at(row, column);
        if width == 2 && column + 1 < columns {
            self.clear_glyph_at(row, column + 1);
        }
        {
            let buffer = self.active_mut();
            buffer.cells[row][column] = TerminalCell {
                text,
                width,
                style: style.clone(),
            };
            if width == 2 && column + 1 < columns {
                buffer.cells[row][column + 1] = TerminalCell {
                    width: 0,
                    style,
                    ..TerminalCell::default()
                };
            }
            if column + usize::from(width) >= columns {
                buffer.column = columns.saturating_sub(1);
                buffer.wrap_pending = true;
            } else {
                buffer.column += usize::from(width);
            }
        }
        self.last_printed = Some(GlyphPosition {
            alternate: self.in_alternate,
            row,
            column,
        });
        self.mark_row(row);
    }

    fn extend_last_grapheme(&mut self, character: char) -> bool {
        let Some(position) = self.last_printed else {
            return false;
        };
        if position.alternate != self.in_alternate {
            return false;
        }
        let Some(previous) = self
            .active()
            .cells
            .get(position.row)
            .and_then(|row| row.get(position.column))
            .filter(|cell| cell.width > 0 && !cell.text.is_empty())
            .map(|cell| cell.text.clone())
        else {
            return false;
        };
        let mut grapheme = previous;
        grapheme.push(character);
        if grapheme.graphemes(true).count() != 1 {
            return false;
        }
        let width =
            u8::try_from(UnicodeWidthStr::width(grapheme.as_str()).clamp(1, 2)).unwrap_or(2);
        let old_width = self.active().cells[position.row][position.column].width;
        let columns = self.active().columns();
        let style = self.active().cells[position.row][position.column]
            .style
            .clone();
        {
            let buffer = self.active_mut();
            buffer.cells[position.row][position.column].text = grapheme;
            buffer.cells[position.row][position.column].width = width;
            if old_width == 2 && width == 1 && position.column + 1 < columns {
                buffer.cells[position.row][position.column + 1] = TerminalCell {
                    style: style.clone(),
                    ..TerminalCell::default()
                };
            } else if width == 2 && position.column + 1 < columns {
                buffer.cells[position.row][position.column + 1] = TerminalCell {
                    width: 0,
                    style,
                    ..TerminalCell::default()
                };
            }
            let end = position.column + usize::from(width);
            if end >= columns {
                buffer.column = columns - 1;
                buffer.wrap_pending = true;
            } else {
                buffer.column = end;
                buffer.wrap_pending = false;
            }
        }
        self.mark_row(position.row);
        true
    }

    fn clear_glyph_at(&mut self, row: usize, column: usize) {
        let columns = self.active().columns();
        let Some(cell) = self
            .active()
            .cells
            .get(row)
            .and_then(|cells| cells.get(column))
        else {
            return;
        };
        let width = cell.width;
        let start = if width == 0 {
            column.saturating_sub(1)
        } else {
            column
        };
        let clear_end = if width == 0 || width == 2 {
            (start + 2).min(columns)
        } else {
            (start + 1).min(columns)
        };
        let buffer = self.active_mut();
        for index in start..clear_end {
            buffer.cells[row][index] = TerminalCell::default();
        }
    }

    fn move_cursor(&mut self, columns: isize, rows: isize) {
        let buffer = self.active_mut();
        buffer.column = buffer
            .column
            .saturating_add_signed(columns)
            .min(buffer.columns() - 1);
        buffer.row = buffer
            .row
            .saturating_add_signed(rows)
            .min(buffer.rows() - 1);
        buffer.wrap_pending = false;
        self.last_printed = None;
    }

    fn set_column(&mut self, column: usize) {
        let buffer = self.active_mut();
        buffer.column = column.min(buffer.columns() - 1);
        buffer.wrap_pending = false;
        self.last_printed = None;
    }

    fn set_row(&mut self, row: usize) {
        let buffer = self.active_mut();
        buffer.row = row.min(buffer.rows() - 1);
        buffer.wrap_pending = false;
        self.last_printed = None;
    }

    fn set_cursor(&mut self, row: usize, column: usize) {
        let buffer = self.active_mut();
        buffer.row = row.min(buffer.rows() - 1);
        buffer.column = column.min(buffer.columns() - 1);
        buffer.wrap_pending = false;
        self.last_printed = None;
    }

    fn line_feed(&mut self) {
        let scroll = {
            let buffer = self.active_mut();
            buffer.wrap_pending = false;
            if buffer.row == buffer.scroll_bottom {
                Some(buffer.scroll_bottom - buffer.scroll_top + 1)
            } else {
                buffer.row = (buffer.row + 1).min(buffer.rows() - 1);
                let row = buffer.row;
                self.dirty_rows.insert(row);
                None
            }
        };
        if scroll.is_some() {
            self.scroll_up(1);
        }
    }

    fn reverse_index(&mut self) {
        let scroll = {
            let buffer = self.active_mut();
            buffer.wrap_pending = false;
            if buffer.row == buffer.scroll_top {
                Some(buffer.scroll_bottom - buffer.scroll_top + 1)
            } else {
                buffer.row = buffer.row.saturating_sub(1);
                let row = buffer.row;
                self.dirty_rows.insert(row);
                None
            }
        };
        if scroll.is_some() {
            self.scroll_down(1);
        }
    }

    fn insert_lines(&mut self, count: usize) {
        let (row, top, bottom, columns) = {
            let buffer = self.active();
            (
                buffer.row,
                buffer.scroll_top,
                buffer.scroll_bottom,
                buffer.columns(),
            )
        };
        if row < top || row > bottom {
            return;
        }
        let count = count.min(bottom - row + 1);
        let buffer = self.active_mut();
        for _ in 0..count {
            buffer.cells.remove(bottom);
            buffer.cells.insert(row, blank_row(columns));
        }
        self.dirty_rows.extend(row..=bottom);
        self.last_printed = None;
    }

    fn delete_lines(&mut self, count: usize) {
        let (row, top, bottom, columns) = {
            let buffer = self.active();
            (
                buffer.row,
                buffer.scroll_top,
                buffer.scroll_bottom,
                buffer.columns(),
            )
        };
        if row < top || row > bottom {
            return;
        }
        let count = count.min(bottom - row + 1);
        let buffer = self.active_mut();
        for _ in 0..count {
            buffer.cells.remove(row);
            buffer.cells.insert(bottom, blank_row(columns));
        }
        self.dirty_rows.extend(row..=bottom);
        self.last_printed = None;
    }

    fn erase_display(&mut self, mode: usize) {
        let (row, column) = self.cursor();
        let rows = self.rows();
        let columns = self.columns();
        match mode {
            0 => {
                self.erase_range(row, column, columns);
                for index in row + 1..rows {
                    self.clear_row(index);
                }
            }
            1 => {
                for index in 0..row {
                    self.clear_row(index);
                }
                self.erase_range(row, 0, column + 1);
            }
            2 | 3 => {
                for index in 0..rows {
                    self.clear_row(index);
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let (row, column) = self.cursor();
        match mode {
            0 => self.erase_range(row, column, self.columns()),
            1 => self.erase_range(row, 0, column + 1),
            2 => self.clear_row(row),
            _ => {}
        }
    }

    fn erase_chars(&mut self, count: usize) {
        let (row, column) = self.cursor();
        self.erase_range(
            row,
            column,
            column.saturating_add(count).min(self.columns()),
        );
    }

    fn erase_range(&mut self, row: usize, start: usize, end: usize) {
        let style = self.style.clone();
        let buffer = self.active_mut();
        let end = end.min(buffer.columns());
        for cell in &mut buffer.cells[row][start.min(end)..end] {
            *cell = TerminalCell {
                style: style.clone(),
                ..TerminalCell::default()
            };
        }
        normalize_wide_row(&mut buffer.cells[row]);
        self.last_printed = None;
        self.mark_row(row);
    }

    fn clear_row(&mut self, row: usize) {
        let style = self.style.clone();
        let buffer = self.active_mut();
        buffer.cells[row] = (0..buffer.columns())
            .map(|_| TerminalCell {
                style: style.clone(),
                ..TerminalCell::default()
            })
            .collect();
        self.last_printed = None;
        self.mark_row(row);
    }

    fn insert_chars(&mut self, count: usize) {
        let (row, column) = self.cursor();
        let style = self.style.clone();
        let buffer = self.active_mut();
        let columns = buffer.columns();
        for _ in 0..count.min(columns - column) {
            buffer.cells[row].insert(
                column,
                TerminalCell {
                    style: style.clone(),
                    ..TerminalCell::default()
                },
            );
            buffer.cells[row].pop();
        }
        normalize_wide_row(&mut buffer.cells[row]);
        self.last_printed = None;
        self.mark_row(row);
    }

    fn delete_chars(&mut self, count: usize) {
        let (row, column) = self.cursor();
        let style = self.style.clone();
        let buffer = self.active_mut();
        let columns = buffer.columns();
        for _ in 0..count.min(columns - column) {
            buffer.cells[row].remove(column);
            buffer.cells[row].push(TerminalCell {
                style: style.clone(),
                ..TerminalCell::default()
            });
        }
        normalize_wide_row(&mut buffer.cells[row]);
        self.last_printed = None;
        self.mark_row(row);
    }

    fn sgr(&mut self, values: &[usize]) {
        let values = if values.is_empty() { &[0][..] } else { values };
        let mut index = 0;
        while index < values.len() {
            let value = values[index];
            match value {
                0 => self.style = TerminalStyle::default(),
                1 => self.style.bold = true,
                2 => self.style.dim = true,
                3 => self.style.italic = true,
                4 => self.style.underline = true,
                5 | 6 => self.style.blink = true,
                7 => self.style.inverse = true,
                8 => self.style.hidden = true,
                9 => self.style.strikethrough = true,
                22 => {
                    self.style.bold = false;
                    self.style.dim = false;
                }
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                25 => self.style.blink = false,
                27 => self.style.inverse = false,
                28 => self.style.hidden = false,
                29 => self.style.strikethrough = false,
                30..=37 => {
                    self.style.foreground = Some(TerminalColor::Indexed(
                        u8::try_from(value - 30).unwrap_or_default(),
                    ));
                }
                39 => self.style.foreground = None,
                40..=47 => {
                    self.style.background = Some(TerminalColor::Indexed(
                        u8::try_from(value - 40).unwrap_or_default(),
                    ));
                }
                49 => self.style.background = None,
                90..=97 => {
                    self.style.foreground = Some(TerminalColor::Indexed(
                        u8::try_from(value - 90 + 8).unwrap_or_default(),
                    ));
                }
                100..=107 => {
                    self.style.background = Some(TerminalColor::Indexed(
                        u8::try_from(value - 100 + 8).unwrap_or_default(),
                    ));
                }
                38 | 48 if values.get(index + 1) == Some(&5) => {
                    if let Some(color) = values
                        .get(index + 2)
                        .and_then(|value| u8::try_from(*value).ok())
                    {
                        let color = Some(TerminalColor::Indexed(color));
                        if value == 38 {
                            self.style.foreground = color;
                        } else {
                            self.style.background = color;
                        }
                    }
                    index = index.saturating_add(2);
                }
                38 | 48 if values.get(index + 1) == Some(&2) => {
                    if let (Some(red), Some(green), Some(blue)) = (
                        values
                            .get(index + 2)
                            .and_then(|value| u8::try_from(*value).ok()),
                        values
                            .get(index + 3)
                            .and_then(|value| u8::try_from(*value).ok()),
                        values
                            .get(index + 4)
                            .and_then(|value| u8::try_from(*value).ok()),
                    ) {
                        let color = Some(TerminalColor::Rgb(red, green, blue));
                        if value == 38 {
                            self.style.foreground = color;
                        } else {
                            self.style.background = color;
                        }
                    }
                    index = index.saturating_add(4);
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn scroll_up(&mut self, count: usize) {
        let (top, bottom, columns) = {
            let buffer = self.active();
            (buffer.scroll_top, buffer.scroll_bottom, buffer.columns())
        };
        let count = count.min(bottom - top + 1);
        let buffer = self.active_mut();
        for _ in 0..count {
            buffer.cells.remove(top);
            buffer.cells.insert(bottom, blank_row(columns));
        }
        self.dirty_rows.extend(top..=bottom);
        self.last_printed = None;
    }

    fn scroll_down(&mut self, count: usize) {
        let (top, bottom, columns) = {
            let buffer = self.active();
            (buffer.scroll_top, buffer.scroll_bottom, buffer.columns())
        };
        let count = count.min(bottom - top + 1);
        let buffer = self.active_mut();
        for _ in 0..count {
            buffer.cells.remove(bottom);
            buffer.cells.insert(top, blank_row(columns));
        }
        self.dirty_rows.extend(top..=bottom);
        self.last_printed = None;
    }

    fn set_scroll_region(&mut self, values: &[usize]) {
        let rows = self.rows();
        let top = positive(values.first().copied().unwrap_or(0)) - 1;
        let bottom = if values.get(1).copied().unwrap_or(0) == 0 {
            rows - 1
        } else {
            positive(values.get(1).copied().unwrap_or(0)) - 1
        };
        if top < bottom && bottom < rows {
            let buffer = self.active_mut();
            buffer.scroll_top = top;
            buffer.scroll_bottom = bottom;
            buffer.row = top;
            buffer.column = 0;
            buffer.wrap_pending = false;
            self.last_printed = None;
            self.mark_row(top);
        }
    }

    fn save_or_restore_mode_cursor(&mut self, save: bool) {
        if save {
            let buffer = self.active();
            self.mode_saved_cursor = Some((buffer.row, buffer.column));
        } else if let Some((row, column)) = self.mode_saved_cursor.take() {
            let buffer = self.active_mut();
            buffer.row = row.min(buffer.rows() - 1);
            buffer.column = column.min(buffer.columns() - 1);
            buffer.wrap_pending = false;
            let row = buffer.row;
            self.last_printed = None;
            self.mark_row(row);
        }
    }

    fn alternate_screen(&mut self, mode: usize, enable: bool) {
        let columns = self.main.columns();
        let rows = self.main.rows();
        match (mode, enable) {
            (47, true) => {
                self.alternate
                    .get_or_insert_with(|| Buffer::new(columns, rows));
                self.in_alternate = true;
            }
            (47, false) => self.in_alternate = false,
            (1047, true) => {
                self.alternate = Some(Buffer::new(columns, rows));
                self.in_alternate = true;
            }
            (1047, false) => {
                self.in_alternate = false;
                self.alternate = None;
            }
            (1049, true) => {
                if !self.in_alternate {
                    self.alternate_saved_cursor = Some((self.main.row, self.main.column));
                }
                self.alternate = Some(Buffer::new(columns, rows));
                self.in_alternate = true;
            }
            (1049, false) => {
                self.in_alternate = false;
                self.alternate = None;
                if let Some((row, column)) = self.alternate_saved_cursor.take() {
                    self.main.row = row.min(self.main.rows() - 1);
                    self.main.column = column.min(self.main.columns() - 1);
                    self.main.wrap_pending = false;
                }
            }
            _ => {}
        }
        self.last_printed = None;
        self.dirty_rows = (0..rows).collect();
    }

    fn reset(&mut self) {
        self.main = Buffer::new(self.main.columns(), self.main.rows());
        self.alternate = None;
        self.in_alternate = false;
        self.style = TerminalStyle::default();
        self.cursor_visible = true;
        self.autowrap = true;
        self.application_cursor = false;
        self.bracketed_paste = false;
        self.mouse_tracking = MouseTracking::Disabled;
        self.sgr_mouse = false;
        self.alternate_saved_cursor = None;
        self.mode_saved_cursor = None;
        self.last_printed = None;
        self.dirty_rows = (0..self.main.rows()).collect();
    }

    fn mark_row(&mut self, row: usize) {
        self.dirty_rows.insert(row);
    }
}

fn positive(value: usize) -> usize {
    value.max(1)
}

fn movement(value: usize) -> isize {
    isize::try_from(positive(value)).unwrap_or(isize::MAX)
}

fn blank_row(columns: usize) -> Vec<TerminalCell> {
    vec![TerminalCell::default(); columns]
}

fn blank_cells(columns: usize, rows: usize) -> Vec<Vec<TerminalCell>> {
    (0..rows).map(|_| blank_row(columns)).collect()
}

fn normalize_wide_row(cells: &mut [TerminalCell]) {
    for column in 0..cells.len() {
        if cells[column].width == 0 {
            if column == 0 || cells[column - 1].width != 2 {
                cells[column] = TerminalCell::default();
            }
        } else if cells[column].width == 2
            && cells
                .get(column + 1)
                .is_none_or(|continuation| continuation.width != 0)
        {
            cells[column].width = 1;
        }
    }
}

fn resize_buffer(buffer: &mut Buffer, columns: usize, rows: usize) {
    buffer.cells.resize_with(rows, || blank_row(columns));
    for row in &mut buffer.cells {
        row.resize(columns, TerminalCell::default());
    }
    buffer.row = buffer.row.min(rows - 1);
    buffer.column = buffer.column.min(columns - 1);
    buffer.scroll_top = buffer.scroll_top.min(rows - 1);
    buffer.scroll_bottom = buffer.scroll_bottom.min(rows - 1).max(buffer.scroll_top);
    clamp_cursor(buffer);
}

fn clamp_cursor(buffer: &mut Buffer) {
    buffer.row = buffer.row.min(buffer.rows() - 1);
    buffer.column = buffer.column.min(buffer.columns() - 1);
    buffer.wrap_pending = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_unicode_and_tracks_dirty_rows_incrementally() {
        let mut screen = TerminalScreen::new(8, 3);
        screen.take_dirty_rows();
        screen.write("Hi你".as_bytes());
        assert_eq!(screen.row_text(0).as_deref(), Some("Hi你"));
        assert_eq!(screen.cell(0, 2).unwrap().width, 2);
        assert_eq!(screen.cell(0, 3).unwrap().width, 0);
        assert_eq!(screen.take_dirty_rows(), vec![0]);
        assert_eq!(screen.take_dirty_rows(), Vec::<usize>::new());
    }

    #[test]
    fn cursor_motion_does_not_mark_unchanged_rows_dirty() {
        let mut screen = TerminalScreen::new(8, 4);
        screen.take_dirty_rows();
        screen.write(b"\x1b[H\x1b[B\x1b[C\x1b[s\x1b[4;8H\x1b[u\x1b7\x1b[2;2H\x1b8");

        assert_eq!(screen.cursor(), (1, 1));
        assert_eq!(screen.take_dirty_rows(), Vec::<usize>::new());
    }

    #[test]
    fn parser_handles_chunked_cursor_erase_and_sgr_sequences() {
        let mut screen = TerminalScreen::new(8, 3);
        screen.write(b"abc\x1b[2;3H\x1b[31mX");
        assert_eq!(screen.row_text(0).as_deref(), Some("abc"));
        assert_eq!(screen.row_text(1).as_deref(), Some("  X"));
        assert_eq!(
            screen.cell(1, 2).unwrap().style.foreground,
            Some(TerminalColor::Indexed(1))
        );
        screen.write(b"\x1b[2K");
        assert_eq!(screen.row_text(1).as_deref(), Some(""));
    }

    #[test]
    fn truecolor_and_autowrap_sequences_are_applied() {
        let mut screen = TerminalScreen::new(3, 2);
        screen.write(b"\x1b[38;2;9;20;30mabcX");
        assert_eq!(
            screen.cell(0, 2).unwrap().style.foreground,
            Some(TerminalColor::Rgb(9, 20, 30))
        );
        assert_eq!(screen.row_text(1).as_deref(), Some("X"));
        screen.write(b"\x1b[?7labcX");
        assert_eq!(screen.row_text(1).as_deref(), Some("XaX"));
    }

    #[test]
    fn alternate_screen_and_common_terminal_modes_are_tracked() {
        let mut screen = TerminalScreen::new(8, 3);
        screen.write(b"main\x1b[?1049halt\x1b[?25l\x1b[?2004h");
        assert!(screen.is_alternate_screen());
        assert!(!screen.cursor_visible());
        assert!(screen.bracketed_paste());
        assert_eq!(screen.row_text(0).as_deref(), Some("alt"));
        screen.write(b"\x1b[?1049l");
        assert_eq!(screen.row_text(0).as_deref(), Some("main"));
    }

    #[test]
    fn scroll_and_title_sequences_preserve_state() {
        let mut screen = TerminalScreen::new(4, 2);
        screen.write(b"one\r\ntwo\r\nthree\x1b]2;demo\x07");
        assert_eq!(screen.row_text(0).as_deref(), Some("thre"));
        assert_eq!(screen.row_text(1).as_deref(), Some("e"));
        assert_eq!(screen.title(), "demo");
    }

    #[test]
    fn sgr_tracks_full_attributes_and_reset_groups() {
        let mut screen = TerminalScreen::new(8, 2);
        screen.write(b"\x1b[1;2;3;4;5;7;8;9;38;5;200;48;2;1;2;3mX");
        let style = &screen.cell(0, 0).unwrap().style;
        assert!(style.bold);
        assert!(style.dim);
        assert!(style.italic);
        assert!(style.underline);
        assert!(style.blink);
        assert!(style.inverse);
        assert!(style.hidden);
        assert!(style.strikethrough);
        assert_eq!(style.foreground, Some(TerminalColor::Indexed(200)));
        assert_eq!(style.background, Some(TerminalColor::Rgb(1, 2, 3)));

        screen.write(b"\x1b[22;23;24;25;27;28;29;39;49mY");
        let style = &screen.cell(0, 1).unwrap().style;
        assert!(!style.bold && !style.dim);
        assert!(!style.italic);
        assert!(!style.underline);
        assert!(!style.blink);
        assert!(!style.inverse);
        assert!(!style.hidden);
        assert!(!style.strikethrough);
        assert_eq!(style.foreground, None);
        assert_eq!(style.background, None);
    }

    #[test]
    fn line_insert_delete_scroll_and_index_operations_respect_margins() {
        let mut screen = TerminalScreen::new(4, 4);
        screen.write(b"A\r\nB\r\nC\r\nD");
        screen.write(b"\x1b[2;1H\x1b[1L");
        assert_eq!(screen.row_text(0).as_deref(), Some("A"));
        assert_eq!(screen.row_text(1).as_deref(), Some(""));
        assert_eq!(screen.row_text(2).as_deref(), Some("B"));
        assert_eq!(screen.row_text(3).as_deref(), Some("C"));

        screen.write(b"\x1b[3;1H\x1b[1M");
        assert_eq!(screen.row_text(2).as_deref(), Some("C"));
        assert_eq!(screen.row_text(3).as_deref(), Some(""));

        screen.write(b"\x1b[1;4r\x1b[1S");
        assert_eq!(screen.row_text(0).as_deref(), Some(""));
        assert_eq!(screen.row_text(1).as_deref(), Some("C"));

        screen.write(b"\x1b[1;1H\x1bM");
        assert_eq!(screen.row_text(0).as_deref(), Some(""));
        assert_eq!(screen.row_text(1).as_deref(), Some(""));
        assert_eq!(screen.row_text(2).as_deref(), Some("C"));
    }

    #[test]
    fn alternate_modes_preserve_primary_and_obey_clear_and_cursor_rules() {
        let mut screen = TerminalScreen::new(12, 3);
        screen.write(b"\x1b[2;4Hmain");
        let main_cursor = screen.cursor();
        screen.write(b"\x1b[?47halt\x1b[?47l");
        assert_eq!(screen.row_text(1).as_deref(), Some("   main"));
        assert_eq!(screen.cursor(), main_cursor);
        screen.write(b"\x1b[?47h");
        assert_eq!(screen.row_text(0).as_deref(), Some("alt"));
        screen.write(b"\x1b[?47l\x1b[?1047h");
        assert_eq!(screen.row_text(0).as_deref(), Some(""));
        screen.write(b"temp\x1b[?1047l");
        assert_eq!(screen.row_text(1).as_deref(), Some("   main"));

        screen.write(b"\x1b[?1048h\x1b[1;1H\x1b[?1048l");
        assert_eq!(screen.cursor(), main_cursor);
        screen.write(b"\x1b[?1049halt\x1b[?1049l");
        assert_eq!(screen.row_text(1).as_deref(), Some("   main"));
        assert_eq!(screen.cursor(), main_cursor);
    }

    #[test]
    fn unicode_graphemes_and_widths_survive_stream_chunk_boundaries() {
        let mut screen = TerminalScreen::new(16, 2);
        screen.write("Aあ你e".as_bytes());
        screen.write("\u{301}".as_bytes());
        screen.write("👩".as_bytes());
        screen.write("\u{200d}".as_bytes());
        screen.write("💻".as_bytes());
        assert_eq!(screen.cell(0, 1).unwrap().width, 2);
        assert_eq!(screen.cell(0, 3).unwrap().width, 2);
        assert_eq!(screen.cell(0, 5).unwrap().text, "e\u{301}");
        assert_eq!(screen.cell(0, 6).unwrap().text, "👩\u{200d}💻");
        assert_eq!(screen.cell(0, 6).unwrap().width, 2);
        assert_eq!(screen.cell(0, 7).unwrap().width, 0);
        assert_eq!(
            screen.row_text(0).as_deref(),
            Some("Aあ你e\u{301}👩\u{200d}💻")
        );
    }

    #[test]
    fn private_cursor_and_mouse_modes_are_reported_in_render_view() {
        let mut screen = TerminalScreen::new(6, 2);
        screen.write(b"\x1b[?1h\x1b[?1002h\x1b[?1006h\x1b[31mX");
        assert!(screen.application_cursor());
        assert_eq!(screen.mouse_tracking(), MouseTracking::ButtonMotion);
        assert!(screen.sgr_mouse());
        {
            let view = screen.render_view();
            assert_eq!(view.dirty_rows, [0, 1]);
            assert_eq!(view.cells[0][0].text, "X");
            assert_eq!(
                view.cells[0][0].style.foreground,
                Some(TerminalColor::Indexed(1))
            );
        }
        assert_eq!(screen.take_dirty_rows(), Vec::<usize>::new());
    }
}

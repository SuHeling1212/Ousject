//! In-memory terminal screen model for Terminal Objects.
//!
//! This implements the control sequences currently emitted by Praxis programs
//! (cursor movement, erase, SGR, scrolling, and the common alternate-screen
//! modes). It is deliberately a screen model, not a host display driver.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalStyle {
    pub bold: bool,
    pub underline: bool,
    pub inverse: bool,
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
pub struct TerminalScreen {
    main: Buffer,
    alternate: Option<Buffer>,
    parser: ParserState,
    sequence: Vec<u8>,
    utf8_pending: Vec<u8>,
    style: TerminalStyle,
    title: String,
    cursor_visible: bool,
    autowrap: bool,
    bracketed_paste: bool,
    dirty_rows: BTreeSet<usize>,
}

impl TerminalScreen {
    #[must_use]
    pub fn new(columns: usize, rows: usize) -> Self {
        let columns = columns.clamp(1, 512);
        let rows = rows.clamp(1, 256);
        Self {
            main: Buffer::new(columns, rows),
            alternate: None,
            parser: ParserState::Ground,
            sequence: Vec::new(),
            utf8_pending: Vec::new(),
            style: TerminalStyle::default(),
            title: String::new(),
            cursor_visible: true,
            autowrap: true,
            bracketed_paste: false,
            dirty_rows: (0..rows).collect(),
        }
    }

    fn active(&self) -> &Buffer {
        self.alternate.as_ref().unwrap_or(&self.main)
    }

    fn active_mut(&mut self) -> &mut Buffer {
        self.alternate.as_mut().unwrap_or(&mut self.main)
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
        self.alternate.is_some()
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
        std::mem::take(&mut self.dirty_rows).into_iter().collect()
    }

    /// Resizes both screen buffers and marks every row dirty.
    pub fn resize(&mut self, columns: usize, rows: usize) {
        let columns = columns.clamp(1, 512);
        let rows = rows.clamp(1, 256);
        resize_buffer(&mut self.main, columns, rows);
        if let Some(alternate) = &mut self.alternate {
            resize_buffer(alternate, columns, rows);
        }
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
                let buffer = self.active_mut();
                buffer.column = 0;
                buffer.wrap_pending = false;
            }
            b'\n' | 0x0b | 0x0c => self.line_feed(),
            0x08 => {
                let buffer = self.active_mut();
                buffer.column = buffer.column.saturating_sub(1);
                buffer.wrap_pending = false;
            }
            0x09 => {
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
            match std::str::from_utf8(&self.utf8_pending) {
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
                            String::from_utf8_lossy(&self.utf8_pending[..valid]).into_owned();
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
                let buffer = self.active_mut();
                (buffer.row, buffer.column) = buffer.saved_cursor;
                clamp_cursor(buffer);
                let row = buffer.row;
                self.mark_row(row);
            }
            b'D' => self.line_feed(),
            b'E' => {
                self.active_mut().column = 0;
                self.line_feed();
            }
            b'M' => self.reverse_index(),
            b'c' => self.reset(),
            _ => {}
        }
    }

    fn csi_byte(&mut self, byte: u8) {
        if (0x40..=0x7e).contains(&byte) {
            let sequence = std::mem::take(&mut self.sequence);
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
        if let Ok(sequence) = std::str::from_utf8(&self.sequence) {
            if let Some((command, title)) = sequence.split_once(';') {
                if matches!(command, "0" | "1" | "2") {
                    self.title = title.to_owned();
                }
            }
        }
        self.sequence.clear();
    }

    fn execute_csi(&mut self, bytes: &[u8], final_byte: u8) {
        let Ok(sequence) = std::str::from_utf8(bytes) else {
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
                    25 => self.cursor_visible = enable,
                    7 => self.autowrap = enable,
                    1049 => self.alternate_screen(enable),
                    2004 => self.bracketed_paste = enable,
                    _ => {}
                }
            }
            return;
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
            b'm' => self.sgr(&values),
            b's' => {
                let buffer = self.active_mut();
                buffer.saved_cursor = (buffer.row, buffer.column);
            }
            b'u' => {
                let buffer = self.active_mut();
                (buffer.row, buffer.column) = buffer.saved_cursor;
                clamp_cursor(buffer);
                let row = buffer.row;
                self.mark_row(row);
            }
            b'r' => self.set_scroll_region(&values),
            _ => {}
        }
    }

    fn put_character(&mut self, character: char) {
        let width = character_width(character);
        if width == 0 {
            let dirty = {
                let buffer = self.active_mut();
                let column = buffer
                    .column
                    .saturating_sub(usize::from(!buffer.wrap_pending));
                let row = buffer.row;
                if let Some(cell) = buffer.cells[row].get_mut(column) {
                    cell.text.push(character);
                    Some(row)
                } else {
                    None
                }
            };
            if let Some(row) = dirty {
                self.mark_row(row);
            }
            return;
        }
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
        let style = self.style.clone();
        let mut cell = TerminalCell {
            text: character.to_string(),
            width,
            style: style.clone(),
        };
        if width == 2 && column + 1 >= columns {
            cell.width = 1;
        }
        {
            let buffer = self.active_mut();
            buffer.cells[row][column] = cell;
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
        self.mark_row(row);
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
        let row = buffer.row;
        self.mark_row(row);
    }

    fn set_column(&mut self, column: usize) {
        let buffer = self.active_mut();
        buffer.column = column.min(buffer.columns() - 1);
        buffer.wrap_pending = false;
    }

    fn set_row(&mut self, row: usize) {
        let buffer = self.active_mut();
        buffer.row = row.min(buffer.rows() - 1);
        buffer.wrap_pending = false;
        let row = buffer.row;
        self.mark_row(row);
    }

    fn set_cursor(&mut self, row: usize, column: usize) {
        let buffer = self.active_mut();
        buffer.row = row.min(buffer.rows() - 1);
        buffer.column = column.min(buffer.columns() - 1);
        buffer.wrap_pending = false;
        let row = buffer.row;
        self.mark_row(row);
    }

    fn line_feed(&mut self) {
        let buffer = self.active_mut();
        buffer.wrap_pending = false;
        if buffer.row == buffer.scroll_bottom {
            let top = buffer.scroll_top;
            let bottom = buffer.scroll_bottom;
            buffer.cells.remove(top);
            buffer.cells.insert(bottom, blank_row(buffer.columns()));
            self.dirty_rows.extend(top..=bottom);
        } else {
            buffer.row = (buffer.row + 1).min(buffer.rows() - 1);
            let row = buffer.row;
            self.dirty_rows.insert(row);
        }
    }

    fn reverse_index(&mut self) {
        let buffer = self.active_mut();
        buffer.wrap_pending = false;
        if buffer.row == buffer.scroll_top {
            let top = buffer.scroll_top;
            let bottom = buffer.scroll_bottom;
            buffer.cells.remove(bottom);
            buffer.cells.insert(top, blank_row(buffer.columns()));
            self.dirty_rows.extend(top..=bottom);
        } else {
            buffer.row = buffer.row.saturating_sub(1);
            let row = buffer.row;
            self.dirty_rows.insert(row);
        }
    }

    fn erase_display(&mut self, mode: usize) {
        let (row, column) = self.cursor();
        let rows = self.rows();
        match mode {
            0 => {
                self.erase_range(row, column, self.columns());
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
        self.erase_range(row, column, (column + count).min(self.columns()));
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
                4 => self.style.underline = true,
                7 => self.style.inverse = true,
                22 => self.style.bold = false,
                24 => self.style.underline = false,
                27 => self.style.inverse = false,
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

    fn set_scroll_region(&mut self, values: &[usize]) {
        let buffer = self.active_mut();
        let top = positive(values.first().copied().unwrap_or(0)) - 1;
        let bottom = positive(values.get(1).copied().unwrap_or(0)) - 1;
        if top < bottom && bottom < buffer.rows() {
            buffer.scroll_top = top;
            buffer.scroll_bottom = bottom;
            buffer.row = top;
            buffer.column = 0;
        }
    }

    fn alternate_screen(&mut self, enable: bool) {
        if enable && self.alternate.is_none() {
            self.alternate = Some(Buffer::new(self.main.columns(), self.main.rows()));
        } else if !enable && self.alternate.take().is_some() {
            self.dirty_rows = (0..self.main.rows()).collect();
        }
    }

    fn reset(&mut self) {
        self.main = Buffer::new(self.main.columns(), self.main.rows());
        self.alternate = None;
        self.style = TerminalStyle::default();
        self.cursor_visible = true;
        self.autowrap = true;
        self.bracketed_paste = false;
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

fn character_width(character: char) -> u8 {
    let code = u32::from(character);
    if character.is_control()
        || (0x0300..=0x036f).contains(&code)
        || (0x1ab0..=0x1aff).contains(&code)
        || (0x1dc0..=0x1dff).contains(&code)
        || (0xfe00..=0xfe0f).contains(&code)
        || (0xfe20..=0xfe2f).contains(&code)
        || (0xe0100..=0xe01ef).contains(&code)
        || code == 0x200d
        || (0x1f3fb..=0x1f3ff).contains(&code)
    {
        return 0;
    }
    if (0x1100..=0x115f).contains(&code)
        || (0x2329..=0x232a).contains(&code)
        || (0x2e80..=0xa4cf).contains(&code)
        || (0xac00..=0xd7a3).contains(&code)
        || (0xf900..=0xfaff).contains(&code)
        || (0xfe10..=0xfe19).contains(&code)
        || (0xfe30..=0xfe6f).contains(&code)
        || (0xff00..=0xff60).contains(&code)
        || (0xffe0..=0xffe6).contains(&code)
        || (0x1f300..=0x1faff).contains(&code)
        || (0x20000..=0x3fffd).contains(&code)
    {
        2
    } else {
        1
    }
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
}

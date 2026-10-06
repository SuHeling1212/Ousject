use super::super::{BTreeMap, Duration, Value};

pub(super) const MAX_KEYBOARD_EVENTS: usize = 4096;

#[derive(Debug, Clone)]
pub(crate) struct KeyEvent {
    pub(crate) key: String,
    pub(crate) text: Option<String>,
    pub(crate) control: bool,
    pub(crate) alt: bool,
    pub(crate) shift: bool,
}

impl KeyEvent {
    pub(crate) fn as_value(&self) -> Value {
        Value::Record(BTreeMap::from([
            ("key".to_owned(), Value::Text(self.key.clone())),
            (
                "text".to_owned(),
                self.text.clone().map_or(Value::Null, Value::Text),
            ),
            ("pressed".to_owned(), Value::Bool(true)),
            ("ctrl".to_owned(), Value::Bool(self.control)),
            ("alt".to_owned(), Value::Bool(self.alt)),
            ("shift".to_owned(), Value::Bool(self.shift)),
        ]))
    }
}

#[derive(Debug)]
enum PendingInput {
    Escape {
        bytes: Vec<u8>,
        since: std::time::Instant,
    },
    Utf8 {
        bytes: Vec<u8>,
        expected: usize,
    },
}

#[derive(Debug, Default)]
pub(crate) struct InputParser {
    pending: Option<PendingInput>,
}

impl InputParser {
    pub(super) fn clear_pending(&mut self) {
        self.pending = None;
    }

    pub(crate) fn push(&mut self, byte: u8) -> Vec<KeyEvent> {
        let Some(pending) = self.pending.take() else {
            return self.begin(byte);
        };
        match pending {
            PendingInput::Utf8 {
                mut bytes,
                expected,
            } => {
                if byte & 0xc0 != 0x80 {
                    let mut events = vec![replacement_event()];
                    events.extend(self.begin(byte));
                    return events;
                }
                bytes.push(byte);
                if bytes.len() == expected {
                    let text = String::from_utf8(bytes).unwrap_or_else(|_| "�".to_owned());
                    vec![KeyEvent {
                        key: "text".to_owned(),
                        text: Some(text),
                        control: false,
                        alt: false,
                        shift: false,
                    }]
                } else {
                    self.pending = Some(PendingInput::Utf8 { bytes, expected });
                    Vec::new()
                }
            }
            PendingInput::Escape { mut bytes, since } => {
                bytes.push(byte);
                if bytes.len() == 2 && !matches!(byte, b'[' | b'O') {
                    if byte.is_ascii() && !byte.is_ascii_control() {
                        return vec![KeyEvent {
                            key: "text".to_owned(),
                            text: Some(char::from(byte).to_string()),
                            control: false,
                            alt: true,
                            shift: false,
                        }];
                    }
                    return vec![special_event("escape")];
                }
                if bytes.len() > 2 && bytes[1] == b'[' && (0x40..=0x7e).contains(&byte) {
                    return vec![parse_csi(&bytes)];
                }
                if bytes.len() == 3 && bytes[1] == b'O' {
                    return vec![parse_ss3(byte)];
                }
                if bytes.len() > 32 {
                    return vec![special_event("escape")];
                }
                self.pending = Some(PendingInput::Escape { bytes, since });
                Vec::new()
            }
        }
    }

    fn begin(&mut self, byte: u8) -> Vec<KeyEvent> {
        match byte {
            0x1b => {
                self.pending = Some(PendingInput::Escape {
                    bytes: vec![byte],
                    since: std::time::Instant::now(),
                });
                Vec::new()
            }
            0xc2..=0xdf => {
                self.pending = Some(PendingInput::Utf8 {
                    bytes: vec![byte],
                    expected: 2,
                });
                Vec::new()
            }
            0xe0..=0xef => {
                self.pending = Some(PendingInput::Utf8 {
                    bytes: vec![byte],
                    expected: 3,
                });
                Vec::new()
            }
            0xf0..=0xf4 => {
                self.pending = Some(PendingInput::Utf8 {
                    bytes: vec![byte],
                    expected: 4,
                });
                Vec::new()
            }
            0x01..=0x07 | 0x0b..=0x0c | 0x0e..=0x1a => vec![KeyEvent {
                key: char::from(b'a' + byte - 1).to_string(),
                text: None,
                control: true,
                alt: false,
                shift: false,
            }],
            0x08 | 0x7f => vec![special_event("backspace")],
            b'\t' => vec![special_event("tab")],
            b'\r' | b'\n' => vec![special_event("enter")],
            0x20..=0x7e => vec![plain_event(byte)],
            _ => Vec::new(),
        }
    }

    pub(crate) fn flush_escape_timeout(&mut self, now: std::time::Instant) -> Option<KeyEvent> {
        let expired = matches!(
            self.pending,
            Some(PendingInput::Escape { since, .. })
                if now.saturating_duration_since(since) >= Duration::from_millis(35)
        );
        if expired {
            self.pending = None;
            Some(special_event("escape"))
        } else {
            None
        }
    }
}

fn plain_event(byte: u8) -> KeyEvent {
    let text = char::from(byte).to_string();
    KeyEvent {
        key: "text".to_owned(),
        text: Some(text),
        control: false,
        alt: false,
        shift: byte.is_ascii_uppercase(),
    }
}

fn replacement_event() -> KeyEvent {
    KeyEvent {
        key: "text".to_owned(),
        text: Some("�".to_owned()),
        control: false,
        alt: false,
        shift: false,
    }
}

pub(super) fn special_event(key: &str) -> KeyEvent {
    KeyEvent {
        key: key.to_owned(),
        text: None,
        control: false,
        alt: false,
        shift: false,
    }
}

fn parse_ss3(byte: u8) -> KeyEvent {
    match byte {
        b'A' => special_event("arrow_up"),
        b'B' => special_event("arrow_down"),
        b'C' => special_event("arrow_right"),
        b'D' => special_event("arrow_left"),
        b'H' => special_event("home"),
        b'F' => special_event("end"),
        b'P' => special_event("f1"),
        b'Q' => special_event("f2"),
        b'R' => special_event("f3"),
        b'S' => special_event("f4"),
        _ => special_event("unknown"),
    }
}

fn parse_csi(bytes: &[u8]) -> KeyEvent {
    let Some((&final_byte, parameters)) = bytes.get(2..).and_then(|body| body.split_last()) else {
        return special_event("unknown");
    };
    let parameters = std::str::from_utf8(parameters).unwrap_or_default();
    let codes = parameters
        .split(';')
        .filter_map(|value| value.parse::<u16>().ok())
        .collect::<Vec<_>>();
    let modifier = codes.get(1).copied().unwrap_or(1).saturating_sub(1);
    let shift = modifier & 1 != 0;
    let alt = modifier & 2 != 0;
    let control = modifier & 4 != 0;
    let key = match final_byte {
        b'A' => "arrow_up",
        b'B' => "arrow_down",
        b'C' => "arrow_right",
        b'D' => "arrow_left",
        b'H' => "home",
        b'F' => "end",
        b'~' => match codes.first().copied().unwrap_or_default() {
            1 | 7 => "home",
            2 => "insert",
            3 => "delete",
            4 | 8 => "end",
            5 => "page_up",
            6 => "page_down",
            11 => "f1",
            12 => "f2",
            13 => "f3",
            14 => "f4",
            15 => "f5",
            17 => "f6",
            18 => "f7",
            19 => "f8",
            20 => "f9",
            21 => "f10",
            23 => "f11",
            24 => "f12",
            _ => "unknown",
        },
        _ => "unknown",
    };
    KeyEvent {
        key: key.to_owned(),
        text: None,
        control,
        alt,
        shift,
    }
}

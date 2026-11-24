//! Server-sent events, split incrementally. Data bytes are handed on as they arrive, so
//! a streamed token reaches the json scanner while the rest of the event is in flight.

pub trait SseSink {
    /// The start of an event's data (the first `data:` line).
    fn begin(&mut self);
    /// Bytes of the data field. Multiple data lines are joined with '\n'.
    fn data(&mut self, b: &[u8]);
    /// A blank line ended an event that had data.
    fn dispatch(&mut self);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    LineStart,
    Field,
    ValueStart,
    Value,
    Skip,
}

pub struct Sse {
    st: St,
    field: [u8; 8],
    flen: usize,
    is_data: bool,
    has_data: bool,
    after_cr: bool,
}

impl Default for Sse {
    fn default() -> Self {
        Self::new()
    }
}

impl Sse {
    pub fn new() -> Sse {
        Sse { st: St::LineStart, field: [0; 8], flen: 0, is_data: false, has_data: false, after_cr: false }
    }

    fn line_end(&mut self, sink: &mut dyn SseSink) {
        if self.st == St::LineStart {
            // blank line: dispatch
            if self.has_data {
                sink.dispatch();
                self.has_data = false;
            }
        } else if self.st == St::Field {
            // a field name with no colon and no value
            self.start_value(sink);
        }
        self.st = St::LineStart;
    }

    fn start_value(&mut self, sink: &mut dyn SseSink) {
        self.is_data = &self.field[..self.flen] == b"data";
        if self.is_data {
            if self.has_data {
                sink.data(b"\n");
            } else {
                sink.begin();
                self.has_data = true;
            }
        }
    }

    pub fn feed(&mut self, data: &[u8], sink: &mut dyn SseSink) {
        let mut i = 0;
        while i < data.len() {
            let b = data[i];
            if self.after_cr {
                self.after_cr = false;
                if b == b'\n' {
                    i += 1;
                    continue;
                }
            }
            if b == b'\r' || b == b'\n' {
                self.after_cr = b == b'\r';
                i += 1;
                self.line_end(sink);
                continue;
            }
            match self.st {
                St::LineStart => {
                    if b == b':' {
                        self.st = St::Skip; // comment
                    } else {
                        self.flen = 0;
                        self.st = St::Field;
                        continue;
                    }
                    i += 1;
                }
                St::Field => {
                    i += 1;
                    if b == b':' {
                        self.start_value(sink);
                        self.st = St::ValueStart;
                    } else if self.flen < self.field.len() {
                        self.field[self.flen] = b;
                        self.flen += 1;
                    } else {
                        self.flen = self.field.len();
                        self.field[0] = 0; // too long to be a field we use
                    }
                }
                St::ValueStart => {
                    self.st = St::Value;
                    if b == b' ' {
                        i += 1;
                    }
                }
                St::Value => {
                    let start = i;
                    while i < data.len() && data[i] != b'\r' && data[i] != b'\n' {
                        i += 1;
                    }
                    if self.is_data {
                        sink.data(&data[start..i]);
                    }
                }
                St::Skip => {
                    while i < data.len() && data[i] != b'\r' && data[i] != b'\n' {
                        i += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Events {
        cur: Vec<u8>,
        all: Vec<String>,
    }
    impl SseSink for Events {
        fn begin(&mut self) {
            self.cur.clear();
        }
        fn data(&mut self, b: &[u8]) {
            self.cur.extend_from_slice(b);
        }
        fn dispatch(&mut self) {
            self.all.push(String::from_utf8(self.cur.clone()).unwrap());
        }
    }

    #[test]
    fn splits_events_in_any_chunking() {
        let raw: &[u8] = b"event: message_start\ndata: {\"a\":1}\n\n: ping\r\n\r\nevent: x\r\ndata: one\r\ndata:two\r\n\r\ndata\n\nretry: 5\n\n";
        for size in 1..raw.len() {
            let mut s = Sse::new();
            let mut e = Events::default();
            for c in raw.chunks(size) {
                s.feed(c, &mut e);
            }
            assert_eq!(e.all, vec!["{\"a\":1}", "one\ntwo", ""], "size {size}");
        }
    }
}

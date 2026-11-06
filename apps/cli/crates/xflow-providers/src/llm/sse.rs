//! Server-sent events over `Response::chunk()`; chunks may split lines and UTF-8.
#[derive(Debug, Default, PartialEq)]
pub struct Frame {
    pub event: String,
    pub data: String,
}
#[derive(Default)]
pub struct Parser {
    line: Vec<u8>,
    frame: Frame,
    has_data: bool,
    after_cr: bool,
}
impl Parser {
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<Frame>) {
        for &byte in chunk {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            if matches!(byte, b'\n' | b'\r') {
                self.after_cr = byte == b'\r';
                self.end_line(out);
            } else {
                self.line.push(byte);
            }
        }
    }
    /// End of body: a last frame may lack its blank line.
    pub fn finish(&mut self, out: &mut Vec<Frame>) {
        self.end_line(out);
        self.end_line(out);
    }
    fn end_line(&mut self, out: &mut Vec<Frame>) {
        let bytes = std::mem::take(&mut self.line);
        let line = String::from_utf8_lossy(&bytes);
        if line.is_empty() {
            if std::mem::take(&mut self.has_data) {
                out.push(std::mem::take(&mut self.frame));
            }
            self.frame.event.clear();
            return;
        }
        // A `:` line is a comment; the caller's clock already counted its bytes.
        let (field, value) = line.split_once(':').unwrap_or((&line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.frame.event = value.to_owned(),
            "data" => {
                if std::mem::replace(&mut self.has_data, true) {
                    self.frame.data.push('\n');
                }
                self.frame.data.push_str(value);
            }
            _ => {}
        }
    }
}

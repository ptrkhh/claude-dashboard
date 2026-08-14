use std::collections::VecDeque;
use std::sync::Mutex;
use time::OffsetDateTime;

const MAX_LINES: usize = 200;

/// A 200-entry ring of `HH:MM:SS`-prefixed lines, mirroring
/// `logBuffer` in `lib/collect.js:21-27`. Also echoes to stderr.
pub struct LogBuffer {
    lines: Mutex<VecDeque<String>>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl LogBuffer {
    pub fn new() -> Self {
        Self { lines: Mutex::new(VecDeque::with_capacity(MAX_LINES)) }
    }

    pub fn push(&self, line: impl AsRef<str>) {
        let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        let stamped = format!(
            "{:02}:{:02}:{:02} {}",
            now.hour(),
            now.minute(),
            now.second(),
            line.as_ref()
        );
        eprintln!("{stamped}");
        // A poisoned mutex must not take down logging; recover the guard.
        let mut guard = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() == MAX_LINES {
            guard.pop_front();
        }
        guard.push_back(stamped);
    }

    pub fn lines(&self) -> Vec<String> {
        let guard = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        guard.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_at_most_200_entries_dropping_oldest() {
        let buf = LogBuffer::new();
        for i in 0..250 {
            buf.push(format!("line {i}"));
        }
        let lines = buf.lines();
        assert_eq!(lines.len(), 200);
        assert!(lines[0].ends_with("line 50"));
        assert!(lines[199].ends_with("line 249"));
    }

    #[test]
    fn each_line_carries_an_hhmmss_prefix() {
        let buf = LogBuffer::new();
        buf.push("hello");
        let line = buf.lines().remove(0);
        assert_eq!(line.len(), "00:00:00 hello".len());
        assert_eq!(&line[2..3], ":");
        assert_eq!(&line[5..6], ":");
        assert!(line.ends_with(" hello"));
    }
}

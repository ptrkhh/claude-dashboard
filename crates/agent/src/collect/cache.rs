use super::fsio::read_if;
use crate::parse::transcript::{parse_transcript, Transcript};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

/// Mirrors `TRANSCRIPT_CACHE_MAX` (`lib/collect.js:50`).
pub const TRANSCRIPT_CACHE_MAX: usize = 200;

/// Node compared `stat.mtimeMs`, a float millisecond count. Keeping the same
/// representation keeps the revalidation predicate identical.
pub fn mtime_ms(md: &std::fs::Metadata) -> f64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

struct Entry {
    mtime_ms: f64,
    result: Transcript,
}

/// Memoized transcript parse, keyed by path and revalidated by mtime.
/// Mirrors `parseTranscriptCached` (`lib/collect.js:51-62`).
pub struct TranscriptCache {
    map: Mutex<HashMap<PathBuf, Entry>>,
}

impl Default for TranscriptCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptCache {
    pub fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub async fn get(&self, file: &Path) -> Option<Transcript> {
        let md = tokio::fs::metadata(file).await.ok()?;
        let stamp = mtime_ms(&md);
        {
            let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = map.get(file) {
                if hit.mtime_ms == stamp {
                    return Some(hit.result.clone());
                }
            }
        }
        let txt = read_if(file).await?;
        let result = parse_transcript(&txt);
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        // ponytail: crude cap, same as Node's — swap for an LRU only if a
        // profile ever shows the reparse storm mattering.
        if map.len() >= TRANSCRIPT_CACHE_MAX {
            map.clear();
        }
        map.insert(file.to_path_buf(), Entry { mtime_ms: stamp, result: result.clone() });
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-cache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn msg(text: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
        )
    }

    /// `utimensat` via std is not exposed, and `filetime` is a dependency this
    /// crate does not need in production. `rustix` is already a dependency and
    /// has the syscall.
    fn filetime_set(p: &Path, t: SystemTime) {
        let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        let stamp = rustix::fs::Timespec {
            tv_sec: d.as_secs() as _,
            tv_nsec: d.subsec_nanos() as _,
        };
        let ts = rustix::fs::Timestamps { last_access: stamp, last_modification: stamp };
        rustix::fs::utimensat(rustix::fs::CWD, p, &ts, rustix::fs::AtFlags::empty()).unwrap();
    }

    #[tokio::test]
    async fn missing_file_yields_none() {
        let c = TranscriptCache::new();
        assert!(c.get(Path::new("/no/such/cdash.jsonl")).await.is_none());
    }

    #[tokio::test]
    async fn an_unchanged_mtime_serves_the_memoized_parse_without_rereading() {
        // Node asserted object identity. Rust returns a clone, so identity
        // cannot be the assertion — instead the file is rewritten with its
        // mtime restored. A cache that re-read would see "second".
        let dir = tempdir("hit");
        let f = dir.join("x.jsonl");
        std::fs::write(&f, msg("first")).unwrap();
        let stamp = std::fs::metadata(&f).unwrap().modified().unwrap();

        let c = TranscriptCache::new();
        assert_eq!(c.get(&f).await.unwrap().last_assistant_text.as_deref(), Some("first"));

        std::fs::write(&f, msg("second")).unwrap();
        filetime_set(&f, stamp);
        assert_eq!(
            c.get(&f).await.unwrap().last_assistant_text.as_deref(),
            Some("first"),
            "same mtime must serve the cached parse"
        );
    }

    #[tokio::test]
    async fn a_changed_mtime_forces_a_reparse() {
        let dir = tempdir("miss");
        let f = dir.join("x.jsonl");
        std::fs::write(&f, msg("first")).unwrap();

        let c = TranscriptCache::new();
        assert_eq!(c.get(&f).await.unwrap().last_assistant_text.as_deref(), Some("first"));

        std::fs::write(&f, msg("second")).unwrap();
        filetime_set(&f, SystemTime::now() + Duration::from_secs(60));
        assert_eq!(c.get(&f).await.unwrap().last_assistant_text.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn the_cache_is_cleared_at_the_cap_rather_than_growing_without_bound() {
        let dir = tempdir("cap");
        let c = TranscriptCache::new();
        for i in 0..=TRANSCRIPT_CACHE_MAX {
            let f = dir.join(format!("{i}.jsonl"));
            std::fs::write(&f, msg("t")).unwrap();
            c.get(&f).await;
        }
        assert!(c.len() <= TRANSCRIPT_CACHE_MAX, "cache must not exceed its cap");
    }
}

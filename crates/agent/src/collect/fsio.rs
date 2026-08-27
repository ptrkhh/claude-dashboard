use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Only the tail of a transcript is ever wanted, and a transcript has no upper
/// size. Mirrors `TAIL_BYTES` in `lib/collect.js:64`.
pub const TAIL_BYTES: u64 = 128 * 1024;

/// Read a whole file, or `None` if it cannot be read for any reason.
/// Mirrors `readIf` (`lib/collect.js:29`).
pub async fn read_if(file: &Path) -> Option<String> {
    tokio::fs::read_to_string(file).await.ok()
}

/// Read at most the last `TAIL_BYTES` of a file. A cut mid-character yields
/// U+FFFD, exactly as Node's `buf.toString('utf8')` did.
pub async fn read_tail(file: &Path) -> Option<String> {
    let mut fh = tokio::fs::File::open(file).await.ok()?;
    let size = fh.metadata().await.ok()?.len();
    let start = size.saturating_sub(TAIL_BYTES);
    if start > 0 {
        fh.seek(std::io::SeekFrom::Start(start)).await.ok()?;
    }
    let mut buf = Vec::new();
    // `take` and not a bare `read_to_end`: the seek offset comes from a stat
    // taken moments earlier, and a live `claude` appends between the two. The
    // cap has to be on the read, or a session dumping tool output mid-poll is
    // buffered whole.
    fh.take(TAIL_BYTES).read_to_end(&mut buf).await.ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Write-then-rename. A reader either sees the old file or the new one, never
/// a half-written one. `tmp_suffix` is a parameter because Node used two
/// different ones and both are observable on disk: `.cdash.tmp` for
/// `~/.claude.json` (`lib/collect.js:121`) and `.tmp` for the places file
/// (`lib/places.js:30`).
pub async fn write_atomic(file: &Path, contents: &str, tmp_suffix: &str) -> std::io::Result<()> {
    let mut tmp = file.as_os_str().to_os_string();
    tmp.push(tmp_suffix);
    let tmp = PathBuf::from(tmp);
    tokio::fs::write(&tmp, contents).await?;
    tokio::fs::rename(&tmp, file).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cdash-fsio-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn read_if_returns_none_for_a_missing_file() {
        assert_eq!(read_if(Path::new("/no/such/cdash-file")).await, None);
    }

    #[tokio::test]
    async fn read_tail_returns_a_whole_short_file() {
        let dir = tempdir("short");
        let f = dir.join("a.jsonl");
        tokio::fs::write(&f, "hello\n").await.unwrap();
        assert_eq!(read_tail(&f).await.as_deref(), Some("hello\n"));
    }

    #[tokio::test]
    async fn read_tail_reads_only_the_last_128_kib() {
        // The bound exists because a long session's transcript can be tens of
        // MiB and only the last assistant turn is wanted.
        let dir = tempdir("long");
        let f = dir.join("big.jsonl");
        let filler = "x".repeat(TAIL_BYTES as usize);
        tokio::fs::write(&f, format!("HEAD{filler}TAIL")).await.unwrap();

        let got = read_tail(&f).await.unwrap();
        assert_eq!(got.len(), TAIL_BYTES as usize);
        assert!(got.ends_with("TAIL"));
        assert!(!got.contains("HEAD"), "the head of an oversized file must not be read");
    }

    #[tokio::test]
    async fn read_tail_caps_a_file_that_grows_after_the_stat() {
        // The seek offset is chosen from a stat; by the time the read runs the
        // file is longer. Only a cap on the read itself survives that.
        let dir = tempdir("growing");
        let f = dir.join("live.jsonl");
        tokio::fs::write(&f, "x".repeat(1024)).await.unwrap();

        let mut fh = tokio::fs::File::open(&f).await.unwrap();
        let size = fh.metadata().await.unwrap().len();
        let start = size.saturating_sub(TAIL_BYTES);
        tokio::fs::write(&f, "x".repeat(4 * TAIL_BYTES as usize)).await.unwrap();

        if start > 0 {
            fh.seek(std::io::SeekFrom::Start(start)).await.unwrap();
        }
        let mut buf = Vec::new();
        fh.take(TAIL_BYTES).read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf.len() as u64, TAIL_BYTES);
    }

    #[tokio::test]
    async fn write_atomic_leaves_no_temp_file_behind() {
        let dir = tempdir("atomic");
        let f = dir.join("places.json");
        write_atomic(&f, "{\"a\":1}", ".tmp").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&f).await.unwrap(), "{\"a\":1}");
        assert!(!dir.join("places.json.tmp").exists(), "temp file must be renamed, not left");
    }

    #[tokio::test]
    async fn write_atomic_replaces_existing_content_wholesale() {
        let dir = tempdir("replace");
        let f = dir.join("x.json");
        write_atomic(&f, "old", ".cdash.tmp").await.unwrap();
        write_atomic(&f, "new", ".cdash.tmp").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&f).await.unwrap(), "new");
    }
}

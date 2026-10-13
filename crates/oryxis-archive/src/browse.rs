//! Virtual navigation inside a zip archive, without extraction.
//!
//! Zip keeps its full index (the central directory) at the end of the
//! file, so over any [`RangedSource`] the whole listing costs a few
//! positioned reads of the archive tail; a single entry is then
//! decompressed by fetching only its byte range. This is what lets the
//! SFTP pane "enter" a multi-gigabyte remote zip for KiBs of traffic,
//! with zero tooling on the server.

use std::io::{Read, Write};
use std::sync::atomic::AtomicU64;

use crate::ArchiveError;
use crate::ranged::{CachedRangeReader, RangedSource};
use crate::zip_entry_label;

/// Chunk geometry for the underlying reader: a few times the SFTP
/// 255 KiB per-request ceiling, with enough slots that the central
/// directory and one streaming window coexist.
const CHUNK_SIZE: u64 = 512 * 1024;
const MAX_CHUNKS: usize = 32;

/// One entry of the archive index, normalized for display.
#[derive(Debug, Clone)]
pub struct ZipEntryMeta {
    /// Index into the archive, stable across re-opens (feeds
    /// [`extract_entry`]).
    pub index: usize,
    /// `/`-separated path inside the archive, no leading or trailing
    /// slash (directories lose their trailing `/` here; see `is_dir`).
    pub name: String,
    pub is_dir: bool,
    /// Uncompressed size.
    pub size: u64,
    pub compressed: u64,
    /// Modification time as unix seconds, when the archive carries one.
    pub mtime_unix: Option<u32>,
    pub encrypted: bool,
}

/// Parsed archive index.
#[derive(Debug, Clone, Default)]
pub struct ZipIndex {
    pub entries: Vec<ZipEntryMeta>,
    /// Entries skipped because their path escapes the archive root
    /// (absolute or `..`-bearing). Surfaced so the UI can mention it
    /// instead of silently hiding data.
    pub skipped_unsafe: usize,
}

/// A row for the virtual directory listing at one level.
#[derive(Debug, Clone)]
pub struct VirtualEntry {
    /// Bare child name (no path).
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_unix: Option<u32>,
    /// Archive entry index for files (and for directories that have an
    /// explicit entry); `None` for directories that exist only as path
    /// prefixes of deeper entries.
    pub index: Option<usize>,
    pub encrypted: bool,
}

/// Read the central directory of the archive behind `src`.
pub fn read_index<S: RangedSource>(src: S) -> Result<ZipIndex, ArchiveError> {
    let reader = CachedRangeReader::new(src, CHUNK_SIZE, MAX_CHUNKS)?;
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| ArchiveError::Malformed(format!("not a readable zip: {e}")))?;
    let mut index = ZipIndex::default();
    for i in 0..archive.len() {
        // `by_index_raw` never touches entry data, only metadata.
        let entry = archive
            .by_index_raw(i)
            .map_err(|e| ArchiveError::Malformed(format!("zip entry {i}: {e}")))?;
        let raw_name = entry
            .name()
            .map_err(|e| ArchiveError::Malformed(format!("zip entry {i} name: {e}")))?;
        let Some(name) = normalize_entry_name(&raw_name) else {
            index.skipped_unsafe += 1;
            continue;
        };
        if name.is_empty() {
            // A bare "/" or "." entry: nothing to show.
            continue;
        }
        index.entries.push(ZipEntryMeta {
            index: i,
            is_dir: entry.is_dir(),
            size: entry.size(),
            compressed: entry.compressed_size(),
            mtime_unix: entry.last_modified().and_then(datetime_to_unix),
            encrypted: entry.encrypted(),
            name,
        });
    }
    Ok(index)
}

/// Normalize an entry path for listing: `\` to `/`, strip leading and
/// trailing slashes and `./` segments. Returns `None` when the path
/// tries to escape the root (`..` component or absolute after a drive
/// prefix), which in a listing context means "do not show".
fn normalize_entry_name(raw: &str) -> Option<String> {
    let cleaned = raw.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in cleaned.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            p if p.contains(':') => return None,
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// List the immediate children of `inner` (`""` for the archive root,
/// otherwise a `/`-separated path with no trailing slash). Directories
/// come from explicit dir entries AND from path prefixes of deeper
/// entries, deduplicated.
pub fn list_dir(index: &ZipIndex, inner: &str) -> Vec<VirtualEntry> {
    let prefix_len = if inner.is_empty() { 0 } else { inner.len() + 1 };
    let mut out: Vec<VirtualEntry> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for entry in &index.entries {
        let rel = if inner.is_empty() {
            entry.name.as_str()
        } else if entry.name.starts_with(inner)
            && entry.name.as_bytes().get(inner.len()) == Some(&b'/')
        {
            &entry.name[prefix_len..]
        } else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        match rel.split_once('/') {
            None => {
                // Direct child.
                let child = VirtualEntry {
                    name: rel.to_string(),
                    is_dir: entry.is_dir,
                    size: entry.size,
                    mtime_unix: entry.mtime_unix,
                    index: Some(entry.index),
                    encrypted: entry.encrypted,
                };
                match seen.get(rel) {
                    Some(&slot) => {
                        // An explicit entry wins over a synthesized dir.
                        out[slot] = child;
                    }
                    None => {
                        seen.insert(rel.to_string(), out.len());
                        out.push(child);
                    }
                }
            }
            Some((first, _rest)) => {
                // Deeper entry: materialize the intermediate directory.
                if !seen.contains_key(first) {
                    seen.insert(first.to_string(), out.len());
                    out.push(VirtualEntry {
                        name: first.to_string(),
                        is_dir: true,
                        size: 0,
                        mtime_unix: None,
                        index: None,
                        encrypted: false,
                    });
                }
            }
        }
    }
    out
}

/// All FILE entries at or under `inner_dir` (pass `""` for the whole
/// archive), as `(entry index, path relative to inner_dir)` pairs, plus
/// the relative directory paths needed to recreate the tree. Drives
/// "copy this folder out of the zip".
pub fn entries_under(index: &ZipIndex, inner_dir: &str) -> (Vec<(usize, String)>, Vec<String>) {
    let prefix_len = if inner_dir.is_empty() { 0 } else { inner_dir.len() + 1 };
    let mut files = Vec::new();
    let mut dirs = std::collections::BTreeSet::new();
    for entry in &index.entries {
        let rel = if inner_dir.is_empty() {
            entry.name.as_str()
        } else if entry.name.starts_with(inner_dir)
            && entry.name.as_bytes().get(inner_dir.len()) == Some(&b'/')
        {
            &entry.name[prefix_len..]
        } else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        if entry.is_dir {
            dirs.insert(rel.to_string());
        } else {
            // Parent chain of a file is also a directory to create.
            let mut acc = String::new();
            for part in rel.split('/').rev().skip(1).collect::<Vec<_>>().into_iter().rev() {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(part);
                dirs.insert(acc.clone());
            }
            files.push((entry.index, rel.to_string()));
        }
    }
    (files, dirs.into_iter().collect())
}

/// Open-once reader for extracting SEVERAL entries: the central
/// directory is parsed a single time and every `extract_to` call only
/// fetches its entry's byte range. This is what "copy a folder out of
/// the zip" iterates over.
pub struct ZipReader<S: RangedSource> {
    archive: zip::ZipArchive<CachedRangeReader<S>>,
}

impl<S: RangedSource> ZipReader<S> {
    pub fn open(src: S) -> Result<Self, ArchiveError> {
        let reader = CachedRangeReader::new(src, CHUNK_SIZE, MAX_CHUNKS)?;
        let archive = zip::ZipArchive::new(reader)
            .map_err(|e| ArchiveError::Malformed(format!("not a readable zip: {e}")))?;
        Ok(Self { archive })
    }

    /// Decompress the entry at `index` into `out`, streaming. Returns
    /// the byte count written. `progress`, when given, accumulates
    /// uncompressed bytes for a live bar.
    pub fn extract_to<W: Write>(
        &mut self,
        index: usize,
        mut out: W,
        progress: Option<&AtomicU64>,
    ) -> Result<u64, ArchiveError> {
        let mut entry = self.archive.by_index(index).map_err(|e| match e {
            zip::result::ZipError::UnsupportedArchive(msg) => {
                ArchiveError::Unsupported(format!("unsupported zip entry: {msg}"))
            }
            other => ArchiveError::Malformed(format!("zip entry {index}: {other}")),
        })?;
        if entry.encrypted() {
            return Err(ArchiveError::Unsupported(format!(
                "entry is password-protected: {}",
                zip_entry_label(&entry)
            )));
        }
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = entry.read(&mut buf).map_err(|e| {
                ArchiveError::Malformed(format!("decompress {}: {e}", zip_entry_label(&entry)))
            })?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            total += n as u64;
            if let Some(p) = progress {
                p.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            }
        }
        out.flush()?;
        Ok(total)
    }
}

/// Decompress one entry into `out` (single-shot convenience over
/// [`ZipReader`]). Returns the byte count written.
pub fn extract_entry<S: RangedSource, W: Write>(
    src: S,
    entry_index: usize,
    out: W,
    progress: Option<&AtomicU64>,
) -> Result<u64, ArchiveError> {
    ZipReader::open(src)?.extract_to(entry_index, out, progress)
}

/// Convert the zip DOS timestamp to unix seconds (UTC-naive, like every
/// zip tool). Uses the days-from-civil algorithm; no chrono dependency.
fn datetime_to_unix(dt: zip::DateTime) -> Option<u32> {
    let (y, mo, d) = (dt.year() as i64, dt.month() as i64, dt.day() as i64);
    let (h, mi, s) = (dt.hour() as i64, dt.minute() as i64, dt.second() as i64);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y_adj = if mo <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3_600 + mi * 60 + s;
    u32::try_from(secs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    /// Build a test archive: nested files with no explicit dir entries,
    /// one explicit empty dir, one root file.
    fn sample_zip() -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("root.txt", opts).unwrap();
        w.write_all(b"at the root").unwrap();
        w.start_file("docs/readme.md", opts).unwrap();
        w.write_all(b"# hello").unwrap();
        w.start_file("docs/sub/data.bin", opts).unwrap();
        w.write_all(&[7u8; 3000]).unwrap();
        w.add_directory("empty", opts).unwrap();
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn index_and_root_listing() {
        let index = read_index(Cursor::new(sample_zip())).unwrap();
        assert_eq!(index.skipped_unsafe, 0);
        let mut names: Vec<(String, bool)> = list_dir(&index, "")
            .into_iter()
            .map(|e| (e.name, e.is_dir))
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                ("docs".to_string(), true),
                ("empty".to_string(), true),
                ("root.txt".to_string(), false),
            ]
        );
    }

    #[test]
    fn nested_listing_and_sizes() {
        let index = read_index(Cursor::new(sample_zip())).unwrap();
        let docs = list_dir(&index, "docs");
        assert_eq!(docs.len(), 2);
        let sub = list_dir(&index, "docs/sub");
        assert_eq!(sub.len(), 1);
        assert_eq!(sub[0].name, "data.bin");
        assert!(!sub[0].is_dir);
        assert_eq!(sub[0].size, 3000);
        assert!(sub[0].mtime_unix.is_some());
        assert!(list_dir(&index, "empty").is_empty());
        assert!(list_dir(&index, "nope").is_empty());
    }

    #[test]
    fn extract_single_entry_roundtrip() {
        let index = read_index(Cursor::new(sample_zip())).unwrap();
        let target = index
            .entries
            .iter()
            .find(|e| e.name == "docs/sub/data.bin")
            .unwrap();
        let mut out = Vec::new();
        let progress = AtomicU64::new(0);
        let n = extract_entry(
            Cursor::new(sample_zip()),
            target.index,
            &mut out,
            Some(&progress),
        )
        .unwrap();
        assert_eq!(n, 3000);
        assert_eq!(out, vec![7u8; 3000]);
        assert_eq!(progress.load(std::sync::atomic::Ordering::Relaxed), 3000);
    }

    #[test]
    fn entries_under_collects_files_and_dirs() {
        let index = read_index(Cursor::new(sample_zip())).unwrap();
        let (files, dirs) = entries_under(&index, "docs");
        let names: Vec<&str> = files.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, vec!["readme.md", "sub/data.bin"]);
        assert_eq!(dirs, vec!["sub".to_string()]);
        let (all_files, all_dirs) = entries_under(&index, "");
        assert_eq!(all_files.len(), 3);
        assert!(all_dirs.contains(&"docs/sub".to_string()));
        assert!(all_dirs.contains(&"empty".to_string()));
    }

    #[test]
    fn hostile_entry_names_are_skipped() {
        // Hand-assemble names the writer API would reject: use the raw
        // normalizer directly.
        assert_eq!(normalize_entry_name("../evil"), None);
        assert_eq!(normalize_entry_name("a/../../evil"), None);
        assert_eq!(normalize_entry_name("C:\\evil"), None);
        assert_eq!(normalize_entry_name("/abs/path"), Some("abs/path".into()));
        assert_eq!(normalize_entry_name("./a/./b"), Some("a/b".into()));
        assert_eq!(normalize_entry_name("a\\b"), Some("a/b".into()));
    }

    #[test]
    fn dos_datetime_conversion() {
        // 2026-07-17 12:30:00 UTC = 1784291400.
        let dt = zip::DateTime::from_date_and_time(2026, 7, 17, 12, 30, 0).unwrap();
        assert_eq!(datetime_to_unix(dt), Some(1_784_291_400));
        // Epoch-adjacent: zip cannot represent pre-1980, so 1980-01-01.
        let dt = zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(datetime_to_unix(dt), Some(315_532_800));
    }
}

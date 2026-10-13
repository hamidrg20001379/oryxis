//! Local-pane archive operations: pure Rust, no shell.
//!
//! All functions are synchronous and built to run inside
//! `tokio::task::spawn_blocking` (or the app's off-thread listing
//! pattern). The UI shows a busy state while they run; per-byte
//! progress is deliberately not threaded through (matching the remote
//! exec side, which cannot report it either).

use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path};

use crate::ArchiveError;
use crate::names::ArchiveKind;
use crate::zip_entry_label;

/// Extract `archive` into `dest` (created if missing; the caller picks
/// a fresh directory name). Format from `kind`; local extraction
/// supports zip, tar.gz and tar (the exotic tar codecs are remote-only,
/// where the host's tar provides them).
pub fn extract_archive(kind: ArchiveKind, archive: &Path, dest: &Path) -> Result<(), ArchiveError> {
    std::fs::create_dir_all(dest)?;
    match kind {
        ArchiveKind::Zip => extract_zip(archive, dest),
        ArchiveKind::TarGz => {
            let file = File::open(archive)?;
            extract_tar(flate2::read::GzDecoder::new(file), dest)
        }
        ArchiveKind::Tar => extract_tar(File::open(archive)?, dest),
        other => Err(ArchiveError::Unsupported(format!(
            "local extraction does not support {}",
            other.extension()
        ))),
    }
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<(), ArchiveError> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| ArchiveError::Malformed(format!("not a readable zip: {e}")))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| ArchiveError::Malformed(format!("zip entry {i}: {e}")))?;
        if entry.encrypted() {
            return Err(ArchiveError::Unsupported(format!(
                "entry is password-protected: {}",
                zip_entry_label(&entry)
            )));
        }
        // `enclosed_name` is the zip-slip guard: it refuses absolute
        // paths and `..` escapes. An entry failing it is hostile or
        // corrupt; stop rather than silently drop data.
        let Some(rel) = entry.enclosed_name() else {
            return Err(ArchiveError::Unsupported(format!(
                "entry path escapes the archive root: {}",
                zip_entry_label(&entry)
            )));
        };
        let target = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        if entry
            .unix_mode()
            .is_some_and(|m| m & 0o170_000 == 0o120_000)
        {
            // Symlink entry: content is the target path. `enclosed_name`
            // only vetted the entry's OWN path, not where the link
            // points. A link to an absolute path or one climbing out
            // with `..` would let a LATER file entry (`link/inner`)
            // write THROUGH it, outside `dest`, because `File::create`
            // follows symlinks. Refuse those; a link that stays relative
            // and `..`-free can only resolve back inside `dest`.
            let mut link_target = String::new();
            entry
                .read_to_string(&mut link_target)
                .map_err(|e| ArchiveError::Malformed(format!("symlink {}: {e}", target.display())))?;
            let tp = Path::new(&link_target);
            if tp.is_absolute()
                || tp
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(ArchiveError::Unsupported(format!(
                    "symlink target escapes the archive root: {} -> {link_target}",
                    zip_entry_label(&entry)
                )));
            }
            std::os::unix::fs::symlink(&link_target, &target)?;
            continue;
        }
        let mut out = File::create(&target)?;
        io::copy(&mut entry, &mut out)
            .map_err(|e| ArchiveError::Malformed(format!("decompress {}: {e}", target.display())))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            // Mask to the permission bits only (0o777), dropping
            // setuid / setgid / sticky (0o7000): Info-ZIP's `unzip`
            // strips them too, since honouring them from an untrusted
            // archive is a needless risk (a setgid file dropped into a
            // shared-group directory, say).
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

fn extract_tar<R: Read>(reader: R, dest: &Path) -> Result<(), ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    // `unpack` refuses entries that escape `dest` (the tar equivalent
    // of the zip-slip guard) and restores permissions and symlinks.
    archive
        .unpack(dest)
        .map_err(|e| ArchiveError::Malformed(format!("tar extraction failed: {e}")))?;
    Ok(())
}

/// Create archive `out` from `items` (names relative to `cwd`, straight
/// from the pane listing). Only zip and tar.gz are offered for
/// creation, mirroring the remote side.
pub fn compress(
    kind: ArchiveKind,
    cwd: &Path,
    items: &[String],
    out: &Path,
) -> Result<(), ArchiveError> {
    if items.is_empty() {
        return Err(ArchiveError::Unsupported("nothing to compress".into()));
    }
    for item in items {
        // Listing names are single components; anything else means a
        // caller bug and could escape `cwd`.
        let p = Path::new(item);
        if p.components().count() != 1
            || !matches!(p.components().next(), Some(Component::Normal(_)))
        {
            return Err(ArchiveError::UnsafeName(format!(
                "not a plain directory entry name: {item:?}"
            )));
        }
    }
    let result = match kind {
        ArchiveKind::Zip => compress_zip(cwd, items, out),
        ArchiveKind::TarGz => compress_tar_gz(cwd, items, out),
        other => Err(ArchiveError::Unsupported(format!(
            "archive creation does not support {}",
            other.extension()
        ))),
    };
    if result.is_err() {
        // Never leave a half-written archive that looks real.
        let _ = std::fs::remove_file(out);
    }
    result
}

fn compress_zip(cwd: &Path, items: &[String], out: &Path) -> Result<(), ArchiveError> {
    let mut w = zip::ZipWriter::new(File::create(out)?);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        // Entries above 4 GiB need zip64; enabling it up front avoids
        // a mid-write failure on the first big file.
        .large_file(true);
    for item in items {
        let root = cwd.join(item);
        let meta = std::fs::symlink_metadata(&root)?;
        if meta.is_dir() {
            zip_add_dir(&mut w, &root, item, opts)?;
        } else {
            zip_add_leaf(&mut w, &root, item, &meta, opts)?;
        }
    }
    w.finish()
        .map_err(|e| ArchiveError::Malformed(format!("finalize zip: {e}")))?;
    Ok(())
}

fn zip_add_dir(
    w: &mut zip::ZipWriter<File>,
    root: &Path,
    name: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<(), ArchiveError> {
    w.add_directory(name, opts)
        .map_err(|e| ArchiveError::Malformed(format!("zip dir {name}: {e}")))?;
    for entry in walkdir::WalkDir::new(root).min_depth(1).follow_links(false) {
        let entry = entry.map_err(|e| ArchiveError::Malformed(format!("walk {name}: {e}")))?;
        let rel = entry
            .path()
            .strip_prefix(root)
            .expect("walkdir stays under its root");
        // Zip entry names are `/`-separated regardless of OS.
        let entry_name = format!(
            "{name}/{}",
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        );
        let meta = entry
            .metadata()
            .map_err(|e| ArchiveError::Malformed(format!("stat {entry_name}: {e}")))?;
        if entry.file_type().is_dir() {
            w.add_directory(&entry_name, opts)
                .map_err(|e| ArchiveError::Malformed(format!("zip dir {entry_name}: {e}")))?;
        } else {
            zip_add_leaf(w, entry.path(), &entry_name, &meta, opts)?;
        }
    }
    Ok(())
}

/// Add a single non-directory path (file or symlink) to the writer.
fn zip_add_leaf(
    w: &mut zip::ZipWriter<File>,
    path: &Path,
    entry_name: &str,
    meta: &std::fs::Metadata,
    opts: zip::write::SimpleFileOptions,
) -> Result<(), ArchiveError> {
    let opts = with_unix_mode(opts, meta);
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        w.add_symlink(entry_name, target.to_string_lossy(), opts)
            .map_err(|e| ArchiveError::Malformed(format!("zip symlink {entry_name}: {e}")))?;
        return Ok(());
    }
    w.start_file(entry_name, opts)
        .map_err(|e| ArchiveError::Malformed(format!("zip file {entry_name}: {e}")))?;
    let mut f = File::open(path)?;
    io::copy(&mut f, w)
        .map_err(|e| ArchiveError::Malformed(format!("zip file {entry_name}: {e}")))?;
    Ok(())
}

#[cfg(unix)]
fn with_unix_mode(
    opts: zip::write::SimpleFileOptions,
    meta: &std::fs::Metadata,
) -> zip::write::SimpleFileOptions {
    use std::os::unix::fs::PermissionsExt;
    opts.unix_permissions(meta.permissions().mode())
}

#[cfg(not(unix))]
fn with_unix_mode(
    opts: zip::write::SimpleFileOptions,
    _meta: &std::fs::Metadata,
) -> zip::write::SimpleFileOptions {
    opts
}

fn compress_tar_gz(cwd: &Path, items: &[String], out: &Path) -> Result<(), ArchiveError> {
    let file = File::create(out)?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(enc);
    // Preserve symlinks as symlinks instead of following them (a link
    // to a huge dir would otherwise balloon the archive, and a broken
    // one would fail it).
    builder.follow_symlinks(false);
    for item in items {
        let path = cwd.join(item);
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            builder
                .append_dir_all(item, &path)
                .map_err(|e| ArchiveError::Malformed(format!("tar dir {item}: {e}")))?;
        } else {
            builder
                .append_path_with_name(&path, item)
                .map_err(|e| ArchiveError::Malformed(format!("tar file {item}: {e}")))?;
        }
    }
    let enc = builder
        .into_inner()
        .map_err(|e| ArchiveError::Malformed(format!("finalize tar: {e}")))?;
    enc.finish()
        .map_err(|e| ArchiveError::Malformed(format!("finalize gzip: {e}")))?
        .sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::unique_name;

    /// Lay out a small tree: a file, a nested dir, a unicode name.
    fn make_tree(root: &Path) {
        std::fs::create_dir_all(root.join("proj/sub")).unwrap();
        std::fs::write(root.join("top.txt"), b"top level").unwrap();
        std::fs::write(root.join("proj/a.txt"), b"alpha").unwrap();
        std::fs::write(root.join("proj/sub/b.bin"), vec![42u8; 2048]).unwrap();
        std::fs::write(root.join("proj/acentuação.md"), "ação".as_bytes()).unwrap();
    }

    fn assert_tree(dest: &Path) {
        assert_eq!(std::fs::read(dest.join("proj/a.txt")).unwrap(), b"alpha");
        assert_eq!(
            std::fs::read(dest.join("proj/sub/b.bin")).unwrap(),
            vec![42u8; 2048]
        );
        assert_eq!(
            std::fs::read(dest.join("proj/acentuação.md")).unwrap(),
            "ação".as_bytes()
        );
    }

    #[test]
    fn zip_compress_extract_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let out = tmp.path().join("bundle.zip");
        compress(
            ArchiveKind::Zip,
            tmp.path(),
            &["proj".into(), "top.txt".into()],
            &out,
        )
        .unwrap();
        let dest = tmp.path().join("extracted");
        extract_archive(ArchiveKind::Zip, &out, &dest).unwrap();
        assert_tree(&dest);
        assert_eq!(std::fs::read(dest.join("top.txt")).unwrap(), b"top level");
    }

    #[test]
    fn targz_compress_extract_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let out = tmp.path().join("bundle.tar.gz");
        compress(ArchiveKind::TarGz, tmp.path(), &["proj".into()], &out).unwrap();
        let dest = tmp.path().join("extracted");
        extract_archive(ArchiveKind::TarGz, &out, &dest).unwrap();
        assert_tree(&dest);
    }

    #[cfg(unix)]
    #[test]
    fn zip_preserves_exec_bit_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        std::fs::write(tmp.path().join("d/run.sh"), b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            tmp.path().join("d/run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("run.sh", tmp.path().join("d/link")).unwrap();
        let out = tmp.path().join("d.zip");
        compress(ArchiveKind::Zip, tmp.path(), &["d".into()], &out).unwrap();
        let dest = tmp.path().join("x");
        extract_archive(ArchiveKind::Zip, &out, &dest).unwrap();
        let mode = std::fs::metadata(dest.join("d/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "exec bits survived");
        let link = std::fs::symlink_metadata(dest.join("d/link")).unwrap();
        assert!(link.file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(dest.join("d/link")).unwrap(),
            std::path::PathBuf::from("run.sh")
        );
    }

    #[test]
    fn zip_slip_is_refused() {
        // Craft a zip with a `../escape.txt` entry. The writer API
        // refuses such names, so assemble it from a legit archive by
        // patching the stored name (same length) in both the local
        // header and the central directory.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("xx.txt"), b"payload").unwrap();
        let out = tmp.path().join("evil.zip");
        compress(ArchiveKind::Zip, tmp.path(), &["xx.txt".into()], &out).unwrap();
        let mut bytes = std::fs::read(&out).unwrap();
        let needle = b"xx.txt";
        let evil = b"../a.t";
        assert_eq!(needle.len(), evil.len());
        let mut patched = 0;
        let mut i = 0;
        while i + needle.len() <= bytes.len() {
            if &bytes[i..i + needle.len()] == needle {
                bytes[i..i + needle.len()].copy_from_slice(evil);
                patched += 1;
            }
            i += 1;
        }
        assert!(patched >= 2, "expected name in local + central headers");
        std::fs::write(&out, &bytes).unwrap();
        let dest = tmp.path().join("dest");
        let err = extract_archive(ArchiveKind::Zip, &out, &dest).unwrap_err();
        assert!(matches!(err, ArchiveError::Unsupported(_)), "{err}");
        assert!(!tmp.path().join("a.t").exists(), "escape file must not exist");
    }

    #[cfg(unix)]
    #[test]
    fn zip_symlink_escape_is_refused() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        // A symlink entry pointing outside the extraction root, followed
        // by a file entry that would write THROUGH it. `enclosed_name`
        // clears both entry names; only the symlink-target check stops
        // the escape.
        let tmp = tempfile::tempdir().unwrap();
        let secret = tmp.path().join("secret");
        std::fs::create_dir(&secret).unwrap();
        let out = tmp.path().join("evil.zip");
        {
            let f = File::create(&out).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            // Absolute target: `File::create` through this link would
            // land in `secret/`, outside `dest`.
            zw.add_symlink("pwn", secret.to_str().unwrap(), SimpleFileOptions::default())
                .unwrap();
            zw.start_file("pwn/planted", SimpleFileOptions::default()).unwrap();
            zw.write_all(b"owned").unwrap();
            zw.finish().unwrap();
        }
        let dest = tmp.path().join("dest");
        let err = extract_archive(ArchiveKind::Zip, &out, &dest).unwrap_err();
        assert!(matches!(err, ArchiveError::Unsupported(_)), "{err}");
        assert!(
            !secret.join("planted").exists(),
            "file must not be written through the symlink"
        );
    }

    #[test]
    fn compress_rejects_path_like_items() {
        let tmp = tempfile::tempdir().unwrap();
        make_tree(tmp.path());
        let out = tmp.path().join("o.zip");
        for bad in ["../up", "a/b", "/abs", ".."] {
            assert!(
                compress(ArchiveKind::Zip, tmp.path(), &[bad.into()], &out).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn failed_compress_leaves_no_partial_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("ghost.zip");
        assert!(
            compress(ArchiveKind::Zip, tmp.path(), &["missing".into()], &out).is_err()
        );
        assert!(!out.exists());
    }

    #[test]
    fn unique_name_composes_with_listing() {
        let existing = ["proj.zip".to_string(), "proj-1.zip".to_string()];
        let name = unique_name("proj", ".zip", existing.iter().map(|s| s.as_str()));
        assert_eq!(name, "proj-2.zip");
    }
}

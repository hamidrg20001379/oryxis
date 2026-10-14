//! Full-vault snapshot encode/decode for the SFTP sync transport.
//!
//! P2P sync negotiates a delta over a live QUIC/relay session. The SFTP
//! transport has no peer to talk to, only a file: it treats that file as
//! a "virtual peer" and exchanges the whole vault state every round. The
//! reconciliation itself is identical to P2P, this module just reuses the
//! manifest bricks (`build_manifest` / `collect_records` / `apply_records`)
//! to turn the vault into one sealed blob and back.
//!
//! A round on a device is: download the remote blob, [`merge_snapshot`]
//! it into the local vault (LWW + tombstones, same as a `DeltaPush`),
//! then [`build_full_snapshot`] the now-merged vault and upload it. Each
//! device keeps its own local state and tombstones, so a lost upload race
//! self-heals on the next round.

use std::sync::Arc;
use std::sync::Mutex;

use oryxis_vault::VaultStore;

use crate::crypto;
use crate::engine::{apply_records, build_manifest, collect_records};
use crate::error::SyncError;
use crate::protocol::{DeltaRef, SyncRecord};

/// Header prefixing a sealed snapshot. A wrong-format or truncated file
/// then fails on the magic/version check instead of being fed to the
/// AEAD and surfacing as an opaque decrypt error.
///
/// v2 (protocol v6) moved the outer + per-record seal to
/// XChaCha20-Poly1305 (24-byte nonce). A v1 blob (ChaCha20, 12-byte
/// nonce, shipped in v0.8.3) is rejected here on the version byte
/// rather than fed to the wider-nonce cipher: the reject is
/// non-destructive (the caller refuses to push after a failed merge),
/// which matches the coordinated re-sync a v6 bump expects.
///
/// v3 (protocol v7) is a schema gate: the record payloads gained enum
/// variants a protocol-v6 client cannot deserialize (certificate auth,
/// sk- key algorithms). The crypto is identical to v2, so reads accept
/// BOTH versions (a v7 client understands every v2 payload) and writes
/// stamp v3; an old client rejects a v3 blob loudly at this header
/// instead of silently warn-skipping records it cannot parse.
///
/// v4 (protocol v8) is the same kind of schema gate, one level up:
/// `EntityType` itself gained a variant (`LoginScript`). A record whose
/// entity type an older build does not know is worse than an unreadable
/// payload, because the type is what the reader dispatches on, so the
/// whole record list fails to deserialize rather than one entry. Crypto
/// is again unchanged, so reads still accept v2 and v3 and writes stamp
/// v4.
///
/// v5 (0.20.0): `AuthMethod::SecurityKey`, a payload variant a v4 reader
/// cannot deserialize; since the record list is decoded whole, one such
/// host failed the entire merge on an older build. Reads accept v2 and
/// up, writes stamp v5.
///
/// v6: `Snippet.steps`, a field a v5 reader drops on its next save (see
/// `PROTOCOL_VERSION` v10). Reads accept v2 and up, writes stamp v6.
const SNAPSHOT_MAGIC: &[u8; 6] = b"ORXSNP";
const SNAPSHOT_VERSION: u16 = 6;
/// Oldest snapshot version this build still reads (same AEAD layout).
const SNAPSHOT_MIN_READ_VERSION: u16 = 2;
const HEADER_LEN: usize = SNAPSHOT_MAGIC.len() + 2;

/// Serialize the entire vault into one encrypted snapshot blob.
///
/// The manifest covers every live entity plus every tombstone, so the
/// snapshot carries deletions the same way a P2P delta does. Each
/// `SyncRecord` payload is already AEAD-sealed per entity by
/// [`collect_records`]; the outer seal here also covers the record list
/// itself so entity ids, types and timestamps don't sit in clear on the
/// remote host.
pub fn build_full_snapshot(
    vault: &Arc<Mutex<VaultStore>>,
    secret: &[u8; 32],
) -> Result<Vec<u8>, SyncError> {
    let manifest = build_manifest(vault)?;
    let needed: Vec<DeltaRef> = manifest
        .iter()
        .map(|e| DeltaRef {
            entity_type: e.entity_type,
            entity_id: e.entity_id,
        })
        .collect();
    let records = collect_records(vault, &needed, Some(secret))?;
    let json = serde_json::to_vec(&records)
        .map_err(|e| SyncError::Protocol(format!("snapshot encode: {e}")))?;
    let sealed = crypto::encrypt_payload(&json, secret)?;

    let mut out = Vec::with_capacity(HEADER_LEN + sealed.len());
    out.extend_from_slice(SNAPSHOT_MAGIC);
    out.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Decrypt a snapshot blob and merge its records into the local vault via
/// the same LWW path as an incoming `DeltaPush`. Returns the number of
/// records carried by the snapshot (not all are necessarily applied, the
/// defensive LWW in `apply_records` skips any record that isn't strictly
/// newer than the local copy).
///
/// A decrypt failure (wrong passphrase, corrupt file) returns an error
/// and leaves the vault untouched, so a caller must NOT push a fresh
/// snapshot after a failed merge or it would clobber good remote data
/// with a vault that never absorbed the remote state.
pub fn merge_snapshot(
    vault: &Arc<Mutex<VaultStore>>,
    blob: &[u8],
    secret: &[u8; 32],
) -> Result<usize, SyncError> {
    let body = parse_header(blob)?;
    let json = crypto::decrypt_payload(body, secret)?;
    let records: Vec<SyncRecord> = serde_json::from_slice(&json)
        .map_err(|e| SyncError::Protocol(format!("snapshot decode: {e}")))?;
    let count = records.len();
    apply_records(vault, &records, Some(secret))?;
    Ok(count)
}

/// Validate the snapshot header and return the sealed body that follows
/// it. A short or wrong-magic buffer is a hard error; an unknown version
/// is rejected rather than guessed at.
fn parse_header(blob: &[u8]) -> Result<&[u8], SyncError> {
    if blob.len() < HEADER_LEN {
        return Err(SyncError::Protocol("snapshot too short".into()));
    }
    if &blob[..SNAPSHOT_MAGIC.len()] != SNAPSHOT_MAGIC {
        return Err(SyncError::Protocol("snapshot bad magic".into()));
    }
    let version = u16::from_le_bytes([blob[6], blob[7]]);
    if !(SNAPSHOT_MIN_READ_VERSION..=SNAPSHOT_VERSION).contains(&version) {
        return Err(SyncError::Protocol(format!(
            "snapshot version {version} unsupported"
        )));
    }
    Ok(&blob[HEADER_LEN..])
}

/// A stable fingerprint of everything the vault would put in a
/// snapshot: entity ids, their kinds, their timestamps and their
/// tombstones.
///
/// This exists because a snapshot BLOB cannot be compared for equality.
/// The payload is sealed with a fresh nonce every time, so building the
/// same vault twice yields different bytes, and a transport that
/// commits "when the file changed" would commit on every round forever.
/// The manifest is the logical content, so hashing it answers the
/// question the bytes cannot: has anything actually changed?
///
/// It carries no secret and no plaintext: only a hash goes out, and it
/// is derived from ids and timestamps rather than from any field
/// value.
pub fn vault_signature(vault: &Arc<Mutex<VaultStore>>) -> Result<String, SyncError> {
    use sha2::{Digest, Sha256};
    let mut manifest = build_manifest(vault)?;
    // The manifest's order is whatever the queries returned; sort so
    // two devices holding identical data agree on the fingerprint.
    manifest.sort_by(|a, b| {
        (a.entity_type as u8, a.entity_id).cmp(&(b.entity_type as u8, b.entity_id))
    });
    let mut hasher = Sha256::new();
    for entry in &manifest {
        hasher.update([entry.entity_type as u8]);
        hasher.update(entry.entity_id.as_bytes());
        hasher.update(entry.updated_at.timestamp_millis().to_le_bytes());
        hasher.update([u8::from(entry.is_deleted)]);
    }
    // sha2 0.11 returns a generic `Array`, which has no `LowerHex`.
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>())
}

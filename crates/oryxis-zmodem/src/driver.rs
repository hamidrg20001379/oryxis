//! Async driver that runs one ZMODEM transfer to completion.
//!
//! `zmodem2` is a synchronous poll/submit state machine; this wraps it
//! in a tokio task that pumps its [`Action`]s against the pane's
//! transport (the `WriteWire` bytes go out through the session's input
//! sender, exactly where a keystroke would) and the local filesystem
//! (`WriteFile` for a download, `ReadFile` for an upload). Wire input
//! arrives on a channel the app fills with the bytes it diverts from
//! the terminal; progress and the terminal result flow back on another.
//!
//! Design constraints that shaped this:
//!
//! - **No blocking dialogs mid-flight.** ZMODEM peers time out, so the
//!   destination (a download folder) and the source (an upload file)
//!   are decided by the caller BEFORE the driver starts; the driver
//!   never stops to ask.
//! - **`submit_wire` may take only part of a chunk** (heapless internal
//!   buffer), so unconsumed wire is retained and re-submitted rather
//!   than dropped.
//! - **Abort is cooperative but must never wait on the peer**: a shared
//!   flag lets the app request cancellation, and an empty chunk on
//!   `wire_in` wakes a driver parked on a silent peer's `recv` so the
//!   flag is actually seen. On abort `zmodem2` queues the cancel
//!   sequence ahead of its `Aborted` event and the loop writes it like
//!   any other wire, so the remote tears down cleanly instead of waiting
//!   out its timeout, and gets the sequence once.
//! - **Completion is not the end of the wire.** `poll()` yields events
//!   before queued wire bytes, so `SessionCompleted` arrives while the
//!   final handshake (the receiver's ZFIN reply, the sender's "OO") is
//!   still queued. The loops keep pumping until `Idle` before
//!   returning; a download then also absorbs the peer's trailing "OO".
//!   Returning on the event alone left the remote `sz` retrying ZFIN
//!   against silence, holding its tty for ~20 s of timeouts (issue
//!   #77). A local error sends `CANCEL` for the same reason: the peer
//!   must never be left to wait out its own clock.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;
use zmodem2::{Action, Event, FileInfo, Position, Receiver, Sender};

use crate::Direction;

/// Floor between two streamed `Advanced` reports. Unthrottled, one
/// report per ~1 KiB subpacket turns a large transfer into hundreds of
/// thousands of UI messages; 100 ms keeps the overlay smooth while the
/// exact figure always lands with the final per-file report.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Buffered file I/O capacity. Every `tokio::fs` operation dispatches
/// to the blocking pool, so unbuffered per-subpacket I/O costs one
/// dispatch (plus syscall) per ~1 KiB; this amortizes it to ~4 per MiB.
const FILE_BUF_SIZE: usize = 256 * 1024;

/// How long a wait for peer bytes runs before the state machine's
/// `timeout()` is poked (re-queueing its handshake frame when one is
/// pending), and how many consecutive silent windows are tolerated
/// before the peer is declared gone (60 s total). SSH delivers
/// reliably, so this only fires when the remote lrzsz process died or
/// never really engaged; erroring out releases the diverted pane
/// instead of parking it until disconnect.
const RECV_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const RECV_SILENT_WINDOWS: u32 = 6;

/// The sender's streaming window: subpackets pushed before it waits for
/// a ZACK. Chosen by the caller per transport. On a reliable, possibly
/// high-latency link (SSH, or Telnet over TCP) a wide window amortises
/// the round trip and is what makes streaming fast. A byte-rate-limited
/// serial line is the opposite case: a wide window just piles up ~1 MiB
/// of in-flight data that takes minutes to drain, and the upload sits
/// silent waiting for the ZACK the whole time, tripping the watchdog on
/// a perfectly healthy transfer. Serial uses a small window instead; it
/// costs almost no throughput there because a serial round trip is
/// milliseconds, so the window barely needs to amortise anything.
pub const DEFAULT_STREAMING_WINDOW: usize = 1024;
pub const SERIAL_STREAMING_WINDOW: usize = 32;

/// Upload watchdog. A slow serial line legitimately goes silent while a
/// full streaming window drains, far longer than any fixed ceiling could
/// safely allow, so the deadline adapts: the peer is declared gone only
/// after `MARGIN` times the wall-clock the PREVIOUS window actually took.
/// `MIN` floors it so a fast link (SSH) keeps a prompt dead-peer
/// detection, and `BOOTSTRAP` covers the very first window, before any
/// duration has been measured.
const UPLOAD_WATCHDOG_MARGIN: u32 = 2;
const UPLOAD_WATCHDOG_MIN: std::time::Duration = std::time::Duration::from_secs(60);
const UPLOAD_WATCHDOG_BOOTSTRAP: std::time::Duration = std::time::Duration::from_secs(150);

/// Hard ceiling for a download. An announced ZFILE size is enforced
/// byte-for-byte below, but an unannounced one (`size == 0`) has no
/// sender-supplied bound, so a hostile host could stream forever and fill
/// the disk. `zmodem2`'s file position is a `u32` (the fork hands us the
/// ZFILE size as a `Position`, so `total` is already <= `u32::MAX`), and
/// this doubles as the floor the announced ceiling is clamped to, so even
/// a future fork that widened `Position` could not push the position past
/// a wrap. It is a protocol ceiling, not a product setting: a transfer
/// that legitimately needs >4 GiB cannot be expressed in this ZMODEM
/// implementation anyway.
const MAX_UNANNOUNCED_DOWNLOAD: u64 = u32::MAX as u64;

/// Suffix for in-flight downloads. The finished file only appears
/// under its real name once complete (FileDone renames it), and a
/// leftover part file after a crash, cancel or disconnect is the
/// anchor the next transfer of the same file resumes from.
pub const PART_SUFFIX: &str = ".oryxis-part";

/// Where the resume owner of `<name>.oryxis-part` is recorded, when the
/// caller asked for one (`resume_owner` on [`TransferSpec::Download`]):
/// `.<name>.oryxis-part`. The leading dot is what keeps it apart from
/// every download: [`sanitize_name`] strips leading dots, so no remote
/// name can ever land on it, and the shared suffix makes every sweep
/// that leaves part files alone leave this one alone too.
pub fn resume_owner_path(dir: &std::path::Path, name: &str) -> PathBuf {
    dir.join(format!(".{name}{PART_SUFFIX}"))
}

/// The recorded owner of a part file, as the text the sidecar holds:
/// the owner and the size the sender announced, one per line. A part
/// resumes only when BOTH match, so a stale part of the same name from
/// another source (another host, or the same host's different file) is
/// restarted instead of being continued into a mix of two files.
fn resume_owner_record(owner: &str, advertised: u64) -> String {
    format!("{owner}\n{advertised}\n")
}

/// Whether the sidecar beside a part says it belongs to `owner` and to a
/// file of `advertised` bytes. A sidecar that is missing, unreadable or
/// not a regular file (a symlink planted in the folder is never read
/// through) answers no.
async fn resume_owner_matches(sidecar: &std::path::Path, owner: &str, advertised: u64) -> bool {
    match tokio::fs::symlink_metadata(sidecar).await {
        Ok(m) if m.file_type().is_file() => {}
        _ => return false,
    }
    tokio::fs::read_to_string(sidecar)
        .await
        .is_ok_and(|text| text == resume_owner_record(owner, advertised))
}

/// The owner record belonging to a part file, whatever its (possibly
/// numbered) name: `<dir>/.<stem>.oryxis-part` for `<dir>/<stem>.oryxis-part`.
/// `None` for a path that is not a part.
pub fn resume_owner_path_for_part(part: &std::path::Path) -> Option<PathBuf> {
    let file = part.file_name()?.to_str()?;
    let stem = file.strip_suffix(PART_SUFFIX)?;
    Some(resume_owner_path(part.parent()?, stem))
}

/// A part file this transfer has claimed: open, positioned where the
/// next byte goes, and holding an EXCLUSIVE OS lock for as long as the
/// handle lives.
struct ClaimedPart {
    file: tokio::fs::File,
    path: PathBuf,
    resume_at: u64,
}

/// How [`claim_part`] found one candidate part name.
enum PartSlot {
    /// Ours: opened and locked, with its length when it already existed.
    Locked(std::fs::File, Option<u64>),
    /// Another live transfer holds it: never touched, try the next name.
    Held,
    /// Created by someone else between our look and our create: look again.
    Raced,
}

/// Open (or create) and lock the part for `name` in `dir`.
///
/// A part is never shared: two transfers of the same name at once (two
/// panes running `sz syslog`, or two hosts into the one staging folder)
/// used to truncate or append into each other's file, and both came out
/// corrupt without an error. The part is locked EXCLUSIVELY for the life
/// of its write handle, so a part somebody is writing is skipped and
/// this transfer takes the next numbered name (`name (N)`), which is
/// only the part's name: the file still lands as `name` (or its own
/// collision suffix) when it is placed.
///
/// An unlocked part is dead, and resumes (`rz -r`, length based) only
/// when it is no longer than the advertised size and, when the caller
/// named an owner, its owner record says it is the same file from the
/// same source; otherwise it restarts from zero under our lock.
async fn claim_part(
    dir: &std::path::Path,
    name: &str,
    advertised: u64,
    owner: Option<&str>,
) -> Result<ClaimedPart, String> {
    for n in 0..10_000u32 {
        let stem = if n == 0 {
            name.to_string()
        } else {
            numbered_name(name, n)
        };
        let path = dir.join(format!("{stem}{PART_SUFFIX}"));
        let (file, existing) = match open_and_lock_part(&path).await? {
            PartSlot::Locked(file, existing) => (file, existing),
            PartSlot::Held => continue,
            PartSlot::Raced => match open_and_lock_part(&path).await? {
                PartSlot::Locked(file, existing) => (file, existing),
                PartSlot::Held | PartSlot::Raced => continue,
            },
        };
        let sidecar = resume_owner_path(dir, &stem);
        let owned = match owner {
            None => true,
            Some(owner) => resume_owner_matches(&sidecar, owner, advertised).await,
        };
        let resume_at = match existing {
            Some(len) if owned && advertised > 0 && len > 0 && len <= advertised => len,
            _ => 0,
        };
        let file = tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
            use std::io::{Seek, SeekFrom};
            let mut file = file;
            if resume_at == 0 {
                // A dead part that does not qualify restarts, under
                // our lock, so nobody else can be reading it.
                file.set_len(0)?;
            }
            file.seek(SeekFrom::Start(resume_at))?;
            Ok(file)
        })
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?
        .map_err(|e| format!("open {}: {e}", path.display()))?;
        // A fresh part in a shared folder says whose it is, so the next
        // transfer of the same name from somewhere else restarts instead
        // of appending to it.
        if resume_at == 0
            && let Some(owner) = owner
        {
            write_resume_owner(&sidecar, owner, advertised).await?;
        }
        return Ok(ClaimedPart {
            file: tokio::fs::File::from_std(file),
            path,
            resume_at,
        });
    }
    Err(format!(
        "claim {}: too many parts of this name in use",
        dir.join(name).display()
    ))
}

/// Turn a finished part into the delivered file, WITHOUT ever letting
/// go of the part's lock while it still reads as a part.
///
/// The order is the point. Released first and renamed second, the part
/// would sit unlocked for a moment under its part name with its sidecar
/// still naming its source, and a second transfer of the same name from
/// the same source would claim it, see a matching sidecar and a length
/// equal to what it expects, and "resume" a file that is being delivered
/// under it. So, with the lock still held: the sidecar goes first (a part
/// with no matching sidecar never resumes, it restarts), then the part is
/// renamed to its final name, and only then is the handle closed.
///
/// Renaming a file we hold open is fine on unix. On Windows it works
/// because std opens files with `FILE_SHARE_DELETE` by default, and the
/// byte-range lock does not stop a rename. The part always lives in
/// `dest_dir` itself, so `place_file` renames within one directory and
/// never takes its cross-volume copy path, which would have to READ the
/// part through a second handle (refused under a Windows exclusive lock).
async fn finish_part(
    file: tokio::fs::File,
    part: &std::path::Path,
    dest_dir: &std::path::Path,
    name: &str,
    has_owner: bool,
) -> Result<PathBuf, String> {
    // `into_std` waits out any write still in flight on the tokio file,
    // so the bytes are all in the part before it is renamed.
    let held = file.into_std().await;
    if has_owner && let Some(sidecar) = resume_owner_path_for_part(part) {
        let _ = tokio::fs::remove_file(sidecar).await;
    }
    let placed = place_file(part, dest_dir, name).await;
    drop(held);
    placed
}

/// One attempt at the part `path`. `symlink_metadata` does NOT follow a
/// link, so a hostile pre-planted `<name>.oryxis-part` symlink (pointing
/// at ~/.bashrc, say) is seen for what it is and removed, never opened
/// THROUGH; only a regular file we find, or one we create with
/// `create_new` (O_EXCL), is ever written.
async fn open_and_lock_part(path: &std::path::Path) -> Result<PartSlot, String> {
    let existing = match tokio::fs::symlink_metadata(path).await {
        Ok(m) if m.file_type().is_file() => Some(m.len()),
        Ok(_) => {
            let _ = tokio::fs::remove_file(path).await;
            None
        }
        Err(_) => None,
    };
    let owned_path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<PartSlot, String> {
        let path = owned_path;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        if existing.is_none() {
            options.create_new(true);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(e) if existing.is_none() && e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(PartSlot::Raced);
            }
            // Gone between the look and the open: look again.
            Err(e) if existing.is_some() && e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PartSlot::Raced);
            }
            Err(e) => return Err(format!("open {}: {e}", path.display())),
        };
        match file.try_lock() {
            Ok(()) => {
                // The length is read under the lock: whatever wrote the
                // part before is finished with it.
                let len = file
                    .metadata()
                    .map_err(|e| format!("stat {}: {e}", path.display()))?
                    .len();
                Ok(PartSlot::Locked(file, existing.map(|_| len)))
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(PartSlot::Held),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(format!("lock {}: {e}", path.display()))
            }
        }
    })
    .await
    .map_err(|e| format!("open {}: {e}", path.display()))?
}

/// Record `owner` beside a freshly started part. Whatever sits at the
/// sidecar path is removed first (`remove_file` does not follow a
/// link) and the record is written with `create_new`, so a planted
/// symlink can never redirect the write.
async fn write_resume_owner(
    sidecar: &std::path::Path,
    owner: &str,
    advertised: u64,
) -> Result<(), String> {
    let _ = tokio::fs::remove_file(sidecar).await;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(sidecar)
        .await
        .map_err(|e| format!("create {}: {e}", sidecar.display()))?;
    file.write_all(resume_owner_record(owner, advertised).as_bytes())
        .await
        .map_err(|e| format!("write {}: {e}", sidecar.display()))?;
    file.flush()
        .await
        .map_err(|e| format!("flush {}: {e}", sidecar.display()))
}

/// The `CAN` byte (0x18, also ZDLE). A run of these on the wire is the
/// lrzsz cancel a remote `sz`/`rz` sends when the user Ctrl-C's it.
const CAN: u8 = 0x18;

/// How many consecutive raw `CAN` bytes count as a peer cancel. `zmodem2`
/// does not act on the cancel race (its framer only leaves Idle on ZPAD),
/// so we detect it ourselves before feeding the fork. A literal 0x18 in
/// file data is always ZDLE-escaped (`0x18 0x58`) and every header /
/// subpacket puts a non-0x18 type byte right after its single leading
/// ZDLE, so an unescaped run of 0x18 on the wire can only be a deliberate
/// cancel. lrzsz's own receiver fires at 5 (our own [`crate::CANCEL`]
/// sends 8), so 5 catches a real peer promptly without waiting out the
/// silent-window watchdog (~60 s).
const PEER_CANCEL_RUN: u8 = 5;

/// Scan a freshly received wire chunk for the lrzsz cancel sequence,
/// carrying the trailing `CAN` run in `run` so a cancel split across two
/// reads is still caught. Runs on RAW wire bytes only (before they reach
/// the fork's framer), which is what makes the unescaped-run test sound.
/// Returns `true` once the run reaches [`PEER_CANCEL_RUN`].
fn peer_cancelled(run: &mut u8, bytes: &[u8]) -> bool {
    for &b in bytes {
        if b == CAN {
            *run = run.saturating_add(1);
            if *run >= PEER_CANCEL_RUN {
                return true;
            }
        } else {
            *run = 0;
        }
    }
    false
}

/// Where a transfer's bytes come from / go to. Chosen by the caller up
/// front (see the no-blocking-dialogs note above).
#[derive(Debug, Clone)]
pub enum TransferSpec {
    /// Download: save each incoming file into this directory under its
    /// advertised (sanitized) name.
    ///
    /// `budget` bounds what the WHOLE session may write, across every
    /// file in it. The per-file ceiling below cannot do that job: it
    /// re-arms at each ZFILE, so a sender that loops
    /// [ZFILE -> ZDATA -> ZEOF] forever stays inside it forever while
    /// the disk fills. The caller computes the number (free space minus
    /// headroom, or a configured cap) because the policy belongs with
    /// the app; `None` means unbounded, which is what the upload side
    /// and the tests use.
    ///
    /// `resume_owner` ties a part file to where it came from. `None`
    /// keeps the plain `rz -r` rule (a part no larger than the announced
    /// size resumes), which is right for a folder the user picked for
    /// this host. `Some` is for a folder SHARED by every source (the
    /// app's staging folder): a part then resumes only when its sidecar
    /// ([`resume_owner_path`]) names the same owner and the same
    /// announced size, and a fresh part records it.
    Download {
        dest_dir: PathBuf,
        budget: Option<u64>,
        resume_owner: Option<String>,
    },
    /// Upload: send these local files, in order, in one session.
    /// `streaming_window` is the sender's in-flight subpacket window,
    /// picked by the caller per transport (see [`DEFAULT_STREAMING_WINDOW`]
    /// and [`SERIAL_STREAMING_WINDOW`]).
    Upload {
        sources: Vec<PathBuf>,
        streaming_window: usize,
    },
}

/// Progress and terminal outcome, streamed to the app.
#[derive(Debug, Clone)]
pub enum Progress {
    /// A file began; `size` is the advertised total when known.
    /// `batch` is `(k, n)` on a multi-file upload ("file k of n");
    /// `None` for single files and for downloads (a ZMODEM sender
    /// never announces how many files follow).
    Started {
        name: String,
        size: Option<u64>,
        batch: Option<(usize, usize)>,
        /// Where a DOWNLOAD is writing this file's bytes (the part file
        /// the driver claimed, which is not always `<name>.oryxis-part`:
        /// a part another live transfer holds is never shared, so this
        /// one may be numbered). `None` for uploads.
        part: Option<PathBuf>,
    },
    /// Cumulative bytes moved for the current file.
    Advanced { transferred: u64, total: Option<u64> },
    /// One file finished; `path` is where a download landed.
    FileDone { name: String, path: Option<PathBuf> },
    /// The whole session finished successfully. `trailing` carries any
    /// post-protocol bytes captured while confirming the peer's final
    /// "OO" (e.g. a fast shell prompt coalesced into the same read);
    /// they belong to the terminal, not the transfer.
    Completed { trailing: Vec<u8> },
    /// The session was cancelled (by us or the peer).
    Aborted,
    /// The transfer failed; the string is a human-readable reason.
    Error(String),
}

/// Handles the app passes in to drive one transfer.
pub struct TransferIo {
    /// Bytes diverted from the terminal (the detector's `wire` first,
    /// then every subsequent pane output batch). An EMPTY chunk is a
    /// pure wake-up carrying no wire bytes: the cancel path sends one
    /// so a driver blocked on a silent peer's `recv` notices the abort
    /// flag. Closing it (drop) is treated as a disconnect and aborts
    /// the transfer.
    pub wire_in: mpsc::UnboundedReceiver<Vec<u8>>,
    /// The pane transport's input sender: protocol replies to the peer.
    pub wire_out: mpsc::UnboundedSender<Vec<u8>>,
    /// Progress + outcome sink.
    pub progress: mpsc::UnboundedSender<Progress>,
    /// Set by the app to request cancellation.
    pub abort: Arc<AtomicBool>,
}

/// Run a transfer to completion. Consumes `first_wire` (the detector's
/// initial bytes) before awaiting more on `io.wire_in`. Always emits a
/// terminal `Progress` (`Completed` / `Aborted` / `Error`) before
/// returning, so the app can reliably leave divert mode.
pub async fn run(direction: Direction, spec: TransferSpec, first_wire: Vec<u8>, mut io: TransferIo) {
    let result = match direction {
        Direction::Download => run_download(spec, first_wire, &mut io).await,
        Direction::Upload => run_upload(spec, first_wire, &mut io).await,
    };
    let terminal = match result {
        Ok(Outcome::Completed(trailing)) => Progress::Completed { trailing },
        Ok(Outcome::Aborted) => Progress::Aborted,
        Err(e) => {
            // A local failure (disk, protocol) strands the peer
            // mid-session; without a cancel it holds the tty until its
            // own timeouts expire, the same freeze an unanswered ZFIN
            // causes. Release it now; if the wire is already gone the
            // send fails harmlessly.
            let _ = io.wire_out.send(crate::CANCEL.to_vec());
            Progress::Error(e)
        }
    };
    let _ = io.progress.send(terminal);
}

enum Outcome {
    /// Success; carries post-protocol bytes for the terminal (see
    /// [`Progress::Completed`]).
    Completed(Vec<u8>),
    Aborted,
}

/// One decoded step, copied out of the borrowed `Action` so the machine
/// can be mutated again in the same loop turn.
enum Step {
    WriteWire(Vec<u8>),
    WriteFile(Vec<u8>),
    ReadFile { offset: u32, max_len: usize },
    Started { name: Vec<u8>, size: Option<u64> },
    FileDone,
    Completed,
    Aborted,
    Idle,
}

fn decode(action: Action<'_>) -> Step {
    match action {
        Action::WriteWire(b) => Step::WriteWire(b.to_vec()),
        Action::WriteFile(b) => Step::WriteFile(b.to_vec()),
        Action::ReadFile { offset, max_len } => Step::ReadFile {
            offset: offset.get(),
            max_len,
        },
        Action::Event(Event::FileStarted(info)) => Step::Started {
            name: info.name.to_vec(),
            size: info.size.map(|p| u64::from(p.get())),
        },
        Action::Event(Event::FileCompleted) => Step::FileDone,
        Action::Event(Event::SessionCompleted) => Step::Completed,
        Action::Event(Event::Aborted) => Step::Aborted,
        Action::Idle => Step::Idle,
        // `Action` / `Event` are `#[non_exhaustive]`; treat anything new
        // as idle so a crate bump can't silently break the loop.
        _ => Step::Idle,
    }
}

async fn run_download(
    spec: TransferSpec,
    first_wire: Vec<u8>,
    io: &mut TransferIo,
) -> Result<Outcome, String> {
    let TransferSpec::Download {
        dest_dir,
        budget,
        resume_owner,
    } = spec
    else {
        return Err("download driver given a non-download spec".into());
    };
    // Advertise nonstop I/O (zero buffer length) plus CANOVIO: the pane
    // transport is SSH/Telnet/serial with its own flow control, and the
    // driver drains wire into the (buffered) file as fast as it arrives,
    // so there is no reason to make `sz` stop for a ZACK round trip
    // every buffer's worth of data; on links with real latency that
    // pacing, not bandwidth, dominates the transfer time.
    let mut receiver =
        Receiver::with_flow_control(0, true).map_err(|e| format!("zmodem init: {e:?}"))?;
    // Each announced file is answered manually so a leftover part file
    // can be resumed from its offset instead of always ZRPOS(0).
    receiver.set_manual_file_accept(true);
    let mut pending = first_wire;
    let mut dest: Option<BufWriter<tokio::fs::File>> = None;
    let mut dest_path: Option<PathBuf> = None;
    let mut name = String::new();
    let mut transferred: u64 = 0;
    // Bytes written across EVERY file of this session. `transferred`
    // resets at each ZFILE (it drives the per-file progress bar), which
    // is exactly why it cannot bound a batch.
    let mut session_written: u64 = 0;
    let mut total: Option<u64> = None;
    let mut aborting = false;
    let mut finished = false;
    // Consecutive RECV_TIMEOUT windows with no wire bytes; reset on any
    // data. Bounds how long a vanished peer can park the receive loop.
    let mut silent_windows: u32 = 0;
    // Trailing run of raw CAN bytes, carried across reads so a remote
    // `sz` that was Ctrl-C'd ends the transfer at once instead of parking
    // the diverted pane until the silent-window watchdog trips.
    let mut peer_cancel_run: u8 = 0;
    if peer_cancelled(&mut peer_cancel_run, &pending) {
        return Ok(Outcome::Aborted);
    }
    let mut last_progress = std::time::Instant::now();

    loop {
        if !aborting && io.abort.load(Ordering::Relaxed) {
            aborting = true;
            // abort() queues the cancel bytes the peer needs ahead of the
            // Aborted event, so the next poll writes them.
            let _ = receiver.abort();
        }
        match decode(receiver.poll()) {
            Step::WriteWire(bytes) => {
                let n = bytes.len();
                let _ = io.wire_out.send(bytes);
                receiver.wire_written(n);
            }
            Step::WriteFile(bytes) => {
                let n = bytes.len();
                // A correct sender writes exactly the size it announced in
                // ZFILE, so a stream that runs past it is a protocol
                // violation, not a valid transfer. The effective ceiling is
                // the announced size, or the protocol's u32 position limit
                // when nothing was announced (or, defensively, when a size
                // above that limit was) - so a hostile host can neither
                // overrun the disk nor push the fork's u32 file position
                // past a wrap, regardless of what it claims in ZFILE.
                let after = transferred.saturating_add(n as u64);
                let ceiling = match total {
                    Some(t) if t > 0 => t.min(MAX_UNANNOUNCED_DOWNLOAD),
                    _ => MAX_UNANNOUNCED_DOWNLOAD,
                };
                if after > ceiling {
                    return Err(format!(
                        "download exceeded its {ceiling}-byte ceiling; aborting"
                    ));
                }
                // Session budget, which the per-file ceiling above cannot
                // express: it resets at every ZFILE, so an endless batch
                // of in-spec files walks past it indefinitely. Checked
                // BEFORE the write, so the last chunk that would cross
                // the line never lands.
                let session_after = session_written.saturating_add(n as u64);
                if budget.is_some_and(|b| session_after > b) {
                    return Err(format!(
                        "download exceeded its {}-byte session budget; aborting",
                        budget.unwrap_or(0)
                    ));
                }
                // `dest == None` (data outside an open file) is absorbed,
                // not treated as fatal: it is reachable only between files
                // of a batch from a hostile or broken sender (the receiver
                // ignores data before the first accepted ZFILE), and the
                // ceiling above already bounds the absorb. Erroring here
                // would need multi-file loopback coverage first to prove a
                // healthy batch never trips it; `file_written` still runs
                // so the receiver's count stays in sync either way.
                if let Some(file) = dest.as_mut() {
                    file.write_all(&bytes)
                        .await
                        .map_err(|e| format!("write: {e}"))?;
                }
                transferred += n as u64;
                session_written = session_after;
                receiver
                    .file_written(n)
                    .map_err(|e| format!("zmodem file: {e:?}"))?;
                if last_progress.elapsed() >= PROGRESS_INTERVAL {
                    last_progress = std::time::Instant::now();
                    let _ = io.progress.send(Progress::Advanced { transferred, total });
                }
            }
            Step::ReadFile { .. } => {
                return Err("receiver asked to read a file (protocol error)".into());
            }
            Step::Started { name: raw, size } => {
                let safe = sanitize_name(&raw);
                // Refuse on the ANNOUNCED size before opening anything,
                // when the sender bothered to announce one: the write
                // guard below would catch it too, but only after the
                // file exists and most of the budget is already spent on
                // disk. An unannounced file cannot be pre-checked, which
                // is what the running guard is for.
                if let (Some(b), Some(announced)) = (budget, size)
                    && session_written.saturating_add(announced) > b
                {
                    return Err(format!(
                        "{safe} ({announced} bytes) does not fit in the remaining \
                         {} bytes of this session's budget; aborting",
                        b.saturating_sub(session_written)
                    ));
                }
                let advertised = size.unwrap_or(0);
                let claimed =
                    claim_part(&dest_dir, &safe, advertised, resume_owner.as_deref()).await?;
                let resume_at = claimed.resume_at;
                let resume_at32 = u32::try_from(resume_at)
                    .map_err(|_| "partial larger than 4 GiB (ZMODEM limit)".to_string())?;
                let part = claimed.path.clone();
                dest = Some(BufWriter::with_capacity(FILE_BUF_SIZE, claimed.file));
                name = safe;
                dest_path = Some(part);
                total = size;
                transferred = resume_at;
                // The acceptance answer (ZRPOS) can only be queued into
                // an empty outgoing buffer; flush whatever is pending
                // (e.g. the between-files ZACK volley) first.
                loop {
                    match decode(receiver.poll()) {
                        Step::WriteWire(bytes) => {
                            let n = bytes.len();
                            let _ = io.wire_out.send(bytes);
                            receiver.wire_written(n);
                        }
                        Step::Idle => break,
                        _ => return Err("unexpected step while accepting a file".into()),
                    }
                }
                receiver
                    .accept_file_at(resume_at32)
                    .map_err(|e| format!("zmodem accept: {e:?}"))?;
                let _ = io.progress.send(Progress::Started {
                    name: name.clone(),
                    size,
                    batch: None,
                    part: dest_path.clone(),
                });
                if resume_at > 0 {
                    // Let the overlay start at the resumed percentage
                    // instead of zero.
                    let _ = io.progress.send(Progress::Advanced { transferred, total });
                }
            }
            Step::FileDone => {
                let file = match dest.take() {
                    Some(mut file) => {
                        file.flush().await.map_err(|e| format!("flush: {e}"))?;
                        Some(file.into_inner())
                    }
                    None => None,
                };
                // The finished part surfaces under its real name only
                // now; a collision gets the browser-style " (N)".
                let part = dest_path.take();
                let final_path = match (&part, file) {
                    (Some(part), Some(file)) => Some(
                        finish_part(file, part, &dest_dir, &name, resume_owner.is_some()).await?,
                    ),
                    (Some(part), None) => Some(place_file(part, &dest_dir, &name).await?),
                    (None, _) => None,
                };
                if let Some(path) = final_path.as_ref()
                    && let Some(on_disk) = path.file_name()
                {
                    name = on_disk.to_string_lossy().into_owned();
                }
                // Streamed reports are time-strided; snap the overlay
                // to the exact figure before closing the file out.
                let _ = io.progress.send(Progress::Advanced { transferred, total });
                let _ = io.progress.send(Progress::FileDone {
                    name: name.clone(),
                    path: final_path,
                });
            }
            Step::Completed => {
                // The machine queued its ZFIN reply BEHIND this event
                // (`poll` yields events before wire bytes); returning
                // now would strand the reply and leave the remote `sz`
                // retrying ZFIN for ~20 s (issue #77). Keep pumping so
                // the reply drains, then leave through `Idle`.
                finished = true;
            }
            Step::Aborted => return Ok(Outcome::Aborted),
            Step::Idle => {
                if finished {
                    // Idle after the completion event: the ZFIN reply
                    // is on the wire. The peer answers it with "OO"
                    // and only then frees its tty; absorb that so it
                    // doesn't print as stray text at the next prompt.
                    let trailing =
                        swallow_over_and_out(&mut io.wire_in, std::mem::take(&mut pending)).await;
                    return Ok(Outcome::Completed(trailing));
                }
                if pending.is_empty() {
                    // Once the cancel went out, a torn-down peer may
                    // never speak again; exit instead of parking on
                    // wire that will never come. (Normally the Aborted
                    // event above ends the loop first; this is the
                    // backstop.)
                    if aborting {
                        return Ok(Outcome::Aborted);
                    }
                    match tokio::time::timeout(RECV_TIMEOUT, io.wire_in.recv()).await {
                        Ok(Some(bytes)) => {
                            silent_windows = 0;
                            if peer_cancelled(&mut peer_cancel_run, &bytes) {
                                return Ok(Outcome::Aborted);
                            }
                            pending = bytes;
                        }
                        Ok(None) => return Err("connection closed during transfer".into()),
                        Err(_) => {
                            silent_windows += 1;
                            if silent_windows >= RECV_SILENT_WINDOWS {
                                return Err("peer stopped responding".into());
                            }
                            // Re-queue the handshake volley if one is
                            // pending; the next loop turn flushes it.
                            receiver
                                .timeout()
                                .map_err(|e| format!("zmodem timeout: {e:?}"))?;
                            continue;
                        }
                    }
                }
                let consumed = receiver
                    .submit_wire(&pending)
                    .map_err(|e| format!("zmodem wire: {e:?}"))?;
                pending.drain(..consumed);
            }
        }
    }
}

/// After a download's ZFIN reply, the remote `sz` sends the two-byte
/// "OO" ("over and out") sign-off and exits. Those bytes are protocol,
/// not terminal output; wait briefly for them so they don't render.
/// Anything received beyond them (a fast shell prompt coalesced into
/// the same read) is returned for the app to hand to the emulator. A
/// peer that never signs off (killed, non-lrzsz) just runs the wait
/// out; nothing else can arrive in that window since the pane is still
/// diverted here.
async fn swallow_over_and_out(
    wire_in: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    mut pending: Vec<u8>,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(1000);
    let mut seen = 0u8;
    loop {
        let chunk = if pending.is_empty() {
            match tokio::time::timeout_at(deadline, wire_in.recv()).await {
                Ok(Some(chunk)) => chunk,
                // Timeout or disconnect: no sign-off is coming.
                _ => return Vec::new(),
            }
        } else {
            std::mem::take(&mut pending)
        };
        for (i, &b) in chunk.iter().enumerate() {
            match b {
                b'O' if seen < 2 => seen += 1,
                // Framing residue ahead of the "OO": flow control and
                // the ZHEX line terminator, whose CR / LF / XON may
                // arrive with the high bit set (lrzsz ends hex headers
                // with 0x0d 0x8a 0x11).
                0x11 | 0x13 | b'\r' | b'\n' | 0x8d | 0x8a | 0x91 if seen == 0 => {}
                // First byte past the sign-off (or something that is
                // not one): the terminal's from here on. A shell prompt
                // can trail in the same chunk (returned here) or in ones
                // already queued behind it (drained below).
                _ => return drain_buffered(wire_in, chunk[i..].to_vec()),
            }
        }
        if seen == 2 {
            // The "OO" landed exactly at a chunk boundary; a prompt the
            // peer sent right behind it may already be queued. Drain what
            // is buffered now (never blocks) so it reaches the terminal
            // instead of being dropped when the divert tears down. Bytes
            // that arrive later, after this returns, still ride the normal
            // post-transfer path.
            return drain_buffered(wire_in, Vec::new());
        }
    }
}

/// Append every wire chunk already queued in `wire_in` (non-blocking) to
/// `head`. Used at the end of the sign-off eater to sweep a shell prompt
/// that coalesced right behind the peer's "OO" before the pane's divert
/// is torn down, since those queued bytes would otherwise be dropped with
/// the transfer's receiver.
fn drain_buffered(wire_in: &mut mpsc::UnboundedReceiver<Vec<u8>>, mut head: Vec<u8>) -> Vec<u8> {
    while let Ok(chunk) = wire_in.try_recv() {
        head.extend_from_slice(&chunk);
    }
    head
}

/// One upload source: a buffered reader plus its bookkeeping. The
/// read-ahead buffer matters because the machine requests sequential
/// ~1 KiB slices; seeking per request would defeat it and pay two
/// blocking-pool dispatches per subpacket, so a real seek only happens
/// on a retransmission rewind or a receiver-driven resume offset.
struct UploadFile {
    reader: BufReader<tokio::fs::File>,
    pos: u64,
    size: u64,
    size32: u32,
    name: String,
}

impl UploadFile {
    async fn open(path: &std::path::Path) -> Result<Self, String> {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        let size = file.metadata().await.map_err(|e| format!("stat: {e}"))?.len();
        let size32 = u32::try_from(size)
            .map_err(|_| format!("{}: larger than 4 GiB (ZMODEM limit)", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        Ok(Self {
            reader: BufReader::with_capacity(FILE_BUF_SIZE, file),
            pos: 0,
            size,
            size32,
            name,
        })
    }
}

/// How long the upload may wait for the next peer reply before declaring
/// the peer gone, given how long the PREVIOUS window took to come back
/// (`None` before the first one). Adapts to the link so a slow serial
/// window is not mistaken for a dead peer, while a fast link keeps a
/// prompt detection via the floor. Pure so the policy is unit-tested
/// without driving a whole transfer.
fn upload_watchdog_allowance(window_took: Option<std::time::Duration>) -> std::time::Duration {
    window_took
        .map(|d| {
            d.saturating_mul(UPLOAD_WATCHDOG_MARGIN)
                .max(UPLOAD_WATCHDOG_MIN)
        })
        .unwrap_or(UPLOAD_WATCHDOG_BOOTSTRAP)
}

async fn run_upload(
    spec: TransferSpec,
    first_wire: Vec<u8>,
    io: &mut TransferIo,
) -> Result<Outcome, String> {
    let TransferSpec::Upload {
        sources,
        streaming_window,
    } = spec
    else {
        return Err("upload driver given a non-upload spec".into());
    };
    if sources.is_empty() {
        return Err("no files to send".into());
    }
    let total_files = sources.len();
    let mut queue = sources.into_iter().enumerate();
    // `(k, n)` shown by the overlay; suppressed for single files.
    let batch_of = move |index: usize| (total_files > 1).then_some((index + 1, total_files));

    let mut sender = Sender::new().map_err(|e| format!("zmodem init: {e:?}"))?;
    // When the remote `rz` allows streaming (lrzsz advertises a zero
    // buffer with CANOVIO), widen the default 10-subpacket window to the
    // caller's transport-appropriate size (subpackets between ZACK
    // waits). Not usize::MAX: the wire-out channel is unbounded with no
    // backpressure signal from the transport, so the periodic
    // acknowledgement is what bounds how much file data is in flight in
    // memory (and, on a serial line, keeps that in-flight window small
    // enough that the upload watchdog below never mistakes a slow drain
    // for a dead peer).
    sender.set_streaming_window(streaming_window);

    let (index, first) = queue.next().expect("sources checked non-empty");
    let mut current = UploadFile::open(&first).await?;
    sender
        .start_file(FileInfo::new(
            current.name.as_bytes(),
            Some(Position::new(current.size32)),
        ))
        .map_err(|e| format!("zmodem start_file: {e:?}"))?;
    // Requesting the session end when the LAST file starts makes the
    // machine queue ZFIN right after that file completes; earlier
    // files roll straight into the next ZFILE instead.
    if index + 1 == total_files {
        sender.finish().map_err(|e| format!("zmodem finish: {e:?}"))?;
    }
    let _ = io.progress.send(Progress::Started {
        name: current.name.clone(),
        size: Some(current.size),
        batch: batch_of(index),
        part: None,
    });

    let mut pending = first_wire;
    let mut aborting = false;
    let mut finished = false;
    let mut last_progress = std::time::Instant::now();
    // Adaptive upload watchdog state. `wait_started` is set when the
    // sender blocks for a peer reply and stays set (across however many
    // wire deliveries a fragmented reply arrives in) until the sender
    // actually RESUMES producing data; `window_took` is that measured
    // drain-plus-ack duration of the last completed window, which sizes
    // the deadline for the next wait. Keying off the sender's
    // block->resume transition rather than raw wire arrivals keeps a
    // fragmented ZACK from collapsing the measurement to milliseconds.
    let mut window_took: Option<std::time::Duration> = None;
    let mut wait_started: Option<std::time::Instant> = None;
    // Same peer-cancel watch as the download side: a remote `rz` that was
    // Ctrl-C'd sends the CAN run, which the fork ignores, so catch it here
    // and end the upload at once.
    let mut peer_cancel_run: u8 = 0;
    if peer_cancelled(&mut peer_cancel_run, &pending) {
        return Ok(Outcome::Aborted);
    }

    loop {
        if !aborting && io.abort.load(Ordering::Relaxed) {
            aborting = true;
            // Same as the download side: abort() queues the cancel bytes
            // ahead of the Aborted event.
            sender.abort();
        }
        match decode(sender.poll()) {
            Step::WriteWire(bytes) => {
                // First productive step after a wait: the sender resumed,
                // so the wait just ended. Record how long it took to size
                // the next window's deadline.
                if let Some(t) = wait_started.take() {
                    window_took = Some(t.elapsed());
                }
                let n = bytes.len();
                let _ = io.wire_out.send(bytes);
                sender.wire_written(n);
            }
            Step::ReadFile { offset, max_len } => {
                if let Some(t) = wait_started.take() {
                    window_took = Some(t.elapsed());
                }
                let offset = u64::from(offset);
                if offset != current.pos {
                    current
                        .reader
                        .seek(std::io::SeekFrom::Start(offset))
                        .await
                        .map_err(|e| format!("seek: {e}"))?;
                }
                let mut buf = vec![0u8; max_len];
                let read = current
                    .reader
                    .read(&mut buf)
                    .await
                    .map_err(|e| format!("read: {e}"))?;
                buf.truncate(read);
                if read == 0 {
                    return Err("unexpected end of file during upload".into());
                }
                current.pos = offset + read as u64;
                sender
                    .submit_file(&buf)
                    .map_err(|e| format!("zmodem submit: {e:?}"))?;
                if last_progress.elapsed() >= PROGRESS_INTERVAL {
                    last_progress = std::time::Instant::now();
                    let _ = io.progress.send(Progress::Advanced {
                        transferred: current.pos,
                        total: Some(current.size),
                    });
                }
            }
            Step::WriteFile(_) => {
                return Err("sender asked to write a file (protocol error)".into());
            }
            Step::Started { .. } => {}
            Step::FileDone => {
                // Same exact-figure snap as the download side.
                let _ = io.progress.send(Progress::Advanced {
                    transferred: current.size,
                    total: Some(current.size),
                });
                let _ = io.progress.send(Progress::FileDone {
                    name: current.name.clone(),
                    path: None,
                });
                if let Some((index, path)) = queue.next() {
                    current = UploadFile::open(&path).await?;
                    // start_file queues the next ZFILE and needs the
                    // outgoing buffer empty; flush leftovers first.
                    loop {
                        match decode(sender.poll()) {
                            Step::WriteWire(bytes) => {
                                let n = bytes.len();
                                let _ = io.wire_out.send(bytes);
                                sender.wire_written(n);
                            }
                            Step::Idle => break,
                            _ => return Err("unexpected step between files".into()),
                        }
                    }
                    sender
                        .start_file(FileInfo::new(
                            current.name.as_bytes(),
                            Some(Position::new(current.size32)),
                        ))
                        .map_err(|e| format!("zmodem start_file: {e:?}"))?;
                    if index + 1 == total_files {
                        sender.finish().map_err(|e| format!("zmodem finish: {e:?}"))?;
                    }
                    last_progress = std::time::Instant::now();
                    let _ = io.progress.send(Progress::Started {
                        name: current.name.clone(),
                        size: Some(current.size),
                        batch: batch_of(index),
                        part: None,
                    });
                }
            }
            Step::Completed => {
                // Mirror of the download side: the "OO" sign-off the
                // remote `rz` may be reading is queued behind this
                // event. Flush it before leaving.
                finished = true;
            }
            Step::Aborted => return Ok(Outcome::Aborted),
            Step::Idle => {
                if finished {
                    // Sign-off flushed; the receiver sends nothing
                    // after its ZFIN, so there is nothing to absorb.
                    return Ok(Outcome::Completed(Vec::new()));
                }
                if pending.is_empty() {
                    // Backstop, same as the download side: never park
                    // on a peer we just cancelled.
                    if aborting {
                        return Ok(Outcome::Aborted);
                    }
                    // Mark the start of the wait, but only on first entry:
                    // a reply that arrives fragmented re-enters this branch
                    // without the sender having resumed, and the clock must
                    // keep running from the original block so the measured
                    // window is the true drain time, not the gap between
                    // fragments.
                    if wait_started.is_none() {
                        wait_started = Some(std::time::Instant::now());
                    }
                    match tokio::time::timeout(RECV_TIMEOUT, io.wire_in.recv()).await {
                        Ok(Some(bytes)) => {
                            // Do not measure or clear `wait_started` here:
                            // these bytes may be only part of the reply. The
                            // window is timed when the sender actually
                            // resumes (the WriteWire/ReadFile arms above).
                            if peer_cancelled(&mut peer_cancel_run, &bytes) {
                                return Ok(Outcome::Aborted);
                            }
                            pending = bytes;
                        }
                        Ok(None) => return Err("connection closed during transfer".into()),
                        Err(_) => {
                            // A slow serial window legitimately stays silent
                            // while ~its worth of data drains, so allow up to
                            // MARGIN times what the previous window took
                            // (floored so a fast link keeps a prompt
                            // dead-peer detection, BOOTSTRAP for the first
                            // window before any measurement).
                            let allowance = upload_watchdog_allowance(window_took);
                            let waited = wait_started
                                .map(|t| t.elapsed())
                                .unwrap_or_default();
                            if waited >= allowance {
                                return Err("peer stopped responding".into());
                            }
                            // Re-queue the handshake volley if one is
                            // pending; the next loop turn flushes it.
                            sender
                                .timeout()
                                .map_err(|e| format!("zmodem timeout: {e:?}"))?;
                            continue;
                        }
                    }
                }
                let consumed = sender
                    .submit_wire(&pending)
                    .map_err(|e| format!("zmodem wire: {e:?}"))?;
                pending.drain(..consumed);
            }
        }
    }
}

/// Move a completed file into `dest_dir` under `name` without
/// clobbering anything already on disk: try the advertised name first,
/// then browser-style `name (N).ext` candidates. Each candidate is
/// reserved with `create_new` (an empty marker, so the existence check
/// and the claim are one atomic step) and the file atomically replaces
/// its own marker via rename, so a remote-controlled name can never
/// truncate an existing local file.
///
/// Two callers: the driver finalizing its own part file at `FileDone`,
/// and the app moving a finished download out of its staging folder
/// once the user has picked where it goes (receive first, ask
/// meanwhile). The second may cross a volume, where `rename` cannot
/// follow: the file is then copied beside its reserved name under a
/// part suffix and renamed over the marker, so the final name only
/// ever holds a complete file, and the source goes only once the
/// copy is in place.
pub async fn place_file(
    src: &std::path::Path,
    dest_dir: &std::path::Path,
    name: &str,
) -> Result<PathBuf, String> {
    // Hard bound so a pathological directory cannot spin forever;
    // hitting it reports honestly instead of overwriting.
    for attempt in 0..10_000u32 {
        let candidate = if attempt == 0 {
            name.to_string()
        } else {
            numbered_name(name, attempt)
        };
        let path = dest_dir.join(&candidate);
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(_) => {
                if let Err(e) = move_over_marker(src, &path, dest_dir, &candidate).await {
                    // The reservation must not outlive the failure: an
                    // empty file under the real name would read as the
                    // download, and block the next attempt's claim.
                    let _ = tokio::fs::remove_file(&path).await;
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create {}: {e}", path.display())),
        }
    }
    Err(format!(
        "finalize {}: too many name collisions",
        dest_dir.join(name).display()
    ))
}

/// Replace the reserved marker at `path` with `src`: a rename when the
/// two share a volume, a copy-then-rename when they do not.
async fn move_over_marker(
    src: &std::path::Path,
    path: &std::path::Path,
    dest_dir: &std::path::Path,
    candidate: &str,
) -> Result<(), String> {
    match tokio::fs::rename(src, path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            // The copy lands under a part name of its own, claimed
            // with `create_new` like everything else here, so a
            // half-copied file is never mistaken for a finished one.
            let tmp = dest_dir.join(format!(
                "{candidate}.{}{PART_SUFFIX}",
                std::process::id()
            ));
            let copied = async {
                let mut from = tokio::fs::File::open(src)
                    .await
                    .map_err(|e| format!("open {}: {e}", src.display()))?;
                let mut to = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp)
                    .await
                    .map_err(|e| format!("create {}: {e}", tmp.display()))?;
                tokio::io::copy(&mut from, &mut to)
                    .await
                    .map_err(|e| format!("copy {}: {e}", tmp.display()))?;
                to.flush().await.map_err(|e| format!("flush {}: {e}", tmp.display()))?;
                // On disk before the rename makes it the file: a crash
                // right after must not leave a complete name over bytes
                // the page cache still held.
                to.sync_all()
                    .await
                    .map_err(|e| format!("sync {}: {e}", tmp.display()))?;
                Ok::<(), String>(())
            }
            .await;
            if let Err(e) = copied {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
            tokio::fs::rename(&tmp, path)
                .await
                .map_err(|e| format!("rename {}: {e}", path.display()))?;
            // The copy is in place under the final name; only now may
            // the original go. A failure here leaves a duplicate, which
            // is the safe side of this error, so it is NOT an error:
            // reporting one would make the caller drop the reservation,
            // which by now IS the complete copy (and possibly the only
            // one, if something else already took the source).
            if let Err(e) = tokio::fs::remove_file(src).await {
                tracing::warn!(
                    "{} was copied to {} but could not be removed: {e}",
                    src.display(),
                    path.display()
                );
            }
            Ok(())
        }
        Err(e) => Err(format!("rename {}: {e}", path.display())),
    }
}

/// `report.pdf` + 2 -> `report (2).pdf`; an extensionless name gets the
/// counter at the end (`README (2)`).
fn numbered_name(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// Strip a sender-advertised name down to a safe basename: no directory
/// components, no `..`, no leading dots that could hide the file, and
/// no Windows reserved device names. A hostile or careless peer must
/// not be able to write outside the chosen download folder.
fn sanitize_name(raw: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(raw);
    let base = lossy
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| c == '.' || c.is_whitespace() || c.is_control());
    if base.is_empty() || base == ".." {
        return "received.bin".to_string();
    }
    // Neutralize Windows-reserved punctuation. A colon is the dangerous
    // one: `PathBuf::push`/`join` treats a `C:evil.exe` component as a
    // drive-relative path and REPLACES the whole download path, and
    // `name:stream` opens an NTFS alternate data stream. The rest
    // (`<>"|?*`) can never appear in a real Windows name; a peer sending
    // them is hostile or careless. Replace rather than reject so the
    // transfer still lands somewhere safe under the chosen folder.
    let base: String = base
        .chars()
        .map(|c| match c {
            ':' | '<' | '>' | '"' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect();
    // Windows matches device names on the part before the first dot
    // ("CON.txt" still opens the console). Defused on every platform so
    // a download folder that later syncs to a Windows machine stays
    // portable.
    let stem = base.split('.').next().unwrap_or(&base);
    if is_windows_reserved(stem) {
        format!("_{base}")
    } else {
        base
    }
}

/// Windows reserved device names: CON, PRN, AUX, NUL, COM1-9, LPT1-9
/// (case-insensitive; COM0/LPT0 and two-digit ports are NOT reserved).
fn is_windows_reserved(stem: &str) -> bool {
    let stem = stem.trim_end();
    if ["CON", "PRN", "AUX", "NUL"]
        .iter()
        .any(|r| stem.eq_ignore_ascii_case(r))
    {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4
        && (bytes[..3].eq_ignore_ascii_case(b"COM") || bytes[..3].eq_ignore_ascii_case(b"LPT"))
        && (b'1'..=b'9').contains(&bytes[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The upload watchdog adapts to the link instead of a fixed 60 s
    /// ceiling: a window that took longer than the old ceiling to come
    /// back grants proportionally more silence next time (the serial
    /// upload fix), while a fast link keeps a prompt floor and the first
    /// window gets the bootstrap grant.
    #[test]
    fn upload_watchdog_allowance_adapts_to_the_link() {
        use std::time::Duration;
        // No measurement yet (first window): the bootstrap grant.
        assert_eq!(
            upload_watchdog_allowance(None),
            UPLOAD_WATCHDOG_BOOTSTRAP
        );
        // A fast window stays at the floor, so a dead peer is still
        // caught promptly on a quick link.
        assert_eq!(
            upload_watchdog_allowance(Some(Duration::from_millis(200))),
            UPLOAD_WATCHDOG_MIN
        );
        // A slow serial window (100 s) that the old fixed 60 s ceiling
        // would have killed now grants MARGIN times its own duration.
        let slow = Duration::from_secs(100);
        assert_eq!(
            upload_watchdog_allowance(Some(slow)),
            slow * UPLOAD_WATCHDOG_MARGIN
        );
        assert!(
            upload_watchdog_allowance(Some(slow)) > Duration::from_secs(60),
            "a healthy slow window must outlast the old fixed ceiling"
        );
    }

    #[test]
    fn sanitize_strips_path_traversal_and_separators() {
        assert_eq!(sanitize_name(b"../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name(b"C:\\Windows\\System32\\evil.dll"), "evil.dll");
        assert_eq!(sanitize_name(b"report.pdf"), "report.pdf");
        assert_eq!(sanitize_name(b"/abs/log.txt"), "log.txt");
        assert_eq!(sanitize_name(b""), "received.bin");
        assert_eq!(sanitize_name(b"..."), "received.bin");
        assert_eq!(sanitize_name(b"/"), "received.bin");
    }

    #[test]
    fn sanitize_neutralizes_windows_drive_and_ads() {
        // `C:evil.exe` is a drive-relative component: on Windows
        // `dest.join(it)` would drop the download dir entirely. The
        // colon must not survive.
        assert_eq!(sanitize_name(b"C:evil.exe"), "C_evil.exe");
        assert_eq!(sanitize_name(b"report.pdf:hidden"), "report.pdf_hidden");
        assert_eq!(sanitize_name(b"a<b>c|d?e*f\"g"), "a_b_c_d_e_f_g");
        // The colon collapses into the stem, so `CON:x` becomes the
        // harmless `CON_x` (no longer the bare reserved device name).
        assert_eq!(sanitize_name(b"CON:x"), "CON_x");
    }

    #[test]
    fn sanitize_defuses_windows_reserved_device_names() {
        // Reserved with or without an extension, any case.
        assert_eq!(sanitize_name(b"CON"), "_CON");
        assert_eq!(sanitize_name(b"con.txt"), "_con.txt");
        assert_eq!(sanitize_name(b"NUL.tar.gz"), "_NUL.tar.gz");
        assert_eq!(sanitize_name(b"Prn"), "_Prn");
        assert_eq!(sanitize_name(b"aux.log"), "_aux.log");
        assert_eq!(sanitize_name(b"COM1"), "_COM1");
        assert_eq!(sanitize_name(b"lpt9.bin"), "_lpt9.bin");
        // Near misses stay untouched.
        assert_eq!(sanitize_name(b"COM0"), "COM0");
        assert_eq!(sanitize_name(b"COM10"), "COM10");
        assert_eq!(sanitize_name(b"CONSOLE.txt"), "CONSOLE.txt");
        assert_eq!(sanitize_name(b"conf.ig"), "conf.ig");
    }

    #[test]
    fn numbered_name_splits_on_the_last_extension() {
        assert_eq!(numbered_name("report.pdf", 1), "report (1).pdf");
        assert_eq!(numbered_name("archive.tar.gz", 3), "archive.tar (3).gz");
        assert_eq!(numbered_name("README", 2), "README (2)");
    }

    /// A remote-controlled file name must never truncate an existing
    /// local file: finalizing a completed part against a taken name
    /// lands on a browser-style " (N)" suffix, and the original bytes
    /// survive untouched.
    #[test]
    fn download_never_clobbers_an_existing_file() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("oryxis-zm-uniq-{}", std::process::id()));
            let _ = tokio::fs::remove_dir_all(&dir).await;
            tokio::fs::create_dir_all(&dir).await.unwrap();
            tokio::fs::write(dir.join("report.pdf"), b"original").await.unwrap();

            let part1 = dir.join(format!("report.pdf{PART_SUFFIX}"));
            tokio::fs::write(&part1, b"fresh download").await.unwrap();
            let p1 = place_file(&part1, &dir, "report.pdf").await.unwrap();
            assert_eq!(p1.file_name().unwrap().to_str().unwrap(), "report (1).pdf");
            assert_eq!(tokio::fs::read(&p1).await.unwrap(), b"fresh download");
            assert!(!tokio::fs::try_exists(&part1).await.unwrap(), "part left behind");
            // A second same-name download finalizes into its own file.
            let part2 = dir.join(format!("report.pdf{PART_SUFFIX}"));
            tokio::fs::write(&part2, b"second").await.unwrap();
            let p2 = place_file(&part2, &dir, "report.pdf").await.unwrap();
            assert_eq!(p2.file_name().unwrap().to_str().unwrap(), "report (2).pdf");
            // The pre-existing file was not truncated.
            let orig = tokio::fs::read(dir.join("report.pdf")).await.unwrap();
            assert_eq!(orig, b"original".to_vec());

            // A fresh name takes the advertised name directly.
            let part3 = dir.join(format!("clean.txt{PART_SUFFIX}"));
            tokio::fs::write(&part3, b"clean").await.unwrap();
            let p3 = place_file(&part3, &dir, "clean.txt").await.unwrap();
            assert_eq!(p3.file_name().unwrap().to_str().unwrap(), "clean.txt");

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// Placing a file across volumes cannot rename, so it copies beside
    /// the reserved name and renames over it: the final name only ever
    /// holds a complete file, the source is gone afterwards, and an
    /// existing file under that name is still not clobbered. Needs two
    /// filesystems to mean anything (`/dev/shm` against the temp dir on
    /// a typical Linux box); elsewhere it proves nothing and says so.
    #[test]
    fn place_file_crosses_volumes_by_copying() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let shm = std::path::Path::new("/dev/shm");
            if !shm.is_dir() {
                eprintln!("no /dev/shm here: cross-volume placement not exercised");
                return;
            }
            let src_dir = shm.join(format!("oryxis-zm-xdev-{}", std::process::id()));
            let dest = std::env::temp_dir().join(format!("oryxis-zm-xdev-dest-{}", std::process::id()));
            let _ = tokio::fs::remove_dir_all(&src_dir).await;
            let _ = tokio::fs::remove_dir_all(&dest).await;
            tokio::fs::create_dir_all(&src_dir).await.unwrap();
            tokio::fs::create_dir_all(&dest).await.unwrap();
            let probe_src = src_dir.join("probe");
            tokio::fs::write(&probe_src, b"p").await.unwrap();
            match tokio::fs::rename(&probe_src, dest.join("probe")).await {
                Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {}
                _ => {
                    eprintln!("/dev/shm and the temp dir share a volume: not exercised");
                    let _ = tokio::fs::remove_dir_all(&src_dir).await;
                    let _ = tokio::fs::remove_dir_all(&dest).await;
                    return;
                }
            }
            let src = src_dir.join("payload.bin");
            let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
            tokio::fs::write(&src, &body).await.unwrap();
            tokio::fs::write(dest.join("payload.bin"), b"original").await.unwrap();

            let landed = place_file(&src, &dest, "payload.bin").await.unwrap();
            assert_eq!(landed, dest.join("payload (1).bin"));
            assert_eq!(tokio::fs::read(&landed).await.unwrap(), body);
            assert_eq!(tokio::fs::read(dest.join("payload.bin")).await.unwrap(), b"original");
            assert!(!tokio::fs::try_exists(&src).await.unwrap(), "source left behind");
            // No copy scratch left beside the result.
            let mut entries = tokio::fs::read_dir(&dest).await.unwrap();
            let mut names = Vec::new();
            while let Some(e) = entries.next_entry().await.unwrap() {
                names.push(e.file_name().to_string_lossy().into_owned());
            }
            names.sort();
            assert_eq!(names, vec!["payload (1).bin".to_string(), "payload.bin".to_string()]);

            let _ = tokio::fs::remove_dir_all(&src_dir).await;
            let _ = tokio::fs::remove_dir_all(&dest).await;
        });
    }

    /// How many times the cancel sequence appears in what went out. The
    /// peer must get it once: a second run of CANs lands on the remote
    /// shell after `rz` / `sz` has exited, where Ctrl-X is a prefix key.
    fn cancel_runs(sent: &[u8]) -> usize {
        sent.windows(crate::CANCEL.len())
            .filter(|w| *w == crate::CANCEL)
            .count()
    }

    /// A cancel must take effect while the driver is parked on
    /// `wire_in.recv()` waiting for a silent peer: the flag plus the
    /// empty wake-up chunk end the transfer with `Aborted` after the
    /// CANCEL sequence went out, instead of hanging until disconnect.
    #[test]
    fn cancel_wakes_a_driver_blocked_on_a_silent_peer() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("oryxis-zm-abort-{}", std::process::id()));
            let _ = tokio::fs::create_dir_all(&dir).await;

            let (wire_tx, wire_in) = mpsc::unbounded_channel();
            let (wire_out_tx, mut wire_out_rx) = mpsc::unbounded_channel();
            let (p_tx, mut p_rx) = mpsc::unbounded_channel();
            let abort = Arc::new(AtomicBool::new(false));

            let task = tokio::spawn(run(
                Direction::Download,
                TransferSpec::Download {
                    budget: None,
                    dest_dir: dir.clone(),
                    resume_owner: None,
                },
                Vec::new(),
                TransferIo {
                    wire_in,
                    wire_out: wire_out_tx,
                    progress: p_tx,
                    abort: abort.clone(),
                },
            ));
            // Let the receiver flush its opening wire and park on recv
            // (the peer never answers).
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Cancel: raise the flag, then wake the parked recv with an
            // empty chunk, exactly what the app's ZmodemCancel does.
            abort.store(true, Ordering::Relaxed);
            wire_tx.send(Vec::new()).unwrap();

            tokio::time::timeout(std::time::Duration::from_secs(5), task)
                .await
                .expect("cancel did not wake the blocked driver")
                .unwrap();

            // The terminal progress is Aborted (this is what releases
            // the pane's divert in the app).
            let mut aborted = false;
            while let Ok(p) = p_rx.try_recv() {
                if matches!(p, Progress::Aborted) {
                    aborted = true;
                }
            }
            assert!(aborted, "driver ended without a terminal Aborted");
            // And the peer got the canonical cancel bytes so its `sz`
            // exits instead of waiting out its timeout.
            let mut sent = Vec::new();
            while let Ok(b) = wire_out_rx.try_recv() {
                sent.extend_from_slice(&b);
            }
            assert_eq!(cancel_runs(&sent), 1, "the cancel sequence must go out exactly once");

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// The upload side of the same cancel: the sender parks on recv
    /// waiting for the receiver's ZRINIT, and the flag plus the wake-up
    /// end it with `Aborted` and one cancel sequence on the wire.
    #[test]
    fn cancel_wakes_an_upload_blocked_on_a_silent_peer() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir =
                std::env::temp_dir().join(format!("oryxis-zm-upabort-{}", std::process::id()));
            let _ = tokio::fs::create_dir_all(&dir).await;
            let src = dir.join("payload.bin");
            tokio::fs::write(&src, vec![7u8; 4096]).await.unwrap();

            let (wire_tx, wire_in) = mpsc::unbounded_channel();
            let (wire_out_tx, mut wire_out_rx) = mpsc::unbounded_channel();
            let (p_tx, mut p_rx) = mpsc::unbounded_channel();
            let abort = Arc::new(AtomicBool::new(false));

            let task = tokio::spawn(run(
                Direction::Upload,
                TransferSpec::Upload {
                    sources: vec![src],
                    streaming_window: DEFAULT_STREAMING_WINDOW,
                },
                Vec::new(),
                TransferIo {
                    wire_in,
                    wire_out: wire_out_tx,
                    progress: p_tx,
                    abort: abort.clone(),
                },
            ));
            // Let the sender flush its ZRQINIT and park on recv.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            abort.store(true, Ordering::Relaxed);
            wire_tx.send(Vec::new()).unwrap();

            tokio::time::timeout(std::time::Duration::from_secs(5), task)
                .await
                .expect("cancel did not wake the blocked upload")
                .unwrap();

            let mut aborted = false;
            while let Ok(p) = p_rx.try_recv() {
                if matches!(p, Progress::Aborted) {
                    aborted = true;
                }
            }
            assert!(aborted, "upload ended without a terminal Aborted");
            let mut sent = Vec::new();
            while let Ok(b) = wire_out_rx.try_recv() {
                sent.extend_from_slice(&b);
            }
            assert_eq!(cancel_runs(&sent), 1, "the cancel sequence must go out exactly once");

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// A remote `sz` that the user Ctrl-C's sends the lrzsz CAN run; the
    /// fork's framer ignores it, so without our own detection the pane
    /// would sit through the full silent-window watchdog (~60 s) and end
    /// in `Error`. The wire-level `peer_cancelled` scan must instead end
    /// the transfer with a prompt `Aborted` and NOT echo a cancel back.
    #[test]
    fn a_peer_cancel_run_ends_the_download_at_once() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir =
                std::env::temp_dir().join(format!("oryxis-zm-peercan-{}", std::process::id()));
            let _ = tokio::fs::create_dir_all(&dir).await;

            let (wire_tx, wire_in) = mpsc::unbounded_channel();
            let (wire_out_tx, mut wire_out_rx) = mpsc::unbounded_channel();
            let (p_tx, mut p_rx) = mpsc::unbounded_channel();
            let abort = Arc::new(AtomicBool::new(false));

            let task = tokio::spawn(run(
                Direction::Download,
                TransferSpec::Download { budget: None, dest_dir: dir.clone(), resume_owner: None },
                Vec::new(),
                TransferIo {
                    wire_in,
                    wire_out: wire_out_tx,
                    progress: p_tx,
                    abort: abort.clone(),
                },
            ));
            // Let the receiver flush its opening wire and park on recv.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            // The remote `sz` is cancelled: it sends the canonical CAN
            // run. We never touch `abort` (this is the PEER cancelling,
            // not the user).
            wire_tx.send(crate::CANCEL.to_vec()).unwrap();

            // Well under RECV_TIMEOUT (10 s), let alone the 60 s watchdog:
            // if this times out, the wire scan failed to catch the cancel.
            tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .expect("peer cancel did not end the transfer promptly")
                .unwrap();

            let mut aborted = false;
            let mut errored = false;
            while let Ok(p) = p_rx.try_recv() {
                match p {
                    Progress::Aborted => aborted = true,
                    Progress::Error(_) => errored = true,
                    _ => {}
                }
            }
            assert!(aborted, "a peer cancel must end in Aborted");
            assert!(!errored, "a peer cancel must not surface as Error");
            // We do not echo a cancel: the peer already cancelled and is
            // exiting, so no CANCEL sequence should reach the wire.
            let mut sent = Vec::new();
            while let Ok(b) = wire_out_rx.try_recv() {
                sent.extend_from_slice(&b);
            }
            assert!(
                !sent.windows(crate::CANCEL.len()).any(|w| w == crate::CANCEL),
                "must not echo a cancel back at a peer that already cancelled"
            );

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// The upload side of the peer-cancel fix: a remote `rz` that the user
    /// Ctrl-C's sends the same CAN run. The sender parks on recv waiting
    /// for the receiver's ZRINIT, so without the wire scan it too would
    /// sit through the ~60 s watchdog. It rides the same `peer_cancelled`
    /// helper as the download, but timing is exactly where "symmetric
    /// code" and "tested" diverge, so it gets its own loopback.
    #[test]
    fn a_peer_cancel_run_ends_the_upload_at_once() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir =
                std::env::temp_dir().join(format!("oryxis-zm-upcan-{}", std::process::id()));
            let _ = tokio::fs::create_dir_all(&dir).await;
            let src = dir.join("payload.bin");
            tokio::fs::write(&src, vec![7u8; 4096]).await.unwrap();

            let (wire_tx, wire_in) = mpsc::unbounded_channel();
            let (wire_out_tx, mut wire_out_rx) = mpsc::unbounded_channel();
            let (p_tx, mut p_rx) = mpsc::unbounded_channel();
            let abort = Arc::new(AtomicBool::new(false));

            let task = tokio::spawn(run(
                Direction::Upload,
                TransferSpec::Upload {
                    sources: vec![src],
                    streaming_window: DEFAULT_STREAMING_WINDOW,
                },
                Vec::new(),
                TransferIo {
                    wire_in,
                    wire_out: wire_out_tx,
                    progress: p_tx,
                    abort: abort.clone(),
                },
            ));
            // The sender flushes ZFILE and parks waiting for the receiver
            // to answer; it never does. Then the remote `rz` is cancelled.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            wire_tx.send(crate::CANCEL.to_vec()).unwrap();

            tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .expect("peer cancel did not end the upload promptly")
                .unwrap();

            let mut aborted = false;
            let mut errored = false;
            while let Ok(p) = p_rx.try_recv() {
                match p {
                    Progress::Aborted => aborted = true,
                    Progress::Error(_) => errored = true,
                    _ => {}
                }
            }
            assert!(aborted, "a peer cancel must end the upload in Aborted");
            assert!(!errored, "a peer cancel must not surface as Error");
            let mut sent = Vec::new();
            while let Ok(b) = wire_out_rx.try_recv() {
                sent.extend_from_slice(&b);
            }
            assert!(
                !sent.windows(crate::CANCEL.len()).any(|w| w == crate::CANCEL),
                "must not echo a cancel back at a peer that already cancelled"
            );

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// The post-completion sign-off eater: consumes exactly the "OO"
    /// (plus flow-control padding before it), hands everything after
    /// it back for the terminal, and gives up quietly on a peer that
    /// never signs off.
    #[test]
    fn swallow_over_and_out_eats_the_sign_off_and_keeps_the_prompt() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // Sign-off alone, seeded via pending: nothing trails.
            let (_tx, mut rx) = mpsc::unbounded_channel();
            assert_eq!(swallow_over_and_out(&mut rx, b"OO".to_vec()).await, b"");

            // Padding before, prompt coalesced after: prompt survives.
            let (_tx, mut rx) = mpsc::unbounded_channel();
            let trailing = swallow_over_and_out(&mut rx, b"\r\nOOuser@host$ ".to_vec()).await;
            assert_eq!(trailing, b"user@host$ ");

            // Sign-off split across two wire chunks.
            let (tx, mut rx) = mpsc::unbounded_channel();
            tx.send(b"O".to_vec()).unwrap();
            tx.send(b"O$ ".to_vec()).unwrap();
            assert_eq!(swallow_over_and_out(&mut rx, Vec::new()).await, b"$ ");

            // "OO" ends the chunk exactly, with the prompt already queued
            // in the next: the drain sweeps it instead of dropping it when
            // the divert tears down (the post-OO byte-loss fix).
            let (tx, mut rx) = mpsc::unbounded_channel();
            tx.send(b"$ prompt".to_vec()).unwrap();
            assert_eq!(swallow_over_and_out(&mut rx, b"OO".to_vec()).await, b"$ prompt");

            // Same, but the trailing prompt spans several queued chunks.
            let (tx, mut rx) = mpsc::unbounded_channel();
            tx.send(b"$ ".to_vec()).unwrap();
            tx.send(b"more".to_vec()).unwrap();
            assert_eq!(swallow_over_and_out(&mut rx, b"OO".to_vec()).await, b"$ more");

            // No sign-off at all: whatever came belongs to the terminal.
            let (_tx, mut rx) = mpsc::unbounded_channel();
            let trailing = swallow_over_and_out(&mut rx, b"logout\r\n".to_vec()).await;
            assert_eq!(trailing, b"logout\r\n");

            // Silent peer: the wait runs out empty-handed (channel
            // closed, so this returns immediately, not after 1 s).
            let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
            drop(tx);
            assert_eq!(swallow_over_and_out(&mut rx, Vec::new()).await, b"");
        });
    }

    /// A peer that never says anything (remote lrzsz killed between
    /// the initiation header and the handshake) must not park the
    /// divert forever: the driver re-sends its handshake volley each
    /// silent window and gives up with a clear error after the bounded
    /// total, releasing the pane. Runs on tokio's paused clock, so the
    /// 60 s of windows cost no real time.
    #[test]
    fn silent_peer_is_bounded_by_handshake_retries() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("oryxis-zm-silent-{}", std::process::id()));
            let _ = tokio::fs::create_dir_all(&dir).await;

            // Keep wire_tx alive: the peer is silent, not disconnected.
            let (_wire_tx, wire_in) = mpsc::unbounded_channel::<Vec<u8>>();
            let (wire_out_tx, mut wire_out_rx) = mpsc::unbounded_channel();
            let (p_tx, mut p_rx) = mpsc::unbounded_channel();

            run(
                Direction::Download,
                TransferSpec::Download { budget: None, dest_dir: dir.clone(), resume_owner: None },
                Vec::new(),
                TransferIo {
                    wire_in,
                    wire_out: wire_out_tx,
                    progress: p_tx,
                    abort: Arc::new(AtomicBool::new(false)),
                },
            )
            .await;

            let mut error = None;
            while let Ok(p) = p_rx.try_recv() {
                if let Progress::Error(e) = p {
                    error = Some(e);
                }
            }
            let error = error.expect("driver ended without a terminal Error");
            assert!(
                error.contains("peer stopped responding"),
                "unexpected error: {error}"
            );
            // The handshake was actually retried on the wire, not just
            // waited out: initial ZRINIT plus one volley per silent
            // window (the last window errors out instead of poking).
            let mut volleys = 0;
            while wire_out_rx.try_recv().is_ok() {
                volleys += 1;
            }
            assert!(volleys >= 3, "expected retry volleys, got {volleys}");

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// The per-file ceiling re-arms at every ZFILE, so a sender that
    /// stays inside it on each file can still write forever across a
    /// batch. The session budget is what bounds the batch, and it must
    /// refuse a file whose ANNOUNCED size does not fit, before the file
    /// is opened, rather than after most of the budget is on disk.
    #[test]
    fn a_batch_cannot_outrun_the_session_budget() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir()
                .join(format!("oryxis-zm-budget-{}", std::process::id()));
            let _ = tokio::fs::remove_dir_all(&dir).await;
            let dest_dir = dir.join("incoming");
            tokio::fs::create_dir_all(&dest_dir).await.unwrap();
            // Two files, each far inside the per-file ceiling; together
            // they are past a budget sized for roughly one of them.
            let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
            let a = dir.join("a.bin");
            let b = dir.join("b.bin");
            tokio::fs::write(&a, &payload).await.unwrap();
            tokio::fs::write(&b, &payload).await.unwrap();

            let (up2down_tx, up2down_rx) = mpsc::unbounded_channel();
            let (down2up_tx, down2up_rx) = mpsc::unbounded_channel();
            let (p_up_tx, mut p_up_rx) = mpsc::unbounded_channel();
            let (p_down_tx, mut p_down_rx) = mpsc::unbounded_channel();
            let abort = Arc::new(AtomicBool::new(false));

            let up = tokio::spawn(run(
                Direction::Upload,
                TransferSpec::Upload {
                    sources: vec![a, b],
                    streaming_window: DEFAULT_STREAMING_WINDOW,
                },
                Vec::new(),
                TransferIo {
                    wire_in: down2up_rx,
                    wire_out: up2down_tx,
                    progress: p_up_tx,
                    abort: abort.clone(),
                },
            ));
            let down = tokio::spawn(run(
                Direction::Download,
                TransferSpec::Download {
                    // Room for the first file and a sliver, never the second.
                    budget: Some(payload.len() as u64 + 16),
                    dest_dir: dest_dir.clone(),
                    resume_owner: None,
                },
                Vec::new(),
                TransferIo {
                    wire_in: up2down_rx,
                    wire_out: down2up_tx,
                    progress: p_down_tx,
                    abort,
                },
            ));

            let both = async {
                let _ = up.await;
                let _ = down.await;
            };
            tokio::time::timeout(std::time::Duration::from_secs(20), both)
                .await
                .expect("transfer deadlocked");
            while p_up_rx.recv().await.is_some() {}

            let mut errored = false;
            let mut completed = false;
            while let Some(p) = p_down_rx.recv().await {
                match p {
                    Progress::Error(_) => errored = true,
                    Progress::Completed { .. } => completed = true,
                    _ => {}
                }
            }
            assert!(
                errored && !completed,
                "the batch must be refused once it crosses the session budget"
            );
            // The first file landed; the second never became a file at
            // all, because the announced size was checked before the open.
            let mut names: Vec<String> = Vec::new();
            let mut rd = tokio::fs::read_dir(&dest_dir).await.unwrap();
            while let Ok(Some(e)) = rd.next_entry().await {
                names.push(e.file_name().to_string_lossy().into_owned());
            }
            assert!(
                !names.iter().any(|n| n.starts_with("b.bin")),
                "the over-budget file must never be created, got {names:?}"
            );
            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }


    /// End-to-end oracle: drive our upload driver and our download
    /// driver against each other over crossed channels (each plays the
    /// role lrzsz would on the wire) and assert the file round-trips.
    /// This validates the whole poll/submit loop, the finish() timing,
    /// the ReadFile/WriteFile handling and partial-wire retry, none of
    /// which the crate's own one-sided tests cover.
    #[test]
    fn loopback_upload_to_download_round_trips_a_file() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("oryxis-zm-{}", std::process::id()));
            let _ = tokio::fs::create_dir_all(&dir).await;
            let src = dir.join("payload.bin");
            let dest_dir = dir.join("incoming");
            let _ = tokio::fs::create_dir_all(&dest_dir).await;
            // A payload big enough to span several subpackets and force
            // multiple ReadFile/WriteFile round trips, with control-ish
            // bytes (0x11 XON, 0x18 ZDLE, 0x0d CR) that ZMODEM escapes.
            let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
            tokio::fs::write(&src, &payload).await.unwrap();

            let (up2down_tx, up2down_rx) = mpsc::unbounded_channel();
            let (down2up_tx, down2up_rx) = mpsc::unbounded_channel();
            let (p_up_tx, mut p_up_rx) = mpsc::unbounded_channel();
            let (p_down_tx, mut p_down_rx) = mpsc::unbounded_channel();
            let abort = Arc::new(AtomicBool::new(false));

            let up = tokio::spawn(run(
                Direction::Upload,
                TransferSpec::Upload {
                    sources: vec![src.clone()],
                    streaming_window: DEFAULT_STREAMING_WINDOW,
                },
                Vec::new(),
                TransferIo {
                    wire_in: down2up_rx,
                    wire_out: up2down_tx,
                    progress: p_up_tx,
                    abort: abort.clone(),
                },
            ));
            let down = tokio::spawn(run(
                Direction::Download,
                TransferSpec::Download {
                    budget: None,
                    dest_dir: dest_dir.clone(),
                    resume_owner: None,
                },
                Vec::new(),
                TransferIo {
                    wire_in: up2down_rx,
                    wire_out: down2up_tx,
                    progress: p_down_tx,
                    abort,
                },
            ));

            let both = async {
                up.await.unwrap();
                down.await.unwrap();
            };
            tokio::time::timeout(std::time::Duration::from_secs(20), both)
                .await
                .expect("transfer deadlocked");

            // The download side must have reported Completed, and the
            // last Advanced must be the exact total: streamed reports
            // are time-throttled, so the per-file final snap is what
            // guarantees the overlay never ends short.
            let mut completed = false;
            let mut saved: Option<PathBuf> = None;
            let mut last_advanced = None;
            while let Ok(p) = p_down_rx.try_recv() {
                match p {
                    Progress::Completed { trailing } => {
                        completed = true;
                        assert!(
                            trailing.is_empty(),
                            "loopback produced trailing bytes: {trailing:?}"
                        );
                    }
                    Progress::Advanced { transferred, .. } => last_advanced = Some(transferred),
                    Progress::FileDone { path, .. } => saved = path,
                    Progress::Error(e) => panic!("download error: {e}"),
                    _ => {}
                }
            }
            assert!(completed, "download never completed");
            assert_eq!(
                last_advanced,
                Some(payload.len() as u64),
                "final progress snap is not the exact total"
            );
            // The upload side must complete too: it only can if the
            // download flushed its queued ZFIN reply before exiting
            // (issue #77's regression; the old driver returned on the
            // completion event and left the reply stranded, so this
            // side ended in "connection closed" instead).
            let mut up_completed = false;
            while let Ok(p) = p_up_rx.try_recv() {
                match p {
                    Progress::Completed { .. } => up_completed = true,
                    Progress::Error(e) => panic!("upload error: {e}"),
                    _ => {}
                }
            }
            assert!(up_completed, "upload never completed (ZFIN reply not flushed?)");
            let saved = saved.expect("no saved path reported");
            let got = tokio::fs::read(&saved).await.unwrap();
            assert_eq!(got, payload, "round-tripped bytes differ");

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// Drive one loopback upload of `src` into `dest_dir` and return
    /// the path the download reported for it.
    async fn loopback_download(src: &std::path::Path, dest_dir: &std::path::Path, owner: Option<&str>) -> PathBuf {
        let (up2down_tx, up2down_rx) = mpsc::unbounded_channel();
        let (down2up_tx, down2up_rx) = mpsc::unbounded_channel();
        let (p_up_tx, _p_up_rx) = mpsc::unbounded_channel();
        let (p_down_tx, mut p_down_rx) = mpsc::unbounded_channel();
        let abort = Arc::new(AtomicBool::new(false));
        let up = tokio::spawn(run(
            Direction::Upload,
            TransferSpec::Upload {
                sources: vec![src.to_path_buf()],
                streaming_window: DEFAULT_STREAMING_WINDOW,
            },
            Vec::new(),
            TransferIo {
                wire_in: down2up_rx,
                wire_out: up2down_tx,
                progress: p_up_tx,
                abort: abort.clone(),
            },
        ));
        let down = tokio::spawn(run(
            Direction::Download,
            TransferSpec::Download {
                budget: None,
                dest_dir: dest_dir.to_path_buf(),
                resume_owner: owner.map(str::to_string),
            },
            Vec::new(),
            TransferIo {
                wire_in: up2down_rx,
                wire_out: down2up_tx,
                progress: p_down_tx,
                abort,
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            up.await.unwrap();
            down.await.unwrap();
        })
        .await
        .expect("transfer deadlocked");
        let mut saved = None;
        while let Ok(p) = p_down_rx.try_recv() {
            match p {
                Progress::FileDone { path, .. } => saved = path,
                Progress::Error(e) => panic!("download error: {e}"),
                _ => {}
            }
        }
        saved.expect("no saved path reported")
    }

    /// In a folder every source shares, a leftover part resumes only for
    /// the owner its sidecar names: another owner's part (or one with no
    /// record at all) restarts, and never ends up as the head of a file
    /// the current sender's tail was appended to.
    #[test]
    fn a_shared_folder_part_resumes_only_for_its_owner() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("oryxis-zm-owner-{}", std::process::id()));
            let _ = tokio::fs::remove_dir_all(&dir).await;
            let dest = dir.join("staging");
            tokio::fs::create_dir_all(&dest).await.unwrap();
            let src = dir.join("payload.bin");
            let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
            tokio::fs::write(&src, &payload).await.unwrap();
            let part = dest.join(format!("payload.bin{PART_SUFFIX}"));
            let sidecar = resume_owner_path(&dest, "payload.bin");
            // A head that is NOT the payload's: resuming from it would
            // show up in the result.
            let foreign_head = vec![0xEEu8; 1000];

            // Another owner's part: restarted.
            tokio::fs::write(&part, &foreign_head).await.unwrap();
            tokio::fs::write(&sidecar, resume_owner_record("host-a", 8192)).await.unwrap();
            let saved = loopback_download(&src, &dest, Some("host-b")).await;
            assert_eq!(tokio::fs::read(&saved).await.unwrap(), payload);
            assert!(!tokio::fs::try_exists(&sidecar).await.unwrap(), "record outlived its part");
            tokio::fs::remove_file(&saved).await.unwrap();

            // A part with no record at all: restarted too.
            tokio::fs::write(&part, &foreign_head).await.unwrap();
            let saved = loopback_download(&src, &dest, Some("host-b")).await;
            assert_eq!(tokio::fs::read(&saved).await.unwrap(), payload);
            tokio::fs::remove_file(&saved).await.unwrap();

            // Its own part: resumed from where it stopped (the planted
            // head is kept, which is how the test sees the resume).
            tokio::fs::write(&part, &foreign_head).await.unwrap();
            tokio::fs::write(&sidecar, resume_owner_record("host-b", 8192)).await.unwrap();
            let saved = loopback_download(&src, &dest, Some("host-b")).await;
            let mut expected = foreign_head.clone();
            expected.extend_from_slice(&payload[1000..]);
            assert_eq!(tokio::fs::read(&saved).await.unwrap(), expected);
            assert!(!tokio::fs::try_exists(&sidecar).await.unwrap());

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// Two transfers of the same name at once never share a part: while
    /// one holds `syslog.oryxis-part`, the other writes a numbered part
    /// of its own and the first one's bytes and owner record survive
    /// untouched. Once the first lets go, its dead part resumes for its
    /// own owner.
    #[test]
    fn a_part_another_transfer_holds_is_never_touched() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("oryxis-zm-held-{}", std::process::id()));
            let _ = tokio::fs::remove_dir_all(&dir).await;
            tokio::fs::create_dir_all(&dir).await.unwrap();

            let mut first = claim_part(&dir, "syslog", 100, Some("host-a")).await.unwrap();
            assert_eq!(first.path, dir.join(format!("syslog{PART_SUFFIX}")));
            assert_eq!(first.resume_at, 0);
            first.file.write_all(&[0xAA; 40]).await.unwrap();
            first.file.flush().await.unwrap();

            // Same name, same owner, while the first is live: a part of
            // its own, never a resume into (or a truncate of) the first.
            let second = claim_part(&dir, "syslog", 100, Some("host-a")).await.unwrap();
            assert_eq!(second.path, dir.join(format!("syslog (1){PART_SUFFIX}")));
            assert_eq!(second.resume_at, 0);
            // Another source likewise.
            let third = claim_part(&dir, "syslog", 100, Some("host-b")).await.unwrap();
            assert_eq!(third.path, dir.join(format!("syslog (2){PART_SUFFIX}")));
            assert_eq!(tokio::fs::read(&first.path).await.unwrap(), vec![0xAA; 40]);
            assert!(resume_owner_matches(&resume_owner_path(&dir, "syslog"), "host-a", 100).await);
            assert!(
                resume_owner_matches(&resume_owner_path(&dir, "syslog (2)"), "host-b", 100).await
            );
            assert_eq!(
                resume_owner_path_for_part(&third.path),
                Some(resume_owner_path(&dir, "syslog (2)"))
            );

            // The first transfer dies: its part is dead now, and resumes
            // for its own owner from where it stopped.
            drop(first);
            drop(second);
            drop(third);
            let again = claim_part(&dir, "syslog", 100, Some("host-a")).await.unwrap();
            assert_eq!(again.path, dir.join(format!("syslog{PART_SUFFIX}")));
            assert_eq!(again.resume_at, 40);

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// A finished part is delivered with its lock still held and its
    /// sidecar already gone, so nothing can mistake it for a resumable
    /// part on the way: afterwards the final file carries the bytes, no
    /// part or record is left, and the next transfer of the same name
    /// from the same source starts from zero.
    #[test]
    fn a_finished_part_is_delivered_before_its_lock_is_released() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("oryxis-zm-finish-{}", std::process::id()));
            let _ = tokio::fs::remove_dir_all(&dir).await;
            tokio::fs::create_dir_all(&dir).await.unwrap();

            let mut part = claim_part(&dir, "syslog", 40, Some("host-a")).await.unwrap();
            part.file.write_all(&[0xBB; 40]).await.unwrap();
            part.file.flush().await.unwrap();
            let sidecar = resume_owner_path_for_part(&part.path).unwrap();
            assert!(tokio::fs::symlink_metadata(&sidecar).await.is_ok());

            let placed = finish_part(part.file, &part.path, &dir, "syslog", true)
                .await
                .unwrap();
            assert_eq!(placed, dir.join("syslog"));
            assert_eq!(tokio::fs::read(&placed).await.unwrap(), vec![0xBB; 40]);
            assert!(tokio::fs::symlink_metadata(&part.path).await.is_err());
            assert!(tokio::fs::symlink_metadata(&sidecar).await.is_err());

            let next = claim_part(&dir, "syslog", 40, Some("host-a")).await.unwrap();
            assert_eq!(next.path, dir.join(format!("syslog{PART_SUFFIX}")));
            assert_eq!(next.resume_at, 0);

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// A fresh part in a shared folder records its owner while it is in
    /// flight, and a planted symlink at the record's path is replaced,
    /// never written through.
    #[cfg(unix)]
    #[test]
    fn the_owner_record_never_writes_through_a_symlink() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("oryxis-zm-owner-link-{}", std::process::id()));
            let _ = tokio::fs::remove_dir_all(&dir).await;
            tokio::fs::create_dir_all(&dir).await.unwrap();
            let victim = dir.join("victim");
            tokio::fs::write(&victim, b"keep").await.unwrap();
            let sidecar = resume_owner_path(&dir, "x.bin");
            std::os::unix::fs::symlink(&victim, &sidecar).unwrap();
            assert!(!resume_owner_matches(&sidecar, "keep", 0).await);
            write_resume_owner(&sidecar, "host", 5).await.unwrap();
            assert_eq!(tokio::fs::read(&victim).await.unwrap(), b"keep");
            assert!(resume_owner_matches(&sidecar, "host", 5).await);
            assert!(!resume_owner_matches(&sidecar, "host", 6).await);
            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }

    /// Crossing volumes, a source that cannot be removed once the copy
    /// is in place leaves a duplicate, and the complete copy is KEPT:
    /// the placement reports success rather than dropping its own
    /// result. Only runs where `/dev/shm` is another volume and a
    /// read-only folder actually refuses the removal (not as root).
    #[cfg(unix)]
    #[test]
    fn a_source_that_cannot_be_removed_keeps_the_copy() {
        use std::os::unix::fs::PermissionsExt;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let shm = std::path::Path::new("/dev/shm");
            if !shm.is_dir() {
                eprintln!("no /dev/shm here: not exercised");
                return;
            }
            let src_dir = shm.join(format!("oryxis-zm-norm-{}", std::process::id()));
            let dest = std::env::temp_dir().join(format!("oryxis-zm-norm-dest-{}", std::process::id()));
            let _ = tokio::fs::remove_dir_all(&src_dir).await;
            let _ = tokio::fs::remove_dir_all(&dest).await;
            tokio::fs::create_dir_all(&src_dir).await.unwrap();
            tokio::fs::create_dir_all(&dest).await.unwrap();
            let src = src_dir.join("payload.bin");
            tokio::fs::write(&src, b"complete").await.unwrap();
            let cleanup = |src_dir: PathBuf, dest: PathBuf| async move {
                let _ = std::fs::set_permissions(&src_dir, std::fs::Permissions::from_mode(0o755));
                let _ = tokio::fs::remove_dir_all(&src_dir).await;
                let _ = tokio::fs::remove_dir_all(&dest).await;
            };
            // Preconditions: another volume, and a removal the folder
            // mode really refuses.
            let probe = src_dir.join("probe");
            tokio::fs::write(&probe, b"p").await.unwrap();
            let crosses = matches!(
                tokio::fs::rename(&probe, dest.join("probe")).await,
                Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices
            );
            std::fs::set_permissions(&src_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            let refused = tokio::fs::remove_file(&probe).await.is_err();
            if !crosses || !refused {
                eprintln!("preconditions not met (same volume, or running as root): not exercised");
                cleanup(src_dir, dest).await;
                return;
            }
            let landed = place_file(&src, &dest, "payload.bin").await.unwrap();
            assert_eq!(landed, dest.join("payload.bin"));
            assert_eq!(tokio::fs::read(&landed).await.unwrap(), b"complete");
            assert!(tokio::fs::try_exists(&src).await.unwrap(), "the duplicate stays");
            cleanup(src_dir, dest).await;
        });
    }

    /// The app may hold a folder dialog open between detecting an `sz`
    /// and spawning this driver, and the pane's divert is live for the
    /// whole wait, so every ZRQINIT the sender retransmits meanwhile is
    /// queued on `wire_in` ahead of the real conversation. The receiver
    /// must treat that run of duplicate openers as one opener: answer,
    /// then carry the transfer to completion on the bytes that follow.
    #[test]
    fn a_run_of_queued_openers_does_not_derail_the_download() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = std::env::temp_dir().join(format!("oryxis-zm-dup-{}", std::process::id()));
            let _ = tokio::fs::create_dir_all(&dir).await;
            let src = dir.join("payload.bin");
            let dest_dir = dir.join("incoming");
            let _ = tokio::fs::create_dir_all(&dest_dir).await;
            let payload: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8).collect();
            tokio::fs::write(&src, &payload).await.unwrap();

            let (up2relay_tx, mut up2relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let (relay2down_tx, relay2down_rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let (down2up_tx, down2up_rx) = mpsc::unbounded_channel();
            let (p_up_tx, mut p_up_rx) = mpsc::unbounded_channel();
            let (p_down_tx, mut p_down_rx) = mpsc::unbounded_channel();
            let abort = Arc::new(AtomicBool::new(false));

            let up = tokio::spawn(run(
                Direction::Upload,
                TransferSpec::Upload {
                    sources: vec![src.clone()],
                    streaming_window: DEFAULT_STREAMING_WINDOW,
                },
                Vec::new(),
                TransferIo {
                    wire_in: down2up_rx,
                    wire_out: up2relay_tx,
                    progress: p_up_tx,
                    abort: abort.clone(),
                },
            ));
            // The relay replays the sender's opening chunk four extra
            // times before anything else reaches the receiver: that is
            // what a dialog held open across several lrzsz retransmit
            // windows leaves queued on the divert.
            let relay = tokio::spawn(async move {
                let mut first = true;
                while let Some(chunk) = up2relay_rx.recv().await {
                    if first {
                        first = false;
                        for _ in 0..4 {
                            if relay2down_tx.send(chunk.clone()).is_err() {
                                return;
                            }
                        }
                    }
                    if relay2down_tx.send(chunk).is_err() {
                        return;
                    }
                }
            });
            let down = tokio::spawn(run(
                Direction::Download,
                TransferSpec::Download {
                    budget: None,
                    dest_dir: dest_dir.clone(),
                    resume_owner: None,
                },
                Vec::new(),
                TransferIo {
                    wire_in: relay2down_rx,
                    wire_out: down2up_tx,
                    progress: p_down_tx,
                    abort,
                },
            ));

            let all = async {
                up.await.unwrap();
                down.await.unwrap();
                relay.await.unwrap();
            };
            tokio::time::timeout(std::time::Duration::from_secs(20), all)
                .await
                .expect("transfer deadlocked on the duplicate openers");

            let mut saved: Option<PathBuf> = None;
            let mut completed = false;
            while let Ok(p) = p_down_rx.try_recv() {
                match p {
                    Progress::Completed { .. } => completed = true,
                    Progress::FileDone { path, .. } => saved = path,
                    Progress::Error(e) => panic!("download error: {e}"),
                    _ => {}
                }
            }
            assert!(completed, "download never completed");
            let mut up_completed = false;
            while let Ok(p) = p_up_rx.try_recv() {
                match p {
                    Progress::Completed { .. } => up_completed = true,
                    Progress::Error(e) => panic!("upload error: {e}"),
                    _ => {}
                }
            }
            assert!(up_completed, "upload never completed");
            let saved = saved.expect("no saved path reported");
            let got = tokio::fs::read(&saved).await.unwrap();
            assert_eq!(got, payload, "round-tripped bytes differ");

            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }
}

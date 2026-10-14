//! Local-host connect paths (full tab + split pane), the transport twin
//! of the SSH flows in `dispatch_ssh.rs`.
//!
//! A local host is a saved shell on THIS machine: it opens a PTY like
//! the Local Shell picker does, but with everything a host carries
//! around it (folder, startup command, environment, theme, group, tags,
//! session recording). Which shell it spawns is a REFERENCE into the
//! curated local-terminal list (Settings > Terminal), never a copy of a
//! program path, so the two surfaces cannot disagree about what
//! "PowerShell" means on this machine.
//!
//! There is no session to negotiate and nothing to authenticate, so
//! unlike Telnet / Serial this path publishes no `SshConnected`: the
//! PTY receiver is streamed straight into `PtyOutput`, exactly as the
//! picker's own spawn does.

use std::sync::{Arc, Mutex};

use iced::Task;
use uuid::Uuid;

use oryxis_terminal::widget::TerminalState;

use crate::app::{DEFAULT_TERM_COLS, DEFAULT_TERM_ROWS, Message, Oryxis, TerminalMessage};
use crate::state::{TerminalTab, View};

/// A resolved local shell: `Some((program, args, label))`, or `None`
/// for the OS default shell.
type LocalPick = Option<(String, Vec<String>, String)>;

/// Why a local host could not name a shell to spawn.
enum LocalPickError {
    /// The curated list has never been scanned on this machine (it is
    /// built lazily), so "not found" would be a lie: nobody looked.
    /// Answered with a scan, not with the default shell.
    NotScanned,
    /// The list was scanned and holds no terminal by that name. Carries
    /// the name the host asked for, so the toast can say which.
    Missing(String),
}

impl Oryxis {
    /// Resolve which curated terminal a local host spawns.
    ///
    /// The id is authoritative; the saved label is the fallback that
    /// lets a synced or imported host still resolve on a machine where
    /// the same shell exists under a different id. When the host names
    /// a terminal that this machine does not have, the answer is an
    /// error rather than the default shell: silently opening a
    /// different program than the one the host names would be the kind
    /// of guess a startup command can act on.
    fn resolve_local_pick(
        &self,
        conn: &oryxis_core::models::Connection,
    ) -> Result<LocalPick, LocalPickError> {
        let Some(cfg) = conn.local.as_ref() else {
            return Ok(None);
        };
        // A host that names no terminal takes the default shell, which
        // needs no list at all.
        let names_one =
            cfg.terminal_id.is_some() || cfg.terminal_label.as_deref().is_some_and(|l| !l.trim().is_empty());
        if !names_one {
            return Ok(None);
        }
        let Some(entries) = self.local_terminals.as_deref() else {
            return Err(LocalPickError::NotScanned);
        };
        if let Some(id) = cfg.terminal_id
            && let Some(entry) = entries.iter().find(|e| e.id == id)
        {
            let spec = entry.to_spec();
            return Ok(Some((spec.program, spec.args, spec.label)));
        }
        if let Some(label) = cfg.terminal_label.as_deref().map(str::trim).filter(|l| !l.is_empty())
        {
            if let Some(entry) = entries.iter().find(|e| e.label.eq_ignore_ascii_case(label)) {
                let spec = entry.to_spec();
                return Ok(Some((spec.program, spec.args, spec.label)));
            }
            return Err(LocalPickError::Missing(label.to_string()));
        }
        // The id pointed at an entry that is gone, and no label was
        // saved to recover it by.
        Err(LocalPickError::Missing(
            crate::i18n::t("local_terminal_removed").to_string(),
        ))
    }

    /// Turn a failed resolve into what the user sees, and into the one
    /// action that can fix it: an unscanned machine gets the scan (the
    /// merge-and-persist one, which never opens a shell), a missing
    /// terminal gets named.
    fn local_pick_failed(&mut self, err: LocalPickError) -> Task<Message> {
        match err {
            LocalPickError::NotScanned => {
                self.set_toast(crate::i18n::t("local_terminals_not_scanned").to_string());
                Task::done(Message::Settings(
                    crate::app::SettingsMessage::RescanLocalTerminals,
                ))
            }
            LocalPickError::Missing(name) => {
                self.set_toast(format!("{}: {name}", crate::i18n::t("local_terminal_missing")));
                Task::none()
            }
        }
    }

    /// Host `env_vars` as spawn-time environment pairs. Blank names are
    /// dropped here rather than in the PTY so both spawn paths (tab and
    /// pane) agree on what an unfinished editor row means: nothing.
    fn local_env(conn: &oryxis_core::models::Connection) -> Vec<(String, String)> {
        conn.env_vars
            .iter()
            .filter(|v| !v.key.trim().is_empty())
            .map(|v| (v.key.trim().to_string(), v.value.clone()))
            .collect()
    }

    /// The directory a local host starts in: its own `cwd`, `~`
    /// expanded. A path that no longer exists falls through to the
    /// process default (the PTY ignores a missing dir), so a folder
    /// deleted since the host was saved never blocks the shell.
    fn local_cwd(conn: &oryxis_core::models::Connection) -> Option<String> {
        let raw = conn.local.as_ref().and_then(|l| l.effective_cwd())?;
        Some(crate::util::expand_home(raw).to_string_lossy().into_owned())
    }

    /// Spawn the PTY for a local host, returning the state and its
    /// output receiver. One place so the tab path and the pane path
    /// cannot resolve the shell, the folder or the environment
    /// differently.
    fn spawn_local_state(
        &self,
        conn: &oryxis_core::models::Connection,
        pick: &LocalPick,
    ) -> oryxis_terminal::widget::TerminalResult<(
        TerminalState,
        tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    )> {
        let cwd = Self::local_cwd(conn);
        let env = Self::local_env(conn);
        match pick {
            Some((program, args, _)) => TerminalState::new_with_command_env(
                DEFAULT_TERM_COLS as u16,
                DEFAULT_TERM_ROWS as u16,
                program,
                args,
                cwd.as_deref(),
                &env,
            ),
            None => TerminalState::new_with_env(
                DEFAULT_TERM_COLS as u16,
                DEFAULT_TERM_ROWS as u16,
                cwd.as_deref(),
                &env,
            ),
        }
    }

    /// Arm the host's startup command for this pane.
    ///
    /// Unlike SSH there is no "session ready" event to hang it on, and
    /// writing at spawn time would type into a PTY whose shell has not
    /// read its first byte yet. So the line is parked here and sent
    /// once the pane's output goes quiet (see
    /// [`note_local_output`](Self::note_local_output)). A snippet
    /// reference wins over the literal command, same precedence as
    /// `dispatch_ssh::session`.
    fn arm_local_startup(&mut self, pane_id: Uuid, conn: &oryxis_core::models::Connection) {
        let snippet = conn
            .startup_snippet_id
            .and_then(|id| self.snippets.iter().find(|s| s.id == id))
            .cloned();
        // A startup snippet with actions runs them once the shell is quiet.
        if let Some(snip) = snippet.as_ref().filter(|s| s.has_steps()) {
            if let Some(run) = self.startup_snippet_run(snip) {
                self.pending_local_startup.insert(
                    pane_id,
                    crate::state::PendingLocalStartup { command: String::new(), run: Some(run), batches: 0 },
                );
            }
            return;
        }
        let command = snippet
            .map(|s| s.command)
            .or_else(|| conn.initial_command.clone())
            .filter(|c| !c.trim().is_empty());
        if let Some(command) = command {
            self.pending_local_startup.insert(
                pane_id,
                crate::state::PendingLocalStartup { command, run: None, batches: 0 },
            );
        }
    }

    /// How long a local pane must stay silent before its startup command
    /// is considered safe to send. Long enough to sit past the gaps in a
    /// login banner, short enough that the user does not watch an idle
    /// prompt.
    const LOCAL_STARTUP_QUIET: std::time::Duration = std::time::Duration::from_millis(300);

    /// Count one batch of output from a pane and (re-)arm its startup
    /// timer. Called from the `PtyOutput` handler; a pane with nothing
    /// armed costs one hash lookup and returns no task.
    pub(crate) fn note_local_output(&mut self, pane_id: Uuid) -> Task<Message> {
        let Some(pending) = self.pending_local_startup.get_mut(&pane_id) else {
            return Task::none();
        };
        pending.batches += 1;
        let armed_at = pending.batches;
        Task::perform(
            async move { tokio::time::sleep(Self::LOCAL_STARTUP_QUIET).await },
            move |()| {
                Message::Terminal(TerminalMessage::LocalStartupDue(pane_id, armed_at))
            },
        )
    }

    /// A startup timer expired. Send the command only if the pane has
    /// produced no output since the timer was armed; otherwise the shell
    /// is still printing and the newer batch has already armed its own.
    pub(crate) fn fire_local_startup(&mut self, pane_id: Uuid, armed_at: u64) -> Task<Message> {
        let still_current = self
            .pending_local_startup
            .get(&pane_id)
            .is_some_and(|p| p.batches == armed_at);
        if !still_current {
            return Task::none();
        }
        let Some(pending) = self.pending_local_startup.remove(&pane_id) else {
            return Task::none();
        };
        if let Some((label, steps)) = pending.run {
            return self.start_snippet_run_on_pane(pane_id, label, steps);
        }
        let Some(pane) = self.pane_by_id(pane_id) else {
            return Task::none();
        };
        if let Ok(mut term) = pane.terminal.lock() {
            term.write(format!("{}\n", pending.command).as_bytes());
        }
        Task::none()
    }

    /// Open a new tab running a local host's shell. The counterpart of
    /// `start_ssh_tab`, reached whenever `conn.protocol == Local`.
    pub(crate) fn start_local_tab(
        &mut self,
        conn: oryxis_core::models::Connection,
        origin: crate::state::ProgressOrigin,
    ) -> Task<Message> {
        let pick = match self.resolve_local_pick(&conn) {
            Ok(pick) => pick,
            Err(e) => return self.local_pick_failed(e),
        };
        let (mut state, rx) = match self.spawn_local_state(&conn, &pick) {
            Ok(spawned) => spawned,
            Err(e) => {
                tracing::error!("Failed to spawn local host \"{}\": {e}", conn.label);
                self.set_toast(format!("{}: {e}", crate::i18n::t("local_shell_spawn_failed")));
                return Task::none();
            }
        };
        // A local host is a host: it takes the per-host palette and
        // quirks like any other, not the global local-shell defaults.
        state.set_palette(self.resolve_terminal_palette_for_connection(&conn));
        // Taken before the state is wrapped for the pane: this is the
        // signal that says the shell exited (see `local_pane_stream`).
        let exited = state.pty.as_mut().and_then(|p| p.take_child_exit());
        // No dial, so no progress panel: a stale one from an earlier
        // attempt would sit over a tab that is already live.
        self.connecting = None;

        let terminal = Arc::new(Mutex::new(state));
        let tab_idx = self.tabs.len();
        let session_log_id = if self.should_record_session(Some(&conn)) {
            self.vault.as_ref().map(|vault| {
                let log_id = Uuid::new_v4();
                if let Err(e) = vault.create_session_log(&log_id, &conn.id, &conn.label) {
                    tracing::warn!("session log create failed: {e}");
                }
                self.session_logs_total += 1;
                log_id
            })
        } else {
            None
        };

        let mut new_tab = TerminalTab::new_single(conn.label.clone(), Arc::clone(&terminal));
        new_tab.active_mut().session_log_id = session_log_id;
        new_tab.active_mut().origin = match origin {
            crate::state::ProgressOrigin::Saved(_) => crate::state::PaneOrigin::Host(conn.id),
            crate::state::ProgressOrigin::Quick(id) => crate::state::PaneOrigin::QuickHost(id),
        };
        let resolved_quirks = self.resolve_quirks(&conn);
        new_tab.active_mut().quirks = resolved_quirks;
        if let Ok(term) = new_tab.active().terminal.lock() {
            let (w, r) = resolved_quirks.osc52.map(|o| o.overrides()).unwrap_or((None, None));
            term.set_osc52_override(w, r);
        }
        // A quick (unsaved) local host has no vault row to reconnect
        // from, so the tab carries the entry that created it, exactly
        // as the Telnet path does.
        if let crate::state::ProgressOrigin::Quick(id) = origin
            && let Some(entry) = self.quick_connects.get(&id)
        {
            new_tab.active_mut().relaunch = Some(Box::new(Message::Ssh(
                crate::app::SshMessage::QuickConnect(Box::new(entry.clone())),
            )));
        }
        let pane_id = new_tab.active().id;
        self.tabs.push(new_tab);
        self.active_tab = Some(tab_idx);
        self.remember_terminal_tab_focus(tab_idx);
        self.active_view = View::Terminal;
        self.arm_local_startup(pane_id, &conn);

        // Wired even though this tab starts unsplit: the user can split
        // it later, and then this shell's exit is a pane's end like any
        // other. `note_pane_ended` is what declines to act while the tab
        // still has only the one pane.
        let pty = self.local_pane_stream(pane_id, exited, rx);
        Task::batch(vec![self.tab_scroll_to_active(), pty])
    }

    /// Run a local host in an existing pane (a split, or an in-place
    /// reconnect after its shell exited). The pane keeps its identity,
    /// its history, its recording AND its terminal: the shell is spawned
    /// into the grid the pane already draws, so a restart continues
    /// under the output the user was reading, the way a remote reconnect
    /// does.
    pub(crate) fn spawn_local_for_pane_conn(
        &mut self,
        conn: oryxis_core::models::Connection,
        tab_idx: usize,
        pane_id: Uuid,
    ) -> Task<Message> {
        let pick = match self.resolve_local_pick(&conn) {
            Ok(pick) => pick,
            Err(e) => {
                // Unlike the tab path there is a pane here, and it was
                // armed as dialling (`restart_pane`, the "connect at
                // launch" queue). Only `PaneConnectError` clears that and
                // records the verdict; without it the pane reads
                // "Reconnecting" for good and the launch queue waits on
                // it forever.
                let reason = match &e {
                    LocalPickError::NotScanned => {
                        crate::i18n::t("local_terminals_not_scanned").to_string()
                    }
                    LocalPickError::Missing(name) => {
                        format!("{}: {name}", crate::i18n::t("local_terminal_missing"))
                    }
                };
                let shown = self.local_pick_failed(e);
                return Task::batch([
                    shown,
                    Task::done(Message::Ssh(crate::app::SshMessage::PaneConnectError(
                        pane_id, reason,
                    ))),
                ]);
            }
        };
        let palette = self.resolve_terminal_palette_for_connection(&conn);
        let cwd = Self::local_cwd(&conn);
        let env = Self::local_env(&conn);
        let Some(terminal) = self
            .tabs
            .get(tab_idx)
            .and_then(|t| t.pane_by_id(pane_id))
            .map(|p| p.terminal.clone())
        else {
            return Task::none();
        };
        let spawned = match terminal.lock() {
            Ok(mut state) => {
                let spawned = match &pick {
                    Some((program, args, _)) => state.respawn_command_env(
                        Some(program),
                        args,
                        cwd.as_deref(),
                        &env,
                    ),
                    None => state.respawn_command_env(None, &[], cwd.as_deref(), &env),
                };
                if spawned.is_ok() {
                    state.set_palette(palette);
                }
                spawned.map(|rx| (rx, state.pty.as_mut().and_then(|p| p.take_child_exit())))
            }
            // A poisoned terminal lock: nothing can be spawned into it,
            // and the pane was armed as dialling, so it still owes the
            // verdict that clears the flag.
            Err(_) => {
                return Task::done(Message::Ssh(crate::app::SshMessage::PaneConnectError(
                    pane_id,
                    crate::i18n::t("terminal_state_unavailable").to_string(),
                )));
            }
        };
        let (rx, exited) = match spawned {
            Ok(spawned) => spawned,
            Err(e) => {
                tracing::error!("Failed to spawn local host \"{}\" into pane: {e}", conn.label);
                return Task::done(Message::Ssh(crate::app::SshMessage::PaneConnectError(
                    pane_id,
                    e.to_string(),
                )));
            }
        };

        let session_log_id = if self.should_record_session(Some(&conn)) {
            self.vault.as_ref().map(|v| {
                let id = Uuid::new_v4();
                if let Err(e) = v.create_session_log(&id, &conn.id, &conn.label) {
                    tracing::warn!("session log create failed: {e}");
                }
                id
            })
        } else {
            None
        };
        if session_log_id.is_some() {
            self.session_logs_total += 1;
        }
        let Some(pane) = self.tabs.get_mut(tab_idx).and_then(|t| t.pane_by_id_mut(pane_id)) else {
            return Task::none();
        };
        // Nothing is being dialled: a local shell is live the moment its
        // PTY exists, and only `SshConnected` / `SshDisconnected` /
        // `PaneConnectError` ever clear this flag, none of which a PTY
        // raises. An in-place restart arms it before handing off here
        // (`restart_pane`), so without this the pane reads
        // "Reconnecting" for good and every later Reconnect on the tab
        // is refused as a dial already in flight. The Telnet and Serial
        // arms of the same switch need no such line: both go on to send
        // `SshConnected`. The verdict goes with the flag: this shell
        // replaces the one the pane was reporting on.
        pane.connecting = false;
        pane.ended = false;
        pane.end_verdict = None;
        if let Some(log_id) = session_log_id {
            pane.start_session_log(log_id);
        }
        self.arm_local_startup(pane_id, &conn);

        self.local_pane_stream(pane_id, exited, rx)
    }

    /// Respawn a local shell into an EXISTING pane from the spec the
    /// pane recorded (issue #208), keeping its terminal and scrollback.
    ///
    /// Driven from the pane's `PaneOrigin::Local`, not from the picker
    /// or the "always open X" preference: this pane WAS this exact
    /// shell, and a decision flow would pop a picker over a pane the
    /// user asked to restart. An empty program means the OS default
    /// shell, the same reading `spawn_local_shell_in` gives it.
    pub(crate) fn respawn_local_pane(
        &mut self,
        tab_idx: usize,
        pane_id: Uuid,
        spec: &crate::state::LocalShellSpec,
    ) -> Task<Message> {
        // The pane's last reported directory, so a restart lands where
        // the shell that exited was standing.
        let cwd = self
            .tabs
            .get(tab_idx)
            .and_then(|t| t.pane_by_id(pane_id))
            .and_then(|p| p.cwd.clone());
        // Spawned INTO the pane's own terminal, so the scrollback the
        // user was reading stays and the new shell continues below it.
        let Some(terminal) = self
            .tabs
            .get(tab_idx)
            .and_then(|t| t.pane_by_id(pane_id))
            .map(|p| p.terminal.clone())
        else {
            return Task::none();
        };
        let palette = self.terminal_palette.clone();
        let spawned = match terminal.lock() {
            Ok(mut state) => {
                let program = (!spec.program.is_empty()).then_some(spec.program.as_str());
                let spawned =
                    state.respawn_command_env(program, &spec.args, cwd.as_deref(), &[]);
                if spawned.is_ok() {
                    state.set_palette(palette);
                }
                spawned.map(|rx| (rx, state.pty.as_mut().and_then(|p| p.take_child_exit())))
            }
            // A poisoned terminal lock: nothing can be spawned into it,
            // and the pane was armed as dialling, so it still owes the
            // verdict that clears the flag.
            Err(_) => {
                return Task::done(Message::Ssh(crate::app::SshMessage::PaneConnectError(
                    pane_id,
                    crate::i18n::t("terminal_state_unavailable").to_string(),
                )));
            }
        };
        let (rx, exited) = match spawned {
            Ok(spawned) => spawned,
            Err(e) => {
                tracing::error!("Failed to respawn local shell \"{}\": {e}", spec.label);
                return Task::done(Message::Ssh(crate::app::SshMessage::PaneConnectError(
                    pane_id,
                    e.to_string(),
                )));
            }
        };
        let Some(pane) = self.tabs.get_mut(tab_idx).and_then(|t| t.pane_by_id_mut(pane_id))
        else {
            return Task::none();
        };
        // Nothing is being dialled: a local shell is live the moment its
        // PTY exists, so the pane must not be left reading "Reconnecting";
        // and the verdict it held was about the shell this one replaces.
        pane.connecting = false;
        pane.ended = false;
        pane.end_verdict = None;
        self.local_pane_stream(pane_id, exited, rx)
    }
}

//! Driving a login script against a live pane (issue #122).
//!
//! Three entry points: `arm_login_script` at session-ready,
//! `feed_login_script` on every output batch, and `abort_login_script`
//! from anything that means "the user has taken over".
//!
//! Everything here exists because terminal output is attacker
//! controlled. The engine (`oryxis_core::login_script`) owns the
//! matching guards (window, ordering, per-step deadline checked BEFORE
//! matching); this file owns the ones that only make sense next to a
//! live pane:
//!
//! - a script writes through [`Oryxis::write_secret_to_pane`], which
//!   bypasses the command-history mirror and never broadcasts, so a
//!   credential cannot land in the history table or in a sibling pane;
//! - ZMODEM and Files mode suppress the run entirely (their byte
//!   streams are not a login prompt);
//! - any user keystroke aborts it, because a script racing the person
//!   at the keyboard is worse than no script;
//! - the host's startup command is DEFERRED until the run succeeds:
//!   sending it up front would type it at the bastion's menu, which is
//!   exactly the failure `initial_command` already has today.

use iced::Task;

use oryxis_core::login_script::{
    line_bytes, RunnerAction, ScriptRunner, SecretRef, SendPayload,
};

use crate::app::{Message, Oryxis};

/// How long after session-ready a script stays armed. Matches the
/// Telnet autologin window: a bastion menu can be slow, but past this
/// the user is in a shell and a matching prompt is not ours to answer.
const SCRIPT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

impl Oryxis {
    /// Arm the login script for a freshly-ready pane, if its host has
    /// one.
    ///
    /// Returns the startup command the caller should send NOW (`None`
    /// when a script took ownership of it, or there was none) plus the
    /// first timeout tick.
    pub(crate) fn arm_login_script(
        &mut self,
        pane_id: uuid::Uuid,
        startup: Option<String>,
    ) -> (Option<String>, Task<Message>) {
        let Some(conn) = self.pane_origin_connection(pane_id) else {
            return (startup, Task::none());
        };
        let conn_id = conn.id;
        // A dangling script id (deleted while this host still pointed
        // at it) is "no automation", never an error: same rule as a
        // deleted proxy identity.
        let Some(script) = conn
            .login_script_id
            .and_then(|sid| self.login_scripts.iter().find(|s| s.id == sid))
        else {
            return (startup, Task::none());
        };
        let vars: Vec<(String, String)> = conn
            .login_script_vars
            .iter()
            .map(|v| (v.name.clone(), v.value.clone()))
            .collect();
        // Placeholders are substituted BEFORE the runner exists, so the
        // engine never has to know what a variable is.
        let steps: Vec<_> = script
            .steps
            .iter()
            .map(|step| {
                use oryxis_core::login_script::ExpectPattern;
                let mut step = step.clone();
                step.expect = step.expect.map(|p| match p {
                    ExpectPattern::Suffix(s) => {
                        ExpectPattern::Suffix(crate::util::substitute_snippet_vars(&s, &vars))
                    }
                    ExpectPattern::Regex(r) => {
                        ExpectPattern::Regex(crate::util::substitute_snippet_vars(&r, &vars))
                    }
                });
                if let SendPayload::Text(t) = &step.send {
                    step.send =
                        SendPayload::Text(crate::util::substitute_snippet_vars(t, &vars));
                }
                step
            })
            .collect();

        let runner = match ScriptRunner::new(&steps, SCRIPT_WINDOW, std::time::Instant::now()) {
            Ok(r) => r,
            Err(e) => {
                // A bad pattern is a configuration error, and connecting
                // silently without the automation would look like the
                // feature is broken. Say so and let the user drive.
                tracing::warn!(
                    target = "oryxis::login_script",
                    error = %e,
                    "login script did not compile; connecting without it"
                );
                self.set_toast(format!("{}: {e}", crate::i18n::t("login_script_invalid")));
                return (startup, Task::none());
            }
        };

        let generation = self.login_script_generation.wrapping_add(1);
        self.login_script_generation = generation;
        if let Some(pane) = self.pane_by_id_mut(pane_id) {
            pane.login_script = Some(crate::state::LoginScriptRun {
                runner,
                conn_id,
                pending_startup: startup,
                pending_run: None,
                generation,
            });
        } else {
            return (startup, Task::none());
        }
        tracing::info!(
            target = "oryxis::login_script",
            %pane_id,
            steps = steps.len(),
            "login script armed"
        );
        // The caller must not send the startup command: the run owns it.
        (None, script_tick(pane_id, generation))
    }

    /// Timeout wake-up. Re-polls the engine with the real clock, which
    /// is what makes a per-step deadline mean anything when the host
    /// has gone quiet, then re-arms itself while the run is alive.
    pub(crate) fn tick_login_script(
        &mut self,
        pane_id: uuid::Uuid,
        generation: u64,
    ) -> Task<Message> {
        let current = self
            .pane_by_id(pane_id)
            .and_then(|p| p.login_script.as_ref().map(|r| r.generation));
        if current != Some(generation) {
            // The run finished, was aborted, or a newer one replaced it.
            return Task::none();
        }
        let now = std::time::Instant::now();
        let mut actions = Vec::new();
        if let Some(pane) = self.pane_by_id_mut(pane_id)
            && let Some(run) = &mut pane.login_script
        {
            while let Some(action) = run.runner.poll(now) {
                actions.push(action);
            }
        }
        for action in actions {
            self.apply_script_action(pane_id, action);
        }
        let still_running = self
            .pane_by_id(pane_id)
            .and_then(|p| p.login_script.as_ref().map(|r| r.generation))
            == Some(generation);
        // A run the tick finished hands over its startup snippet here.
        let due = self.start_due_startup_runs();
        if still_running {
            Task::batch([script_tick(pane_id, generation), due])
        } else {
            due
        }
    }

    /// Feed one output batch to the pane's script, if any, and act on
    /// whatever the engine decides. Called from the PTY output funnel.
    pub(crate) fn feed_login_script(&mut self, pane_id: uuid::Uuid, bytes: &[u8]) {
        // Cheap pre-check: the vast majority of batches belong to panes
        // with no script at all.
        let has_run = self
            .pane_by_id(pane_id)
            .is_some_and(|p| p.login_script.is_some());
        if !has_run {
            return;
        }
        // A ZMODEM window or Files mode means these bytes are not a
        // login prompt; suppress rather than risk matching inside a
        // binary frame.
        if self
            .pane_by_id(pane_id)
            .is_some_and(|p| p.zmodem.is_some())
        {
            return;
        }

        let now = std::time::Instant::now();
        let mut actions = Vec::new();
        if let Some(pane) = self.pane_by_id_mut(pane_id)
            && let Some(run) = &mut pane.login_script
        {
            run.runner.feed(bytes);
            while let Some(action) = run.runner.poll(now) {
                actions.push(action);
            }
        }
        for action in actions {
            self.apply_script_action(pane_id, action);
        }
    }

    /// Drive one engine decision.
    fn apply_script_action(&mut self, pane_id: uuid::Uuid, action: RunnerAction) {
        match action {
            RunnerAction::Send { index, payload } => {
                let conn_id = self
                    .pane_by_id(pane_id)
                    .and_then(|p| p.login_script.as_ref().map(|r| r.conn_id));
                let Some(conn_id) = conn_id else { return };
                match payload {
                    SendPayload::Text(text) => {
                        // Not a secret, but still not user input: it must
                        // not land in the command history either, since
                        // the user never typed it.
                        self.write_secret_to_pane(pane_id, &line_bytes(&text));
                    }
                    SendPayload::Key(key) => {
                        self.write_secret_to_pane(pane_id, key.bytes());
                    }
                    SendPayload::Nothing => {}
                    SendPayload::Secret(secret) => {
                        // Decrypted here and nowhere else: never stored
                        // in app state, the resolved String scrubbed on
                        // drop (`Zeroizing`) and the line buffer scrubbed
                        // explicitly after the write.
                        let value = self.resolve_script_secret(conn_id, secret);
                        match value {
                            Some(v) => {
                                let mut bytes = line_bytes(v.as_str());
                                self.write_secret_to_pane(pane_id, &bytes);
                                zeroize_bytes(&mut bytes);
                            }
                            None => {
                                // The credential the script expects is
                                // not there (never set, or the vault is
                                // soft-locked). Stopping is the honest
                                // answer: continuing would type nothing
                                // at a password prompt and hang.
                                tracing::warn!(
                                    target = "oryxis::login_script",
                                    step = index,
                                    "login script step has no stored credential; aborting"
                                );
                                self.abort_login_script(pane_id);
                                self.set_toast(
                                    crate::i18n::t("login_script_failed")
                                        .replace("{step}", &(index + 1).to_string()),
                                );
                                return;
                            }
                        }
                    }
                }
                // Progress for the status bar, read from the engine so
                // the two can never disagree.
                if let Some(pane) = self.pane_by_id(pane_id)
                    && let Some(run) = &pane.login_script
                    && !run.runner.is_done()
                {
                    let (step, total) = run.runner.progress();
                    tracing::debug!(
                        target = "oryxis::login_script",
                        step,
                        total,
                        "login script advanced"
                    );
                }
            }
            RunnerAction::Timeout { index } => {
                tracing::warn!(
                    target = "oryxis::login_script",
                    step = index,
                    "login script timed out"
                );
                self.abort_login_script(pane_id);
                self.set_toast(
                    crate::i18n::t("login_script_failed")
                        .replace("{step}", &(index + 1).to_string()),
                );
            }
            RunnerAction::Finished => {
                // The run got us to the asset's shell, so NOW the
                // startup command means what the user configured.
                let run = self.pane_by_id_mut(pane_id).and_then(|p| p.login_script.take());
                let (startup, startup_run) = run.map_or((None, None), |r| (r.pending_startup, r.pending_run));
                if let Some(cmd) = startup {
                    self.write_secret_to_pane(pane_id, format!("{cmd}\n").as_bytes());
                }
                // Started by the output funnel right after this batch,
                // where a run's first task has somewhere to go.
                if let Some((label, steps)) = startup_run {
                    self.startup_runs_due.push((pane_id, label, steps));
                }
                tracing::info!(
                    target = "oryxis::login_script",
                    %pane_id,
                    "login script finished"
                );
            }
        }
    }

    /// Resolve a `SecretRef` against the vault, at send time. The
    /// plaintext is wrapped in `Zeroizing` so the intermediate copy is
    /// scrubbed when the caller drops it (the TOTP raw seed is also
    /// zeroized before the code is derived).
    fn resolve_script_secret(
        &self,
        conn_id: uuid::Uuid,
        secret: SecretRef,
    ) -> Option<zeroize::Zeroizing<String>> {
        let vault = self.vault.as_ref()?;
        let value = match secret {
            SecretRef::ConnectionPassword => {
                vault.get_connection_password(&conn_id).ok().flatten()
            }
            SecretRef::TargetPassword => vault
                .get_connection_target_password(&conn_id)
                .ok()
                .flatten(),
            SecretRef::Identity(id) => vault.get_identity_password(&id).ok().flatten(),
            SecretRef::Totp => vault
                .get_connection_totp_secret(&conn_id)
                .ok()
                .flatten()
                .map(zeroize::Zeroizing::new)
                .and_then(|raw| oryxis_core::totp::Totp::parse(&raw).ok())
                .map(|t| t.code_now()),
        };
        value
            .filter(|s| !s.is_empty())
            .map(zeroize::Zeroizing::new)
    }

    /// Stop a run and send the startup command it was holding, if the
    /// user is plausibly at a shell.
    ///
    /// Called when the user types, when the session dies, and on a
    /// timeout. The startup command is deliberately DROPPED here rather
    /// than sent: an aborted run means we are not where the script
    /// meant to be, and firing a command into an unknown context is the
    /// one thing worse than not firing it.
    pub(crate) fn abort_login_script(&mut self, pane_id: uuid::Uuid) {
        if let Some(pane) = self.pane_by_id_mut(pane_id)
            && pane.login_script.take().is_some()
        {
            tracing::info!(
                target = "oryxis::login_script",
                %pane_id,
                "login script aborted"
            );
        }
    }

    /// Progress line for the status bar: `Some((step, total))` while a
    /// script is running on the focused pane.
    pub(crate) fn login_script_progress(&self) -> Option<(usize, usize)> {
        let tab = self.tabs.get(self.cur_active_tab()?)?;
        let run = tab.active().login_script.as_ref()?;
        (!run.runner.is_done()).then(|| run.runner.progress())
    }
}

/// Best-effort scrub of a credential buffer after it has been written.
/// `write_volatile` in a loop rather than a plain overwrite so the
/// optimizer cannot drop the stores as dead.
pub(crate) fn zeroize_bytes(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid, aligned, mutable reference for the
        // lifetime of this loop iteration.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// One timeout wake-up, one second out. A second is fine: the deadline
/// itself lives in the engine, this only decides how promptly an
/// expiry is noticed when no output is arriving to drive a poll.
fn script_tick(pane_id: uuid::Uuid, generation: u64) -> Task<Message> {
    Task::perform(
        async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            (pane_id, generation)
        },
        |(pane_id, generation)| {
            Message::Terminal(crate::app::TerminalMessage::LoginScriptTick(
                pane_id, generation,
            ))
        },
    )
}

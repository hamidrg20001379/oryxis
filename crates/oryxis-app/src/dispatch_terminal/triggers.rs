//! What a highlight rule does when it matches (C6).
//!
//! The scan happens in the terminal crate; this is the half that acts.
//! Three actions, in ascending order of how much they can do: a
//! notification, a beep, and typing a stored snippet into the session.
//!
//! Only the third one is dangerous, and it is dangerous in a specific
//! way: the thing that decides whether it fires is text printed by the
//! REMOTE HOST. Without a guard, any server that can write to the
//! terminal could pick which of your snippets runs on it. So a snippet
//! action asks once per rule per session, refusals are remembered, and
//! the confirmation names the rule, the snippet and the line that
//! matched, because the user has to be able to tell an expected trigger
//! from output that just showed up.

use super::*;

use oryxis_core::models::TriggerAction;

/// A pending "may this rule run its snippet" question.
#[derive(Debug, Clone)]
pub(crate) struct TriggerConfirmCard {
    /// The pane the match arrived on, and the only pane the snippet may
    /// be sent to.
    pub pane_id: uuid::Uuid,
    pub rule_id: String,
    pub rule_name: String,
    pub snippet_label: String,
    /// The snippet's body, shown so the user is agreeing to something
    /// they can read rather than to a name. For a snippet with actions,
    /// every step (nested snippets expanded), one per line.
    pub snippet_body: String,
    /// A snippet with actions: the steps, expanded, that run on a yes.
    pub steps: Option<Vec<oryxis_core::models::SnippetStep>>,
    /// The line that matched. Under Privacy Mode this arrives redacted.
    pub line: String,
}

impl Oryxis {
    /// Run one rule's action on the pane its match arrived on.
    ///
    /// The cooldown has already been paid by the caller, so reaching
    /// here means the action is due.
    pub(crate) fn run_trigger_action(
        &mut self,
        pane_id: uuid::Uuid,
        rule_id: &str,
        rule_name: &str,
        line: &str,
        action: &TriggerAction,
    ) -> Task<Message> {
        match action {
            TriggerAction::None => {}
            TriggerAction::Beep => crate::util::play_system_beep(),
            TriggerAction::Notify => {
                let title = if rule_name.trim().is_empty() {
                    crate::i18n::t("highlight_rules").to_string()
                } else {
                    rule_name.to_string()
                };
                let body = self.trigger_body_for_display(pane_id, line);
                // Delivery mirrors an OSC 9 notification: while the
                // window is in front an in-app toast is enough (a
                // native popup for the thing you are already looking at
                // is noise), and away from it the OS notification is
                // the whole point, with a toast when the OS refuses (no
                // daemon on Linux, no AppUserModelID on a non-installed
                // Windows build).
                //
                // The `terminal_notification` setting is NOT consulted,
                // deliberately: it governs notifications the SHELL asks
                // for, and a user who turned those off was refusing the
                // remote host, not themselves. A rule's action is
                // opt-in per rule, so the rule is the switch.
                let toast = format!("{title}: {body}");
                if !self.notify_away(&title, &body, Some(toast.clone())) {
                    self.set_toast(toast);
                }
            }
            TriggerAction::Snippet { id } => {
                let Some(snippet) = self.snippets.iter().find(|s| s.id.to_string() == *id) else {
                    // The snippet was deleted after the rule was
                    // written. The highlight still paints; only the
                    // action is gone, and saying so beats silence.
                    tracing::warn!(rule = rule_id, "highlight rule points at a missing snippet");
                    return Task::none();
                };
                let label = snippet.label.clone();
                // A snippet with actions runs them, nested ones expanded
                // now, so the confirmation lists what will actually run.
                let steps = if snippet.has_steps() {
                    match crate::snippet_run::flatten(&self.snippets, snippet) {
                        Ok(steps) => Some(steps),
                        Err(e) => {
                            return self.show_toast(crate::dispatch_snippet_run::plan_error_text(&label, &e));
                        }
                    }
                } else {
                    None
                };
                let body = match &steps {
                    Some(steps) => self.describe_snippet_steps(steps),
                    None => snippet.command.clone(),
                };
                let allowed = self
                    .pane_by_id(pane_id)
                    .and_then(|p| p.triggers.get(rule_id))
                    .and_then(|t| t.snippet_allowed);
                match allowed {
                    // Already refused this session: stay refused. A rule
                    // that could re-ask on the next matching line would
                    // be a way to wear the user down.
                    Some(false) => {}
                    Some(true) => return self.send_trigger_snippet(pane_id, rule_id, &label, &body, steps),
                    None => {
                        // One question at a time per rule: a burst of
                        // matching lines must not stack dialogs.
                        let already_asking = self
                            .pane_by_id(pane_id)
                            .and_then(|p| p.triggers.get(rule_id))
                            .is_some_and(|t| t.asking);
                        if already_asking || self.trigger_confirm.is_some() {
                            return Task::none();
                        }
                        let line = self.trigger_body_for_display(pane_id, line);
                        if let Some(pane) = self.pane_by_id_mut(pane_id) {
                            pane.triggers.entry(rule_id.to_string()).or_default().asking = true;
                        }
                        self.trigger_confirm = Some(TriggerConfirmCard {
                            pane_id,
                            rule_id: rule_id.to_string(),
                            rule_name: rule_name.to_string(),
                            snippet_label: label,
                            snippet_body: body,
                            steps,
                            line,
                        });
                    }
                }
            }
        }
        Task::none()
    }

    /// The matching line as it may be SHOWN. Privacy Mode masks the
    /// terminal at render time only, so a line that leaves the app (an
    /// OS notification is stored by the notification centre) has to be
    /// redacted here, the same rule the OSC 9 and smart-tab bodies
    /// follow.
    fn trigger_body_for_display(&self, pane_id: uuid::Uuid, line: &str) -> String {
        let label = self
            .pane_by_id(pane_id)
            .map(|p| p.label.clone())
            .unwrap_or_default();
        let trimmed = line.trim();
        if self.privacy_active_for_label(&label) {
            crate::widgets::redact_for_display(
                trimmed,
                &self.privacy_terms(),
                self.privacy_classes(),
            )
        } else {
            trimmed.to_string()
        }
    }

    /// Answer the confirmation. `allow` is remembered for the session
    /// either way: yes means the rule may keep firing on this pane, no
    /// means it never asks again here.
    pub(crate) fn resolve_trigger_confirm(&mut self, allow: bool) -> Task<Message> {
        let Some(card) = self.trigger_confirm.take() else {
            return Task::none();
        };
        if let Some(pane) = self.pane_by_id_mut(card.pane_id) {
            let entry = pane.triggers.entry(card.rule_id.clone()).or_default();
            entry.asking = false;
            entry.snippet_allowed = Some(allow);
        }
        if !allow {
            return Task::none();
        }
        self.send_trigger_snippet(card.pane_id, &card.rule_id, &card.snippet_label, &card.snippet_body, card.steps)
    }

    /// Type a snippet into ONE pane.
    ///
    /// Not through `write_input_to_tab`: that funnel broadcasts to every
    /// participating pane of a split tab and mirrors into the
    /// command-history capture, and neither is right here. A trigger
    /// fired because of what arrived on ONE session, so that is the only
    /// session it may answer; and the capture mirrors what the USER
    /// typed, which this is not. A host with shell integration still
    /// records the command through the OSC 133 marks, like any command
    /// the shell runs.
    ///
    /// A snippet with actions starts a run on that pane instead (its
    /// commands, too, go to that pane alone). A pane already running one
    /// lets this match pass: a burst of matching lines must not queue
    /// runs, nor a toast each.
    fn send_trigger_snippet(
        &mut self,
        pane_id: uuid::Uuid,
        rule_id: &str,
        label: &str,
        body: &str,
        steps: Option<Vec<oryxis_core::models::SnippetStep>>,
    ) -> Task<Message> {
        if let Some(steps) = steps {
            if self.snippet_run_progress(pane_id).is_some() {
                tracing::info!(rule = rule_id, %pane_id, "highlight rule skipped: a snippet is already running");
                return Task::none();
            }
            tracing::info!(rule = rule_id, %pane_id, "highlight rule started its snippet");
            return self.start_snippet_run_on_pane(pane_id, label.to_string(), steps);
        }
        let text = body.trim_end_matches(['\n', '\r']).to_string();
        if text.is_empty() {
            return Task::none();
        }
        // Auditable by design: output on a remote host caused a command
        // to run on it, and that should be findable in the log.
        tracing::info!(rule = rule_id, %pane_id, "highlight rule sent its snippet");
        let snap = self.prefs.scrollback_reset_keypress;
        let mut bytes = text.into_bytes();
        bytes.push(b'\n');
        for tab in &mut self.tabs {
            if tab.files_mode {
                continue;
            }
            if let Some(pane) = tab.pane_grid.panes.values_mut().find(|p| p.id == pane_id) {
                Self::write_bytes_to_pane(pane, &bytes, snap);
                break;
            }
        }
        Task::none()
    }

    /// Drop every trigger grant and cooldown for a pane. Called when its
    /// session ends: consent was given to a session, not to a pane id
    /// that a reconnect happens to reuse.
    ///
    /// This and [`Self::resolve_trigger_confirm`] are the only two
    /// places that clear `trigger_confirm`, and both clear the `asking`
    /// flag with it. A third one that forgot would strand the rule: it
    /// would believe a question is still on screen and never ask again.
    pub(crate) fn reset_triggers_for_pane(&mut self, pane_id: uuid::Uuid) {
        if let Some(pane) = self.pane_by_id_mut(pane_id) {
            pane.triggers.clear();
        }
        if self
            .trigger_confirm
            .as_ref()
            .is_some_and(|c| c.pane_id == pane_id)
        {
            self.trigger_confirm = None;
        }
    }
}

//! The snippet editor's list of actions: editing the rows, and turning
//! them into the snippet's steps on save.

use oryxis_core::models::SnippetStep;

use crate::app::Oryxis;
use crate::messages::SnippetMessage;
use crate::state::{StepForm, StepKind};

impl Oryxis {
    pub(crate) fn handle_snippet_step_edit(&mut self, message: SnippetMessage) {
        let steps = &mut self.snippet_form.steps;
        match message {
            SnippetMessage::SnippetStepAdd => steps.push(StepForm::new(StepKind::Command)),
            SnippetMessage::SnippetStepRemove(i) => {
                // The last row stays: a snippet does something.
                if steps.len() > 1 && i < steps.len() {
                    steps.remove(i);
                }
            }
            SnippetMessage::SnippetStepMoveUp(i) => {
                if i > 0 && i < steps.len() {
                    steps.swap(i - 1, i);
                }
            }
            SnippetMessage::SnippetStepMoveDown(i) => {
                if i + 1 < steps.len() {
                    steps.swap(i, i + 1);
                }
            }
            other => {
                // A pick from a dropdown closes it without the widget
                // publishing `on_close` (the fork's menu closes itself),
                // so the key guard it raised is dropped here.
                if matches!(
                    other,
                    SnippetMessage::SnippetStepKindPicked(..)
                        | SnippetMessage::SnippetStepActionPicked(..)
                        | SnippetMessage::SnippetStepSnippetPicked(..)
                        | SnippetMessage::SnippetStepSaveModePicked(..)
                        | SnippetMessage::SnippetStepSaveContentPicked(..)
                ) {
                    self.keynav.pick_open = false;
                }
                let steps = &mut self.snippet_form.steps;
                let Some(row) = row_of(&other).and_then(|i| steps.get_mut(i)) else {
                    return;
                };
                match other {
                    SnippetMessage::SnippetStepCommandAction(_, action) => row.command.perform(action),
                    SnippetMessage::SnippetStepKindPicked(_, kind) => {
                        // A step from a newer build is replaced only by an
                        // explicit pick; its raw form goes with it.
                        row.kind = kind;
                        row.unknown = None;
                    }
                    SnippetMessage::SnippetStepActionPicked(_, action) => row.action = Some(action),
                    SnippetMessage::SnippetStepSnippetPicked(_, id) => row.snippet = Some(id),
                    SnippetMessage::SnippetStepPatternChanged(_, v) => row.pattern = v,
                    SnippetMessage::SnippetStepTimeoutChanged(_, v) => row.timeout = v,
                    SnippetMessage::SnippetStepSecondsChanged(_, v) => row.seconds = v,
                    SnippetMessage::SnippetStepFileNameChanged(_, v) => row.file_name = v,
                    SnippetMessage::SnippetStepToggleRegex(_) => row.regex = !row.regex,
                    SnippetMessage::SnippetStepToggleContinue(_) => row.continue_on_error = !row.continue_on_error,
                    SnippetMessage::SnippetStepSaveModePicked(_, mode) => row.file_mode = mode,
                    SnippetMessage::SnippetStepSaveContentPicked(_, content) => row.file_content = content,
                    _ => {}
                }
            }
        }
    }

    /// The editor's rows as the snippet's steps, or the form error that
    /// stops the save (the row's number included).
    pub(crate) fn snippet_form_steps(&self) -> Result<Vec<SnippetStep>, String> {
        let t = crate::i18n::t;
        let form = &self.snippet_form;
        if form.label.trim().is_empty() {
            return Err(t("snippet_name_required").to_string());
        }
        let mut steps = Vec::with_capacity(form.steps.len());
        for (i, row) in form.steps.iter().enumerate() {
            match row.to_step() {
                Ok(step) => steps.push(step),
                Err(key) => {
                    return Err(t("snippet_step_error")
                        .replace("{n}", &(i + 1).to_string())
                        .replace("{why}", &t(key).replace("{detail}", "")));
                }
            }
        }
        // An install script is one command, shown whole before it runs.
        if form.install && !matches!(steps.as_slice(), [SnippetStep::Command { .. }]) {
            return Err(t("snippet_install_single_command").to_string());
        }
        let id = form.editing_id.unwrap_or_else(uuid::Uuid::new_v4);
        if let Some(name) = crate::snippet_run::would_cycle(&self.snippets, id, &form.label, &steps) {
            return Err(t("snippet_run_cycle").replace("{name}", &name));
        }
        Ok(steps)
    }
}

/// The row a step-editing message is about.
fn row_of(message: &SnippetMessage) -> Option<usize> {
    match message {
        SnippetMessage::SnippetStepCommandAction(i, _)
        | SnippetMessage::SnippetStepKindPicked(i, _)
        | SnippetMessage::SnippetStepActionPicked(i, _)
        | SnippetMessage::SnippetStepSnippetPicked(i, _)
        | SnippetMessage::SnippetStepPatternChanged(i, _)
        | SnippetMessage::SnippetStepTimeoutChanged(i, _)
        | SnippetMessage::SnippetStepSecondsChanged(i, _)
        | SnippetMessage::SnippetStepFileNameChanged(i, _)
        | SnippetMessage::SnippetStepToggleRegex(i)
        | SnippetMessage::SnippetStepToggleContinue(i)
        | SnippetMessage::SnippetStepSaveModePicked(i, _)
        | SnippetMessage::SnippetStepSaveContentPicked(i, _) => Some(*i),
        _ => None,
    }
}

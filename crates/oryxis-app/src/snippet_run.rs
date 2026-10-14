//! A snippet with actions, run step by step on one pane.
//!
//! This half is pure: what a run IS (the flattened steps, where it waits),
//! which app actions a step may name, how a nested snippet is expanded,
//! how a save step names its file. The half that talks to panes, the
//! clipboard and the disk is `dispatch_snippet_run.rs`.
//!
//! Snippets travel (sync, `.oryxis` import), so the DATA never decides
//! what runs on this device: an action step names an id, and only the
//! ids [`allowed_step_actions`] lists are run; a save step names a file,
//! and only inside the output folder this device chose.

use oryxis_core::login_script::OutputMatcher;
use oryxis_core::models::{Snippet, SnippetStep};
use uuid::Uuid;

use crate::hotkeys::HotkeyAction;

/// The app actions a snippet step may run: pane actions only, each aimed
/// at the pane the run belongs to. Anything else a step names (a newer
/// build's action, a tab or vault verb arriving by sync) stops the run.
pub(crate) fn allowed_step_actions() -> &'static [HotkeyAction] {
    &[
        HotkeyAction::TerminalClearScrollback,
        HotkeyAction::TerminalCopyScreen,
        HotkeyAction::TerminalCopyLastCommand,
        HotkeyAction::TerminalSaveOutput,
    ]
}

/// The allowed action a step's id names, if any.
pub(crate) fn step_action(id: &str) -> Option<HotkeyAction> {
    allowed_step_actions().iter().copied().find(|a| a.id() == id)
}

/// Why a snippet's actions cannot be laid out for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanError {
    /// A snippet runs itself, directly or through others (its label).
    Cycle(String),
    /// A "run snippet" step names one that is not in this vault.
    Missing,
    /// Expansion past [`MAX_PLAN_STEPS`] (a snippet called many times
    /// over through others).
    TooLong,
}

/// The most steps one run may expand to.
pub(crate) const MAX_PLAN_STEPS: usize = 1000;

/// `root`'s actions with every "run snippet" step replaced by that
/// snippet's own actions, recursively. A snippet that reaches itself is
/// refused, never run in a loop.
pub(crate) fn flatten(snippets: &[Snippet], root: &Snippet) -> Result<Vec<SnippetStep>, PlanError> {
    let mut out = Vec::new();
    let mut stack = vec![root.id];
    expand(snippets, root, &mut stack, &mut out)?;
    Ok(out)
}

fn expand(
    snippets: &[Snippet],
    snippet: &Snippet,
    stack: &mut Vec<Uuid>,
    out: &mut Vec<SnippetStep>,
) -> Result<(), PlanError> {
    for step in snippet.actions() {
        match step {
            SnippetStep::RunSnippet { id } => {
                let inner = snippets.iter().find(|s| s.id == id).ok_or(PlanError::Missing)?;
                if stack.contains(&id) {
                    return Err(PlanError::Cycle(inner.label.clone()));
                }
                stack.push(id);
                expand(snippets, inner, stack, out)?;
                stack.pop();
            }
            other => {
                if out.len() >= MAX_PLAN_STEPS {
                    return Err(PlanError::TooLong);
                }
                out.push(other);
            }
        }
    }
    Ok(())
}

/// Whether saving `steps` as snippet `id` (named `label`) would make it
/// reach itself; `Some` names the snippet the loop comes back to. The
/// editor refuses such a save, so a cycle can only arrive by sync, where
/// [`flatten`] refuses to run it.
pub(crate) fn would_cycle(
    snippets: &[Snippet],
    id: Uuid,
    label: &str,
    steps: &[SnippetStep],
) -> Option<String> {
    let mut probe = Snippet::new(label, "");
    probe.id = id;
    probe.steps = steps.to_vec();
    let others: Vec<Snippet> = snippets.iter().filter(|s| s.id != id).cloned().chain([probe.clone()]).collect();
    match flatten(&others, &probe) {
        Err(PlanError::Cycle(label)) => Some(label),
        _ => None,
    }
}

/// The `{name}` placeholders of every command step, in order, each once
/// (the first default wins): what the variables dialog asks for.
pub(crate) fn plan_placeholders(steps: &[SnippetStep]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for step in steps {
        if let SnippetStep::Command { text } = step {
            for (name, default) in crate::util::snippet_placeholders(text) {
                if !out.iter().any(|(n, _)| *n == name) {
                    out.push((name, default));
                }
            }
        }
    }
    out
}

/// `steps` with the variables filled into every command step.
pub(crate) fn substitute(steps: Vec<SnippetStep>, vars: &[(String, String)]) -> Vec<SnippetStep> {
    steps
        .into_iter()
        .map(|step| match step {
            SnippetStep::Command { text } => SnippetStep::Command {
                text: crate::util::substitute_snippet_vars(&text, vars),
            },
            other => other,
        })
        .collect()
}

/// The file name a save step writes, `{host}` and `{date}` expanded and
/// reduced to one safe name: no folder part, no `..`, no leading dot or
/// dash. `None` when nothing usable is left.
pub(crate) fn save_file_name(template: &str, host: &str, date: &str) -> Option<String> {
    let expanded = template.replace("{host}", host).replace("{date}", date);
    let mut name: String = expanded
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ') { c } else { '_' })
        .collect();
    name = name.trim().to_string();
    while name.starts_with('.') || name.starts_with('-') {
        name.replace_range(0..1, "_");
    }
    let mut name: String = name.chars().take(120).collect();
    // Windows drops trailing dots and spaces, and reads these stems as
    // devices (`NUL.txt` "succeeds" and writes nowhere).
    while name.ends_with('.') || name.ends_with(' ') {
        name.pop();
    }
    if name.chars().all(|c| c == '_' || c == '.') {
        return None;
    }
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if device {
        name.insert(0, '_');
    }
    Some(name)
}

/// One snippet run in progress.
pub(crate) struct SnippetRun {
    pub id: u64,
    /// What the user started, for the status line and the toasts.
    pub label: String,
    /// The pane everything goes to and is read from.
    pub pane_id: Uuid,
    pub steps: Vec<SnippetStep>,
    /// Index of the next step to run.
    pub next: usize,
    /// Bumped at every wait: a timer or a reply carrying an older value
    /// belongs to a wait that is over, and is dropped.
    pub generation: u64,
    pub wait: Option<RunWait>,
    /// The wait's timer, aborted when the wait ends or the run is
    /// dropped: a 60 s timeout must not keep sleeping after the command
    /// it guarded has ended.
    pub timer: Option<iced::task::Handle>,
    /// A command was sent since the last wait: a command-end wait has
    /// something to wait for even before the shell marks it running.
    pub sent_command: bool,
}

/// What a run is waiting for.
pub(crate) enum RunWait {
    /// The shell's end-of-command mark (OSC 133 D); `true` = a failure
    /// does not stop the run.
    CommandEnd { continue_on_error: bool },
    /// A pattern in the pane's output.
    Output(OutputMatcher),
    /// A timer.
    Timer,
    /// A save step's write.
    File,
    /// The Save Output dialog an action step opened.
    Dialog,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snip(label: &str, steps: Vec<SnippetStep>) -> Snippet {
        let mut s = Snippet::new(label, "");
        s.set_actions(steps);
        s
    }

    fn cmd(t: &str) -> SnippetStep {
        SnippetStep::Command { text: t.into() }
    }

    #[test]
    fn a_nested_snippet_is_expanded_in_place() {
        let inner = snip("inner", vec![cmd("b"), cmd("c")]);
        let outer = snip("outer", vec![cmd("a"), SnippetStep::RunSnippet { id: inner.id }, cmd("d")]);
        let all = vec![inner.clone(), outer.clone()];
        assert_eq!(flatten(&all, &outer).unwrap(), vec![cmd("a"), cmd("b"), cmd("c"), cmd("d")]);
    }

    #[test]
    fn a_snippet_that_reaches_itself_is_refused() {
        let mut a = snip("a", vec![cmd("x")]);
        let b = snip("b", vec![SnippetStep::RunSnippet { id: a.id }]);
        a.set_actions(vec![cmd("x"), SnippetStep::RunSnippet { id: b.id }]);
        let all = vec![a.clone(), b.clone()];
        assert_eq!(flatten(&all, &a), Err(PlanError::Cycle("a".into())));
        assert_eq!(would_cycle(&all, b.id, "b", &b.steps), Some("b".into()));
        // The same snippet twice in a row is a repeat, not a cycle.
        let c = snip("c", vec![SnippetStep::RunSnippet { id: b.id }, SnippetStep::RunSnippet { id: b.id }]);
        let leaf = snip("leaf", vec![cmd("y")]);
        let d = snip("d", vec![SnippetStep::RunSnippet { id: leaf.id }, SnippetStep::RunSnippet { id: leaf.id }]);
        let all = vec![leaf, d.clone(), c];
        assert_eq!(flatten(&all, &d).unwrap(), vec![cmd("y"), cmd("y")]);
    }

    #[test]
    fn a_missing_snippet_stops_the_plan() {
        let a = snip("a", vec![cmd("x"), SnippetStep::RunSnippet { id: Uuid::new_v4() }]);
        assert_eq!(flatten(std::slice::from_ref(&a), &a), Err(PlanError::Missing));
    }

    #[test]
    fn only_pane_actions_may_run() {
        assert_eq!(step_action("terminal_clear_scrollback"), Some(HotkeyAction::TerminalClearScrollback));
        assert_eq!(step_action("lock_vault"), None);
        assert_eq!(step_action("close_all_tabs"), None);
        assert_eq!(step_action("from_a_newer_build"), None);
        assert_eq!(allowed_step_actions().len(), 4);
    }

    #[test]
    fn variables_are_asked_once_across_commands() {
        let steps = vec![cmd("deploy {env:prod}"), cmd("tail {env} {lines:100}")];
        assert_eq!(
            plan_placeholders(&steps),
            vec![("env".into(), "prod".into()), ("lines".into(), "100".into())]
        );
        let filled = substitute(steps, &[("env".into(), "qa".into()), ("lines".into(), "5".into())]);
        assert_eq!(filled, vec![cmd("deploy qa"), cmd("tail qa 5")]);
    }

    #[test]
    fn a_save_name_never_leaves_the_folder() {
        assert_eq!(save_file_name("{host}-{date}.log", "db-01", "2026-10-14").as_deref(), Some("db-01-2026-10-14.log"));
        assert_eq!(save_file_name("../../.bashrc", "h", "d").as_deref(), Some("_._.._.bashrc"));
        assert_eq!(save_file_name("/etc/passwd", "h", "d").as_deref(), Some("_etc_passwd"));
        assert_eq!(save_file_name("C:\\Windows\\x.txt", "h", "d").as_deref(), Some("C__Windows_x.txt"));
        assert_eq!(save_file_name("-rf", "h", "d").as_deref(), Some("_rf"));
        assert_eq!(save_file_name("..", "h", "d"), None);
        assert_eq!(save_file_name("nul.txt", "h", "d").as_deref(), Some("_nul.txt"));
        assert_eq!(save_file_name("COM1", "h", "d").as_deref(), Some("_COM1"));
        assert_eq!(save_file_name("console.log", "h", "d").as_deref(), Some("console.log"));
        assert_eq!(save_file_name("out. . ", "h", "d").as_deref(), Some("out"));
        assert_eq!(save_file_name("   ", "h", "d"), None);
    }
}

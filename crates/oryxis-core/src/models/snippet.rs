use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::login_script::ExpectPattern;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub id: Uuid,
    pub label: String,
    pub command: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// Optional free-form group name (snippet "folder"). Name-based on
    /// purpose: it rides sync and portable export as plain data with
    /// no new entity type, and the UIs derive the section list from
    /// the snippets themselves.
    #[serde(default)]
    pub group: Option<String>,
    /// Optional custom hotkey that runs this snippet in a focused
    /// terminal, stored in the app's serialized binding format
    /// ("ctrl+shift+k"). Lives on the snippet itself so deleting the
    /// snippet deletes the shortcut with it, by construction.
    #[serde(default)]
    pub hotkey: Option<String>,
    /// Install script (issue #147): a one-time host setup rather than a
    /// command run often. Drives its own affordances (a confirmation
    /// showing the full body before anything is sent, and the per-host
    /// "already ran here" memory); `#[serde(default)]` so snippets from
    /// older peers and exports read as ordinary commands.
    #[serde(default)]
    pub install: bool,
    /// The snippet's actions, in order. EMPTY for the snippet of old,
    /// whose one action is "run `command`": a single-command snippet is
    /// always stored that way (see [`Snippet::set_actions`]), so most rows
    /// never carry the field and an older reader of an export sees what it
    /// always saw. With more than one action (or any action that is not a
    /// command), `command` is a MIRROR: the command texts joined by lines,
    /// what search, previews and an older build read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<SnippetStep>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Snippet {
    pub fn new(label: impl Into<String>, command: impl Into<String>) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4(),
            label: label.into(),
            command: command.into(),
            description: None,
            tags: Vec::new(),
            group: None,
            hotkey: None,
            install: false,
            steps: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

impl Snippet {
    /// What the snippet does, in order: its steps, or the one command a
    /// snippet of old is.
    pub fn actions(&self) -> Vec<SnippetStep> {
        if self.steps.is_empty() {
            vec![SnippetStep::Command { text: self.command.clone() }]
        } else {
            self.steps.clone()
        }
    }

    /// More than the one command a snippet of old is: what the cards mark,
    /// and what the paste, install and fast paths step aside for.
    pub fn has_steps(&self) -> bool {
        !self.steps.is_empty()
    }

    /// Store `actions`. A lone command is kept the old way (`command`, no
    /// steps); anything else keeps the steps and mirrors their command
    /// texts into `command`.
    pub fn set_actions(&mut self, actions: Vec<SnippetStep>) {
        match actions.as_slice() {
            [SnippetStep::Command { text }] => {
                self.command = text.clone();
                self.steps = Vec::new();
            }
            _ => {
                self.command = command_mirror(&actions);
                self.steps = actions;
            }
        }
    }
}

/// The command texts of `steps`, one per line.
pub fn command_mirror(steps: &[SnippetStep]) -> String {
    steps
        .iter()
        .filter_map(|s| match s {
            SnippetStep::Command { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One action of a snippet.
///
/// On the wire each is an object with a `kind` in text. A kind this build
/// does not know (a newer Oryxis wrote it), or a known kind it cannot
/// read, is kept as [`SnippetStep::Unknown`] and written back exactly as it
/// came: synced and exported snippets lose nothing passing through an
/// older build, and the runner refuses to run what it cannot read.
#[derive(Debug, Clone, PartialEq)]
pub enum SnippetStep {
    /// Text typed into the shell and run: the snippet of old.
    Command { text: String },
    /// One of the app's pane actions, by its action id. Which ids may run
    /// is the app's decision, not the data's.
    Action { id: String },
    /// Hold the run until something happens.
    Wait(SnippetWait),
    /// Run another snippet's actions at this point.
    RunSnippet { id: Uuid },
    /// Write the pane's text to a file in the output folder.
    SaveFile(SaveFileStep),
    /// A step this build cannot read, kept verbatim.
    Unknown(serde_json::Value),
}

/// What a wait step waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "until", rename_all = "snake_case")]
pub enum SnippetWait {
    /// The command before it ended, as the shell marks it (OSC 133).
    CommandEnd {
        /// `0` = [`DEFAULT_WAIT_TIMEOUT_MS`].
        #[serde(default)]
        timeout_ms: u32,
        /// Go on when the command fails (the shell reports a non-zero
        /// exit). Off, a failure stops the run, as `&&` would.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        continue_on_error: bool,
    },
    /// A pattern in the output, matched like a login script's prompt.
    Output {
        pattern: ExpectPattern,
        /// `0` = [`DEFAULT_WAIT_TIMEOUT_MS`].
        #[serde(default)]
        timeout_ms: u32,
    },
    /// A fixed time.
    Time { ms: u32 },
}

/// How long a wait that names no timeout lasts.
pub const DEFAULT_WAIT_TIMEOUT_MS: u32 = 60_000;

/// Where a save step writes and what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveFileStep {
    /// File name inside the output folder; `{host}` and `{date}` expand.
    /// Never a path: the folder is the device's choice, not the data's.
    pub name: String,
    pub mode: SaveFileMode,
    pub content: SaveFileContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveFileMode {
    /// Add to the end of the file, creating it the first time.
    Append,
    /// A new file each run: an existing name gets `-1`, `-2`, ...
    NewFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveFileContent {
    /// The screen as drawn.
    Screen,
    /// Scrollback and screen.
    Buffer,
    /// The last command and its output (OSC 133 marks).
    LastCommand,
}

/// The wire shape of the kinds this build knows.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum KnownStep {
    Command { text: String },
    Action { id: String },
    Wait { wait: SnippetWait },
    RunSnippet { id: Uuid },
    SaveFile { file: SaveFileStep },
}

impl Serialize for SnippetStep {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let known = match self {
            SnippetStep::Command { text } => KnownStep::Command { text: text.clone() },
            SnippetStep::Action { id } => KnownStep::Action { id: id.clone() },
            SnippetStep::Wait(wait) => KnownStep::Wait { wait: wait.clone() },
            SnippetStep::RunSnippet { id } => KnownStep::RunSnippet { id: *id },
            SnippetStep::SaveFile(file) => KnownStep::SaveFile { file: file.clone() },
            SnippetStep::Unknown(raw) => return raw.serialize(serializer),
        };
        known.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SnippetStep {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(deserializer)?;
        Ok(match serde_json::from_value::<KnownStep>(raw.clone()) {
            Ok(KnownStep::Command { text }) => SnippetStep::Command { text },
            Ok(KnownStep::Action { id }) => SnippetStep::Action { id },
            Ok(KnownStep::Wait { wait }) => SnippetStep::Wait(wait),
            Ok(KnownStep::RunSnippet { id }) => SnippetStep::RunSnippet { id },
            Ok(KnownStep::SaveFile { file }) => SnippetStep::SaveFile(file),
            Err(_) => SnippetStep::Unknown(raw),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps() -> Vec<SnippetStep> {
        vec![
            SnippetStep::Command { text: "ls -la".into() },
            SnippetStep::Wait(SnippetWait::CommandEnd { timeout_ms: 0, continue_on_error: false }),
            SnippetStep::Action { id: "terminal_copy_last_command".into() },
            SnippetStep::Wait(SnippetWait::Output {
                pattern: ExpectPattern::Suffix("$ ".into()),
                timeout_ms: 5_000,
            }),
            SnippetStep::Wait(SnippetWait::Time { ms: 500 }),
            SnippetStep::RunSnippet { id: Uuid::nil() },
            SnippetStep::SaveFile(SaveFileStep {
                name: "{host}-{date}.log".into(),
                mode: SaveFileMode::Append,
                content: SaveFileContent::LastCommand,
            }),
            SnippetStep::Command { text: "echo done".into() },
        ]
    }

    #[test]
    fn steps_round_trip() {
        let mut snip = Snippet::new("deploy", "");
        snip.set_actions(steps());
        let json = serde_json::to_string(&snip).unwrap();
        let back: Snippet = serde_json::from_str(&json).unwrap();
        assert_eq!(back.steps, steps());
        assert_eq!(back.command, "ls -la\necho done");
    }

    #[test]
    fn a_step_from_a_newer_build_survives_a_round_trip() {
        let json = r#"{"kind":"ask_the_oracle","question":"why","level":3}"#;
        let step: SnippetStep = serde_json::from_str(json).unwrap();
        assert!(matches!(step, SnippetStep::Unknown(_)));
        let again: serde_json::Value = serde_json::to_value(&step).unwrap();
        assert_eq!(again, serde_json::from_str::<serde_json::Value>(json).unwrap());
        // A known kind with a shape this build cannot read is kept too.
        let odd = r#"{"kind":"wait","wait":{"until":"the_heat_death"}}"#;
        assert!(matches!(serde_json::from_str::<SnippetStep>(odd).unwrap(), SnippetStep::Unknown(_)));
    }

    #[test]
    fn a_lone_command_is_stored_the_old_way() {
        let mut snip = Snippet::new("ls", "old");
        snip.set_actions(vec![SnippetStep::Command { text: "ls -la".into() }]);
        assert!(snip.steps.is_empty());
        assert_eq!(snip.command, "ls -la");
        assert!(!snip.has_steps());
        let json = serde_json::to_value(&snip).unwrap();
        assert!(json.get("steps").is_none(), "an old reader sees the old shape");
    }

    #[test]
    fn a_snippet_of_old_is_one_command() {
        let json = r#"{"id":"00000000-0000-0000-0000-000000000000","label":"x","command":"uptime","description":null,"tags":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#;
        let snip: Snippet = serde_json::from_str(json).unwrap();
        assert_eq!(snip.actions(), vec![SnippetStep::Command { text: "uptime".into() }]);
    }
}

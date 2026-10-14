//! Built-in install scripts (issue #147): the one-time host setups our
//! own docs walk through by hand, shipped as editable snippet presets.
//!
//! Every preset is a self-contained POSIX script the USER fires from
//! the snippets surfaces; the app never types into a shell or writes a
//! dotfile on its own (the standing rule since the OSC 7 injection was
//! removed). An install snippet's Run always parks the careful-paste
//! confirmation first, so the full body is on screen before a byte is
//! sent.
//!
//! The scripts are built around the SAME resource files the docs quote
//! (`resources/shell-integration.sh`, `resources/osc7.sh`, both pinned
//! to their doc by test), wrapped in an idempotent installer: write the
//! file under `~/.config/oryxis/`, add the rc line only when absent.
//! The shell-integration preset keeps the `__ORYXIS_NONCE__`
//! placeholder in its body; the snippet injection path substitutes the
//! vault's CURRENT key at run time, so rotating the key never leaves a
//! stale copy of it inside the preset.
//!
//! Preset ids are FIXED (not random): two devices of one sync group
//! each seeding their own vault mint the same rows, so the pair
//! converges to one copy instead of duplicating. The seeded timestamps
//! are fixed and old for the same reason: a user's later edit (or
//! delete) always carries the newer timestamp and wins the LWW merge,
//! so seeding on a second device can never resurrect what the first
//! one changed.

use oryxis_core::models::Snippet;
use uuid::Uuid;

/// The osc7.sh body, kept as a file so `docs/CWD.md` can quote the same
/// bytes (pinned by `osc7_matches_the_documented_one`).
const OSC7_TEMPLATE: &str = include_str!("../../../resources/osc7.sh");

/// Fixed ids (see the module docs for why they are not random).
const SHELL_INTEGRATION_ID: &str = "5a1c9f00-0000-4000-8000-000000000147";
const OSC7_ID: &str = "5a1c9f00-0000-4000-8000-000000000247";
const TMUX_PASSTHROUGH_ID: &str = "5a1c9f00-0000-4000-8000-000000000347";

/// Fixed seed timestamp, safely older than any live edit.
fn seeded_at() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("constant timestamp")
        .with_timezone(&chrono::Utc)
}

/// Wrap a script body in the idempotent installer: write it to `path`
/// (heredoc, quoted delimiter so the host shell expands nothing) and
/// source it from `~/.bashrc` / `~/.zshrc` when the line is not there
/// yet. `grep -qs` keeps a missing rc file from failing the script;
/// the zsh line is only added when a `~/.zshrc` exists at all.
fn installer(file: &str, body: &str) -> String {
    let body = body.trim_end_matches('\n');
    format!(
        "mkdir -p ~/.config/oryxis && cat > ~/.config/oryxis/{file} <<'ORYXIS_INSTALL_EOF'\n\
         {body}\n\
         ORYXIS_INSTALL_EOF\n\
         grep -qs 'oryxis/{file}' ~/.bashrc || printf '\\n[ -f ~/.config/oryxis/{file} ] && . ~/.config/oryxis/{file}\\n' >> ~/.bashrc\n\
         [ -f ~/.zshrc ] && {{ grep -qs 'oryxis/{file}' ~/.zshrc || printf '\\n[ -f ~/.config/oryxis/{file} ] && . ~/.config/oryxis/{file}\\n' >> ~/.zshrc; }}\n\
         echo 'oryxis: {file} installed; open a new shell (or source your rc) to activate.'"
    )
}

/// The three shipped presets. Bodies are plain data the user can edit
/// or delete; labels and descriptions are data too (they live in the
/// vault, not in the i18n tables).
pub(crate) fn presets(shell_integration_body: &str) -> Vec<Snippet> {
    let ts = seeded_at();
    let mk = |id: &str, label: &str, desc: &str, command: String| Snippet {
        id: Uuid::parse_str(id).expect("constant uuid"),
        label: label.to_string(),
        command,
        description: Some(desc.to_string()),
        tags: Vec::new(),
        group: None,
        hotkey: None,
        install: true,
        steps: Vec::new(),
        created_at: ts,
        updated_at: ts,
    };
    vec![
        mk(
            SHELL_INTEGRATION_ID,
            "Shell integration (command marks)",
            "Installs the OSC 133/633 prompt marks that command history, \
             smart tabs and per-command transcripts feed on (bash + zsh). \
             __ORYXIS_NONCE__ is replaced with this vault's current \
             integration key when the script runs.",
            installer("shell-integration.sh", shell_integration_body),
        ),
        mk(
            OSC7_ID,
            "Working-directory reporting (OSC 7)",
            "Makes the shell report its working directory so the file \
             browser follows cd exactly (bash + zsh). The same snippet \
             helps every OSC 7-aware terminal open new tabs in the right \
             directory.",
            installer("osc7.sh", OSC7_TEMPLATE),
        ),
        mk(
            TMUX_PASSTHROUGH_ID,
            "tmux passthrough for Oryxis marks",
            "Lets tmux 3.3+ pass the integration sequences through to the \
             terminal (allow-passthrough), in ~/.tmux.conf and on the \
             running server. Without it the marks stop inside tmux.",
            "grep -qs 'allow-passthrough' ~/.tmux.conf || printf '\\nset -g allow-passthrough on\\n' >> ~/.tmux.conf\n\
             tmux set -g allow-passthrough on 2>/dev/null || true\n\
             echo 'oryxis: tmux passthrough enabled.'"
                .to_string(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `docs/CWD.md` shows the osc7 snippet inline; two copies drift, so
    /// this pins them, exactly like the shell-integration template's own
    /// doc test.
    #[test]
    fn osc7_matches_the_documented_one() {
        const DOC: &str = include_str!("../../../docs/CWD.md");
        let block = DOC
            .split("```sh")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .expect("docs/CWD.md must carry the osc7 snippet in a ```sh block");
        let normalize = |s: &str| s.replace("\r\n", "\n").trim().to_string();
        assert_eq!(
            normalize(block),
            normalize(OSC7_TEMPLATE),
            "docs/CWD.md and resources/osc7.sh drifted apart"
        );
    }

    #[test]
    fn presets_are_install_snippets_with_fixed_identity() {
        let a = presets("body-a");
        let b = presets("body-b");
        assert_eq!(a.len(), 3);
        for (x, y) in a.iter().zip(&b) {
            assert!(x.install, "{} must be an install script", x.label);
            // Fixed ids + fixed timestamps: the sync convergence story.
            assert_eq!(x.id, y.id);
            assert_eq!(x.created_at, x.updated_at);
            assert_eq!(x.created_at, seeded_at());
        }
    }

    #[test]
    fn the_shell_integration_preset_keeps_the_placeholder() {
        // The body embeds whatever the caller passes; seeding passes the
        // TEMPLATE (placeholder intact) so the key is resolved at run
        // time, never frozen into the vault.
        let p = presets("key=__ORYXIS_NONCE__");
        assert!(p[0].command.contains("__ORYXIS_NONCE__"));
    }

    /// The installer must survive the heredoc: a body line that matches
    /// the delimiter would end the write early and hand the rest to the
    /// host shell as commands.
    #[test]
    fn installer_bodies_cannot_break_the_heredoc() {
        for body in [
            include_str!("../../../resources/shell-integration.sh"),
            OSC7_TEMPLATE,
        ] {
            assert!(
                !body.contains("ORYXIS_INSTALL_EOF"),
                "script body collides with the heredoc delimiter"
            );
        }
    }

    #[test]
    fn installers_are_idempotent_by_construction() {
        // Every rc append is guarded by a grep for the same needle, so
        // running the script twice cannot stack duplicate source lines.
        for p in presets("x") {
            for line in p.command.lines().filter(|l| l.contains(">>")) {
                assert!(
                    line.contains("grep -qs"),
                    "unguarded append in {}: {line}",
                    p.label
                );
            }
        }
    }
}

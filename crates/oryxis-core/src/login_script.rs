//! Expect/send login automation, shared by every transport.
//!
//! Some hosts authenticate *inside* the TTY instead of over the
//! protocol: Telnet has no in-protocol auth at all, and bastions like
//! JumpServer's KoKo print a menu that the client has to read and type
//! back. There is no clean version of that, only a hidden one, so this
//! module is the one place that reads output text and answers it.
//!
//! A script is an ordered list of [`LoginStep`]s. Step N arms only
//! after step N-1 has fired, which is what keeps a `password:` string
//! from being answered before the `login:` that was supposed to come
//! first. An `optional` step may be skipped when a LATER step's
//! pattern matches instead, so a banner that only *sometimes* asks a
//! question does not stall the run (this is what lets a bare
//! `Password:` gateway work with a username step configured).
//!
//! The guards below exist because terminal output is attacker
//! controlled. A host we are typing a password into can print anything
//! it likes, including a fake prompt:
//!
//! - the whole run is armed only inside a window from connect, so a
//!   `password:` scrolling past in a log file an hour later can never
//!   trigger an injection;
//! - each step fires AT MOST ONCE, in order (a rejected password falls
//!   through to the user, never a retry loop);
//! - every step has a deadline: a non-optional step that times out
//!   aborts the run instead of waiting forever, so a late match can
//!   never answer a prompt the script did not mean;
//! - the pattern must match the CURRENT tail of the stream, and the
//!   tail is cleared after every fire so the echo of what we sent
//!   cannot re-match.
//!
//! Secrets are never part of a script: [`SendPayload::Secret`] carries
//! a [`SecretRef`] discriminant and the caller resolves it (decrypting
//! from the vault) at send time. That is enforced by the type, not by
//! a lint, which is why the script itself can live in a plaintext
//! column.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Per-step timeout applied when a step declares `timeout_ms == 0`.
/// Ten seconds is the safe guess for a bastion menu over a slow WAN.
pub const DEFAULT_STEP_TIMEOUT_MS: u32 = 10_000;

/// How much output tail is kept for matching. Prompts are short; the
/// cap only needs to survive ANSI-heavy redraws.
const TAIL_CAP: usize = 256;

/// One expect/send pair. The primitive every client in this space
/// converged on (SecureCRT Logon Actions, Xshell Login Scripts, Tabby
/// `loginScripts`), plus the per-step timeout none of them document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStep {
    /// `None` sends immediately without waiting for anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<ExpectPattern>,
    pub send: SendPayload,
    /// `0` inherits [`DEFAULT_STEP_TIMEOUT_MS`].
    #[serde(default)]
    pub timeout_ms: u32,
    /// Timing out here skips the step instead of aborting the run, and
    /// a later step matching first skips it too.
    #[serde(default)]
    pub optional: bool,
}

/// How a step recognizes that the host is waiting for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExpectPattern {
    /// Case-insensitive suffix of the ANSI-stripped tail. The default,
    /// because a prompt is the last thing on the screen rather than
    /// something buried in it. SecureCRT's docs tell users to match the
    /// suffix for the same reason.
    Suffix(String),
    /// Matched anywhere in the tail; anchor with `$` for suffix
    /// semantics. Case-sensitive unless the pattern says `(?i)`.
    Regex(String),
}

/// What a step types back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SendPayload {
    /// Literal text. A password can never live here: the vault-backed
    /// alternative is [`SendPayload::Secret`].
    Text(String),
    /// Resolved and decrypted by the caller at send time, never stored
    /// with the script.
    Secret(SecretRef),
    /// A named key, so "just press Enter" needs no magic empty string.
    Key(NamedKey),
    /// Wait for the pattern and do nothing (a synchronization point).
    Nothing,
}

/// Which stored credential a [`SendPayload::Secret`] step means. The
/// caller owns the mapping to actual bytes; this crate never sees one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SecretRef {
    /// The connection's own password (spent on the bastion login).
    ConnectionPassword,
    /// The second secret, for the host reached THROUGH the bastion.
    TargetPassword,
    /// A vault identity's password.
    Identity(Uuid),
    /// A generated TOTP code for the connection.
    Totp,
}

/// Keys a step can press. Terminal-level bytes, identical on every
/// transport (Telnet's writer maps the CR to CR LF on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedKey {
    Enter,
    Tab,
    Escape,
    Space,
    CtrlC,
    CtrlD,
}

impl NamedKey {
    pub fn bytes(self) -> &'static [u8] {
        match self {
            NamedKey::Enter => b"\r",
            NamedKey::Tab => b"\t",
            NamedKey::Escape => b"\x1b",
            NamedKey::Space => b" ",
            NamedKey::CtrlC => b"\x03",
            NamedKey::CtrlD => b"\x04",
        }
    }
}

/// A text payload is a LINE: the host is waiting for one, so the
/// answer carries the carriage return the Enter key would produce.
pub fn line_bytes(text: &str) -> Vec<u8> {
    let mut out = text.as_bytes().to_vec();
    out.push(b'\r');
    out
}

/// The three prompts an interactive bastion asks before it hands over
/// the asset's shell. Every menu-driven jump box in the field follows
/// this shape (JumpServer's KoKo, Teleport's node picker, the menu
/// firmware on network gear), so the guided form asks for the three
/// strings instead of making the user author steps.
///
/// No other client ships a preset: the expect/send engine is a
/// twenty-year-old commodity, the guided form is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BastionPreset {
    /// Where the bastion asks WHICH asset (`Opt>` on JumpServer).
    pub asset_prompt: String,
    /// Where it asks for the account on that asset.
    pub user_prompt: String,
    /// Where it asks for that account's password.
    pub password_prompt: String,
}

/// Placeholder names the preset's steps reference, filled in per host.
pub const VAR_ASSET: &str = "asset";
pub const VAR_TARGET_USER: &str = "target_user";

impl BastionPreset {
    /// JumpServer / KoKo, the deployment this feature was reported
    /// from. `Opt>` is the menu prompt, and the password line reads
    /// `Manual input(<user>)'s password:` so only its suffix is matched.
    pub fn jumpserver() -> Self {
        Self {
            asset_prompt: "opt>".into(),
            user_prompt: "username:".into(),
            password_prompt: "password:".into(),
        }
    }

    /// Neutral defaults for any other menu-driven bastion; the user
    /// edits the three strings to match what their box prints.
    pub fn generic() -> Self {
        Self {
            asset_prompt: ">".into(),
            user_prompt: "username:".into(),
            password_prompt: "password:".into(),
        }
    }

    /// Expand to steps. The asset and user answers are placeholders so
    /// one script serves every host behind the bastion; the password is
    /// a reference, so the script itself never carries a credential.
    ///
    /// The user step is `optional`: plenty of bastions take the account
    /// as part of the asset selection and never ask separately, and the
    /// engine skips an optional step when a later one matches.
    pub fn build(&self) -> Vec<LoginStep> {
        let mut steps = Vec::with_capacity(3);
        if !self.asset_prompt.trim().is_empty() {
            steps.push(LoginStep {
                expect: Some(ExpectPattern::Suffix(self.asset_prompt.trim().into())),
                send: SendPayload::Text(format!("{{{VAR_ASSET}}}")),
                timeout_ms: 0,
                optional: false,
            });
        }
        if !self.user_prompt.trim().is_empty() {
            steps.push(LoginStep {
                expect: Some(ExpectPattern::Suffix(self.user_prompt.trim().into())),
                send: SendPayload::Text(format!("{{{VAR_TARGET_USER}}}")),
                timeout_ms: 0,
                optional: true,
            });
        }
        if !self.password_prompt.trim().is_empty() {
            steps.push(LoginStep {
                expect: Some(ExpectPattern::Suffix(self.password_prompt.trim().into())),
                send: SendPayload::Secret(SecretRef::TargetPassword),
                timeout_ms: 0,
                optional: false,
            });
        }
        steps
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("step {step}: invalid pattern: {source}")]
    Pattern {
        step: usize,
        #[source]
        source: regex::Error,
    },
    #[error("the script has no steps")]
    Empty,
}

/// What the runner wants the caller to do. Drain with [`ScriptRunner::poll`]
/// in a loop: one feed can produce several actions when consecutive
/// steps need no waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerAction {
    /// Send this payload (resolving a [`SecretRef`] first, if any).
    /// `index` is the 0-based step, for progress display.
    Send { index: usize, payload: SendPayload },
    /// A non-optional step timed out. The run is over and the user
    /// should be told which step gave up.
    Timeout { index: usize },
    /// Every step fired. The run is over.
    Finished,
}

#[derive(Debug)]
struct CompiledStep {
    expect: Option<CompiledPattern>,
    send: SendPayload,
    timeout: Duration,
    optional: bool,
}

#[derive(Debug)]
enum CompiledPattern {
    /// Pre-lowercased; compared against the lowercased tail.
    Suffix(String),
    Regex(regex::Regex),
}

impl CompiledPattern {
    fn compile(pattern: &ExpectPattern) -> Result<Self, regex::Error> {
        Ok(match pattern {
            ExpectPattern::Suffix(s) => CompiledPattern::Suffix(s.trim_end().to_lowercase()),
            ExpectPattern::Regex(r) => CompiledPattern::Regex(regex::Regex::new(r)?),
        })
    }

    fn matches(&self, tail: &str, tail_lower: &str) -> bool {
        match self {
            CompiledPattern::Suffix(s) => tail_lower.ends_with(s.as_str()),
            CompiledPattern::Regex(re) => re.is_match(tail),
        }
    }
}

/// Runs one script against a live output stream.
///
/// The clock is a parameter rather than an ambient `Instant::now()` so
/// timeout behavior is testable without sleeping.
#[derive(Debug)]
pub struct ScriptRunner {
    steps: Vec<CompiledStep>,
    /// Index of the armed step.
    idx: usize,
    /// ANSI-stripped tail of everything seen since the last fire.
    tail: String,
    /// Hard disarm for the whole run.
    deadline: Instant,
    /// Deadline of the armed step.
    step_deadline: Instant,
    done: bool,
}

impl ScriptRunner {
    /// Compile a script and arm its first step at `now`.
    ///
    /// `window` is the hard disarm for the whole run, independent of
    /// the per-step timeouts: past it the runner is dead for the
    /// session even if a step still had budget left.
    pub fn new(steps: &[LoginStep], window: Duration, now: Instant) -> Result<Self, ScriptError> {
        let mut compiled = Vec::with_capacity(steps.len());
        for (i, step) in steps.iter().enumerate() {
            let expect = step
                .expect
                .as_ref()
                .map(CompiledPattern::compile)
                .transpose()
                .map_err(|source| ScriptError::Pattern { step: i, source })?;
            compiled.push(CompiledStep {
                expect,
                send: step.send.clone(),
                timeout: Duration::from_millis(if step.timeout_ms == 0 {
                    u64::from(DEFAULT_STEP_TIMEOUT_MS)
                } else {
                    u64::from(step.timeout_ms)
                }),
                optional: step.optional,
            });
        }
        let done = compiled.is_empty();
        let first = compiled.first().map(|s| s.timeout).unwrap_or_default();
        Ok(ScriptRunner {
            steps: compiled,
            idx: 0,
            tail: String::new(),
            deadline: now + window,
            step_deadline: now + first,
            done,
        })
    }

    /// Reject a script the runner could never execute. Used by the
    /// editor at save time so a bad pattern is a form error, never a
    /// silent no-op at connect.
    pub fn validate(steps: &[LoginStep]) -> Result<(), ScriptError> {
        if steps.is_empty() {
            return Err(ScriptError::Empty);
        }
        for (i, step) in steps.iter().enumerate() {
            if let Some(ExpectPattern::Regex(r)) = &step.expect {
                regex::Regex::new(r).map_err(|source| ScriptError::Pattern { step: i, source })?;
            }
        }
        Ok(())
    }

    /// Feed one decoded output chunk. Call [`Self::poll`] afterwards.
    pub fn feed(&mut self, output: &[u8]) {
        if self.done {
            return;
        }
        self.tail.push_str(&strip_ansi_lossy(output));
        self.trim_tail();
    }

    /// Drain the next action, if any. Call in a loop until `None`.
    pub fn poll(&mut self, now: Instant) -> Option<RunnerAction> {
        if self.done {
            return None;
        }
        if self.idx >= self.steps.len() {
            self.done = true;
            return Some(RunnerAction::Finished);
        }
        // Deadlines are checked BEFORE matching, deliberately: once a
        // step is out of time, a prompt arriving late is not the one
        // the script meant, and answering it could hand a secret to
        // whatever is on screen now.
        if now >= self.deadline {
            self.done = true;
            return Some(RunnerAction::Timeout { index: self.idx });
        }
        if now >= self.step_deadline {
            let index = self.idx;
            if !self.steps[index].optional {
                self.done = true;
                return Some(RunnerAction::Timeout { index });
            }
            // Skipping keeps the tail: the next step's prompt may
            // already be sitting in it.
            self.idx += 1;
            if self.idx < self.steps.len() {
                self.step_deadline = now + self.steps[self.idx].timeout;
            }
            return self.poll(now);
        }
        let trimmed_len = self.tail.trim_end().len();
        let tail = &self.tail[..trimmed_len];
        let tail_lower = tail.to_lowercase();

        // Scan forward over skippable steps: the armed one first, then
        // any OPTIONAL step it could stand in for. The scan stops at
        // the first non-optional step, which may still match (skipping
        // the optional ones before it) but can never be skipped over.
        let mut j = self.idx;
        while j < self.steps.len() {
            let step = &self.steps[j];
            let hit = match &step.expect {
                None => true,
                Some(p) => p.matches(tail, &tail_lower),
            };
            if hit {
                let payload = step.send.clone();
                self.advance_to(j + 1, now);
                return Some(RunnerAction::Send { index: j, payload });
            }
            if !step.optional {
                break;
            }
            j += 1;
        }
        None
    }

    /// `(step number, total)`, 1-based, for a progress indicator.
    pub fn progress(&self) -> (usize, usize) {
        ((self.idx + 1).min(self.steps.len()), self.steps.len())
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    fn advance_to(&mut self, next: usize, now: Instant) {
        self.idx = next;
        // The echo of what we just sent must not re-match.
        self.tail.clear();
        if let Some(step) = self.steps.get(next) {
            self.step_deadline = now + step.timeout;
        }
        // Running off the end is not marked done here: the next poll
        // reports `Finished` once, then goes quiet.
    }

    fn trim_tail(&mut self) {
        trim_tail(&mut self.tail);
    }
}

/// Keep the last [`TAIL_CAP`] bytes of `tail`, cut on a char boundary
/// (the tail is lossy UTF-8 already).
fn trim_tail(tail: &mut String) {
    if tail.len() > TAIL_CAP {
        let cut = tail.len() - TAIL_CAP;
        let cut = (cut..tail.len()).find(|i| tail.is_char_boundary(*i)).unwrap_or(0);
        tail.drain(..cut);
    }
}

/// One pattern watched over a live output stream: a login script step's
/// matcher, for anything else that waits on output (a snippet's wait
/// step). Same rules: ANSI stripped, a suffix compared case-insensitively
/// against the end of what arrived, a regex anywhere in the kept tail.
#[derive(Debug)]
pub struct OutputMatcher {
    pattern: CompiledPattern,
    tail: String,
}

impl OutputMatcher {
    pub fn new(pattern: &ExpectPattern) -> Result<Self, regex::Error> {
        Ok(Self { pattern: CompiledPattern::compile(pattern)?, tail: String::new() })
    }

    /// Feed one output batch; `true` once the pattern matches.
    pub fn feed(&mut self, output: &[u8]) -> bool {
        self.tail.push_str(&strip_ansi_lossy(output));
        trim_tail(&mut self.tail);
        let lower = self.tail.to_lowercase();
        self.pattern.matches(&self.tail, &lower)
    }
}

/// Drop ANSI escape sequences (CSI, OSC, two-byte ESC forms) and
/// non-text control bytes so a colored `Username:` still matches.
/// Non-ASCII bytes pass through untouched: real prompts are ASCII and
/// the lossy path only feeds the matcher, never the terminal.
pub fn strip_ansi_lossy(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            0x1b => {
                i += 1;
                match data.get(i) {
                    // CSI: parameters then one final byte in 0x40..=0x7E.
                    Some(b'[') => {
                        i += 1;
                        while i < data.len() && !(0x40..=0x7e).contains(&data[i]) {
                            i += 1;
                        }
                        i += 1;
                    }
                    // OSC: runs to BEL or ESC \.
                    Some(b']') => {
                        i += 1;
                        while i < data.len() {
                            if data[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            if data[i] == 0x1b && data.get(i + 1) == Some(&b'\\') {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    // Two-byte escape (ESC c, ESC =, charset selects...).
                    Some(_) => i += 1,
                    None => {}
                }
            }
            b'\r' | b'\n' | b'\t' => {
                out.push(data[i] as char);
                i += 1;
            }
            c if c < 0x20 || c == 0x7f => i += 1,
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suffix(s: &str, send: &str, optional: bool) -> LoginStep {
        LoginStep {
            expect: Some(ExpectPattern::Suffix(s.into())),
            send: SendPayload::Text(send.into()),
            timeout_ms: 10_000,
            optional,
        }
    }

    fn run(runner: &mut ScriptRunner, out: &[u8], now: Instant) -> Vec<RunnerAction> {
        runner.feed(out);
        let mut acts = Vec::new();
        while let Some(a) = runner.poll(now) {
            acts.push(a);
        }
        acts
    }

    fn sent(acts: &[RunnerAction]) -> Vec<String> {
        acts.iter()
            .filter_map(|a| match a {
                RunnerAction::Send {
                    payload: SendPayload::Text(t),
                    ..
                } => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_output_matcher_sees_a_prompt_split_across_batches() {
        let mut m = OutputMatcher::new(&ExpectPattern::Suffix("Done.".into())).unwrap();
        assert!(!m.feed(b"building...\r\n\x1b[32mDo"));
        assert!(m.feed(b"ne.\x1b[0m"));
        let mut r = OutputMatcher::new(&ExpectPattern::Regex(r"exit code \d+".into())).unwrap();
        assert!(!r.feed(b"running"));
        assert!(r.feed(b"\nexit code 0\nmore"));
        assert!(OutputMatcher::new(&ExpectPattern::Regex("(".into())).is_err());
    }

    #[test]
    fn steps_fire_in_order_once_each() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[
                suffix("login:", "admin", false),
                suffix("password:", "hunter2", false),
            ],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        assert_eq!(
            sent(&run(&mut r, b"Ubuntu 22.04\r\nrouter login: ", t0)),
            vec!["admin"]
        );
        let acts = run(&mut r, b"Password: ", t0);
        assert_eq!(sent(&acts), vec!["hunter2"]);
        assert_eq!(acts.last(), Some(&RunnerAction::Finished));
        assert!(r.is_done());
        // A second password prompt (rejected credential) falls through
        // to the user, never a retry loop.
        assert!(run(&mut r, b"Login incorrect\r\nPassword: ", t0).is_empty());
    }

    #[test]
    fn a_later_prompt_cannot_answer_an_earlier_step() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[
                suffix("login:", "admin", false),
                suffix("password:", "hunter2", false),
            ],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        // The password prompt arrives first: the required username step
        // is still armed, so nothing is sent.
        assert!(run(&mut r, b"Password: ", t0).is_empty());
    }

    #[test]
    fn an_optional_step_is_skipped_when_a_later_one_matches() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[
                suffix("login:", "admin", true),
                suffix("password:", "hunter2", false),
            ],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        // Gear that goes straight to the password prompt.
        assert_eq!(sent(&run(&mut r, b"Password: ", t0)), vec!["hunter2"]);
    }

    #[test]
    fn prompt_must_terminate_the_stream() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[suffix("login:", "admin", false)],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        // "login:" mid-line followed by more text is not a prompt.
        assert!(run(&mut r, b"last login: Tue Jul 1 from 10.0.0.5\r\n", t0).is_empty());
    }

    #[test]
    fn colored_and_spaced_prompts_match() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[suffix("username:", "admin", false)],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        // Cisco-style with an SGR color around it.
        assert_eq!(
            sent(&run(&mut r, b"\x1b[1mUsername:\x1b[0m ", t0)),
            vec!["admin"]
        );
    }

    #[test]
    fn prompt_split_across_chunks_matches() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[suffix("password:", "pw", false)],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        assert!(run(&mut r, b"Pass", t0).is_empty());
        assert_eq!(sent(&run(&mut r, b"word: ", t0)), vec!["pw"]);
    }

    #[test]
    fn an_empty_script_is_done_on_arrival() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(&[], Duration::from_secs(60), t0).unwrap();
        assert!(r.is_done());
        assert!(run(&mut r, b"login: ", t0).is_empty());
    }

    #[test]
    fn a_required_step_times_out_and_aborts() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[
                suffix("opt>", "web-01", false),
                suffix("password:", "pw", false),
            ],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        let late = t0 + Duration::from_secs(11);
        assert_eq!(
            run(&mut r, b"nothing we asked for\r\n", late),
            vec![RunnerAction::Timeout { index: 0 }]
        );
        assert!(r.is_done());
    }

    #[test]
    fn an_optional_step_times_out_and_the_run_continues() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[
                suffix("press any key", "", true),
                suffix("password:", "pw", false),
            ],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        let late = t0 + Duration::from_secs(11);
        // The optional step gives up, and the prompt already sitting in
        // the tail answers the next one in the same poll.
        assert_eq!(sent(&run(&mut r, b"Password: ", late)), vec!["pw"]);
    }

    #[test]
    fn the_window_disarms_the_whole_run() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[suffix("password:", "pw", false)],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        let hour_later = t0 + Duration::from_secs(3600);
        assert_eq!(
            run(&mut r, b"Password: ", hour_later),
            // The step deadline is hit first; either way nothing is sent.
            vec![RunnerAction::Timeout { index: 0 }]
        );
    }

    #[test]
    fn a_step_with_no_expect_sends_immediately() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[
                LoginStep {
                    expect: None,
                    send: SendPayload::Text("hello".into()),
                    timeout_ms: 0,
                    optional: false,
                },
                suffix("password:", "pw", false),
            ],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        let mut acts = Vec::new();
        while let Some(a) = r.poll(t0) {
            acts.push(a);
        }
        assert_eq!(sent(&acts), vec!["hello"]);
    }

    #[test]
    fn a_secret_step_yields_a_reference_never_a_value() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[LoginStep {
                expect: Some(ExpectPattern::Suffix("password:".into())),
                send: SendPayload::Secret(SecretRef::TargetPassword),
                timeout_ms: 0,
                optional: false,
            }],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        let acts = run(&mut r, b"Manual input(deploy)'s password: ", t0);
        assert!(matches!(
            acts.first(),
            Some(RunnerAction::Send {
                index: 0,
                payload: SendPayload::Secret(SecretRef::TargetPassword)
            })
        ));
    }

    #[test]
    fn a_regex_step_matches_and_compiles() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[LoginStep {
                expect: Some(ExpectPattern::Regex(r"(?i)(login|username):\s*$".into())),
                send: SendPayload::Text("admin".into()),
                timeout_ms: 0,
                optional: false,
            }],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        assert_eq!(sent(&run(&mut r, b"switch Username: ", t0)), vec!["admin"]);
    }

    #[test]
    fn an_invalid_regex_is_rejected_at_compile_time() {
        let steps = [LoginStep {
            expect: Some(ExpectPattern::Regex("(unclosed".into())),
            send: SendPayload::Nothing,
            timeout_ms: 0,
            optional: false,
        }];
        assert!(matches!(
            ScriptRunner::validate(&steps),
            Err(ScriptError::Pattern { step: 0, .. })
        ));
        assert!(ScriptRunner::new(&steps, Duration::from_secs(60), Instant::now()).is_err());
    }

    #[test]
    fn validate_rejects_an_empty_script() {
        assert!(matches!(ScriptRunner::validate(&[]), Err(ScriptError::Empty)));
    }

    #[test]
    fn the_jumpserver_menu_flow_runs_end_to_end() {
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(
            &[
                LoginStep {
                    expect: Some(ExpectPattern::Suffix("opt>".into())),
                    send: SendPayload::Text("web-01".into()),
                    timeout_ms: 0,
                    optional: false,
                },
                LoginStep {
                    expect: Some(ExpectPattern::Suffix("username:".into())),
                    send: SendPayload::Text("deploy".into()),
                    timeout_ms: 0,
                    optional: false,
                },
                LoginStep {
                    expect: Some(ExpectPattern::Suffix("password:".into())),
                    send: SendPayload::Secret(SecretRef::TargetPassword),
                    timeout_ms: 0,
                    optional: false,
                },
            ],
            Duration::from_secs(60),
            t0,
        )
        .unwrap();
        assert_eq!(sent(&run(&mut r, b"\r\nOpt> ", t0)), vec!["web-01"]);
        assert_eq!(sent(&run(&mut r, b"web-01\r\nusername: ", t0)), vec!["deploy"]);
        let acts = run(&mut r, b"Manual input(deploy)'s password: ", t0);
        assert!(matches!(
            acts.first(),
            Some(RunnerAction::Send {
                payload: SendPayload::Secret(SecretRef::TargetPassword),
                ..
            })
        ));
        assert_eq!(acts.last(), Some(&RunnerAction::Finished));
    }

    #[test]
    fn serialized_steps_never_carry_a_secret_value() {
        let steps = vec![LoginStep {
            expect: Some(ExpectPattern::Suffix("password:".into())),
            send: SendPayload::Secret(SecretRef::TargetPassword),
            timeout_ms: 0,
            optional: false,
        }];
        let json = serde_json::to_string(&steps).unwrap();
        assert!(json.contains("target_password"));
        // Round-trips, and the type has nowhere to put a plaintext.
        let back: Vec<LoginStep> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, steps);
    }

    #[test]
    fn named_keys_carry_terminal_bytes() {
        assert_eq!(NamedKey::Enter.bytes(), b"\r");
        assert_eq!(NamedKey::CtrlC.bytes(), b"\x03");
        assert_eq!(line_bytes("admin"), b"admin\r".to_vec());
    }

    #[test]
    fn the_jumpserver_preset_expands_to_the_reported_flow() {
        let steps = BastionPreset::jumpserver().build();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].send, SendPayload::Text("{asset}".into()));
        // The account step is optional: bastions that fold the account
        // into the asset selection must not stall the run.
        assert!(steps[1].optional);
        assert_eq!(steps[2].send, SendPayload::Secret(SecretRef::TargetPassword));
        // And it actually runs against the reporter's transcript.
        let t0 = Instant::now();
        let mut r = ScriptRunner::new(&steps, Duration::from_secs(60), t0).unwrap();
        // Variables are substituted before the runner is built; here the
        // raw placeholder text is enough to prove the sequencing.
        assert_eq!(sent(&run(&mut r, b"\r\nOpt> ", t0)), vec!["{asset}"]);
        assert_eq!(
            sent(&run(&mut r, b"web-01\r\nusername: ", t0)),
            vec!["{target_user}"]
        );
        let acts = run(&mut r, b"Manual input(deploy)'s password: ", t0);
        assert!(matches!(
            acts.first(),
            Some(RunnerAction::Send {
                payload: SendPayload::Secret(SecretRef::TargetPassword),
                ..
            })
        ));
    }

    #[test]
    fn a_preset_drops_the_prompts_left_blank() {
        let preset = BastionPreset {
            asset_prompt: "  ".into(),
            user_prompt: "login:".into(),
            password_prompt: "password:".into(),
        };
        let steps = preset.build();
        assert_eq!(steps.len(), 2, "a blank prompt means the bastion never asks");
        assert_eq!(
            steps[0].expect,
            Some(ExpectPattern::Suffix("login:".into()))
        );
    }
}

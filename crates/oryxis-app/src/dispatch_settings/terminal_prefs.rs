//! Settings dispatch helpers: terminal behavior preferences
//! (toggles, selection / paste, font, scrollback). Split out of
//! dispatch_settings/mod.rs.

use super::*;
use crate::terminal_appearance::bg_fit_label_key;

impl Oryxis {
    /// Terminal-preference arms: behavior toggles, bell / clipboard /
    /// notification modes, font size + family and scrollback.
    pub(super) fn handle_settings_terminal_prefs(
        &mut self,
        message: SettingsMessage,
    ) -> Result<Task<Message>, SettingsMessage> {
        match message {
            SettingsMessage::BellModeChanged(name) => {
                use crate::util::BellMode;
                if let Some(mode) = BellMode::ALL
                    .iter()
                    .find(|m| crate::i18n::t(m.label_key()) == name)
                {
                    self.prefs.bell_mode = *mode;
                    self.persist_setting("terminal_bell_mode", mode.code());
                }
            }
            SettingsMessage::ClipboardAccessChanged(name) => {
                use crate::util::ClipboardAccess;
                if let Some(mode) = ClipboardAccess::ALL
                    .iter()
                    .find(|m| crate::i18n::t(m.label_key()) == name)
                {
                    self.prefs.clipboard_access = *mode;
                    self.persist_setting("terminal_clipboard_access", mode.code());
                    let (cw, cr) = mode.flags();
                    oryxis_terminal::set_clipboard_access(cw, cr);
                }
            }
            SettingsMessage::NotificationModeChanged(name) => {
                use crate::util::NotificationMode;
                if let Some(mode) = NotificationMode::ALL
                    .iter()
                    .find(|m| crate::i18n::t(m.label_key()) == name)
                {
                    self.prefs.notification_mode = *mode;
                    self.persist_setting("terminal_notification", mode.code());
                }
            }
            SettingsMessage::SettingToggleSmartTabs => {
                self.prefs.smart_tabs = !self.prefs.smart_tabs;
                self.persist_setting(
                    "smart_tabs",
                    if self.prefs.smart_tabs { "true" } else { "false" },
                );
                // Turning it off retires any attention already raised;
                // stale dots surviving the toggle would contradict the
                // "all its UI hidden when off" rule.
                if !self.prefs.smart_tabs {
                    for tab in &mut self.tabs {
                        for pane in tab.pane_grid.panes.values_mut() {
                            pane.attention = None;
                            pane.running_cmd = None;
                            pane.last_submitted = None;
                        }
                    }
                }
            }
            SettingsMessage::ToggleConfirmCloseSessionTab => {
                self.prefs.confirm_close_session_tab = !self.prefs.confirm_close_session_tab;
                self.persist_setting(
                    "confirm_close_session_tab",
                    if self.prefs.confirm_close_session_tab { "true" } else { "false" },
                );
            }
            SettingsMessage::SmartTabsThresholdChanged(label) => {
                if let Some((secs, _)) = crate::smart_tabs::threshold_options()
                    .into_iter()
                    .find(|(_, l)| *l == label)
                {
                    self.prefs.smart_long_secs = secs;
                    self.persist_setting("smart_tabs_long_seconds", &secs.to_string());
                }
            }
            SettingsMessage::TerminalFontSizeIncrease => {
                self.terminal_font_size = (self.terminal_font_size + 1.0).min(24.0);
                self.normalize_terminal_font_zoom();
                self.persist_setting(
                    "terminal_font_size",
                    &format!("{}", self.terminal_font_size),
                );
            }
            SettingsMessage::TerminalFontSizeDecrease => {
                self.terminal_font_size = (self.terminal_font_size - 1.0).max(10.0);
                self.normalize_terminal_font_zoom();
                self.persist_setting(
                    "terminal_font_size",
                    &format!("{}", self.terminal_font_size),
                );
            }
            SettingsMessage::TerminalFontChanged(name) => {
                self.terminal_font_name = name;
                self.persist_setting("terminal_font_name", &self.terminal_font_name);
                // Picking a pack font (issue #109) pulls its file on
                // demand, once per session (guard contract of
                // `loaded_cjk_fonts`). A cached file loads silently;
                // a download shows a hint toast. Either way the font
                // registers via `PackFontReady`, live panes re-render
                // with it, no restart.
                if let Some(task) = self.ensure_pack_face() {
                    return Ok(task);
                }
            }
            SettingsMessage::TerminalFontWeightChanged(weight) => {
                self.terminal_font_weight = weight;
                self.persist_setting("terminal_font_weight", weight.setting_value());
                // A pack family keeps one file per weight, so the new
                // weight may be a face this machine has never fetched.
                if let Some(task) = self.ensure_pack_face() {
                    return Ok(task);
                }
            }
            SettingsMessage::TerminalTextThicknessChanged(thickness) => {
                self.terminal_text_thickness = thickness;
                self.persist_setting(
                    "terminal_text_thickness",
                    thickness.setting_value(),
                );
            }
            SettingsMessage::PackFontReady(key, result) => match result {
                Ok(bytes) => {
                    // Clear the "downloading" hint and register the
                    // font with the iced font system; the terminal
                    // widget resolves the family by name per frame, so
                    // the picked font applies as soon as the load
                    // lands. `iced::font::Error` is uninhabited, so
                    // the load result is discarded.
                    self.toast = None;
                    return Ok(iced::font::load(bytes).discard());
                }
                Err(e) => {
                    tracing::warn!(
                        target = "oryxis::fonts",
                        face = %key,
                        error = %e,
                        "pack font not loaded; keeping the fallback rendering"
                    );
                    // Drop the guard so re-picking the font (or the
                    // offline switch going off) retries.
                    self.loaded_pack_fonts.remove(&key);
                    let toast = match e {
                        crate::fonts::FetchError::Offline => "font_pack_offline",
                        crate::fonts::FetchError::Other(_) => "font_pack_failed",
                    };
                    self.set_toast(crate::i18n::t(toast).to_string());
                    return Ok(Task::perform(
                        async {
                            tokio::time::sleep(
                                std::time::Duration::from_millis(2600),
                            )
                            .await;
                        },
                        |_| Message::ToastClear,
                    ));
                }
            },
            SettingsMessage::ToggleCopyOnSelect => {
                self.prefs.copy_on_select = !self.prefs.copy_on_select;
                self.persist_setting(
                    "copy_on_select",
                    if self.prefs.copy_on_select { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleRightClickCopy => {
                self.prefs.right_click_copy = !self.prefs.right_click_copy;
                self.persist_setting(
                    "right_click_copy",
                    if self.prefs.right_click_copy { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleMiddleClickPaste => {
                // Writes through the binding table, which owns the
                // gesture; there is no `middle_click_paste` setting any
                // more (see `set_middle_click_paste`).
                let on = !self.middle_click_pastes();
                return Ok(self.set_middle_click_paste(on));
            }
            SettingsMessage::ToggleWheelZoom => {
                // Same shape: the wheel zoom IS the Ctrl+wheel chords on
                // the font zoom actions, and this toggle adds / removes
                // them (see `set_wheel_zoom`).
                let on = !self.wheel_zoom_bound();
                return Ok(self.set_wheel_zoom(on));
            }
            SettingsMessage::SettingSftpDefaultEditorChanged(v) => {
                self.prefs.sftp_default_editor = v;
                self.persist_setting(
                    "sftp_default_editor",
                    &self.prefs.sftp_default_editor.clone(),
                );
            }
            SettingsMessage::SettingSftpDefaultEditorBrowse => {
                return Ok(Task::perform(
                    tokio::task::spawn_blocking(move || {
                        rfd::FileDialog::new()
                            .set_title(crate::i18n::t("setting_default_editor"))
                            .pick_file()
                            .map(|p| p.to_string_lossy().to_string())
                            .ok_or_else(|| "cancelled".to_string())
                    }),
                    |result| {
                        let r = match result {
                            Ok(r) => r,
                            Err(e) => Err(format!("Thread error: {e}")),
                        };
                        Message::Settings(SettingsMessage::SettingSftpDefaultEditorPicked(r))
                    },
                ));
            }
            SettingsMessage::SettingSftpDefaultEditorPicked(result) => {
                if let Ok(path) = result {
                    self.prefs.sftp_default_editor = path;
                    self.persist_setting(
                        "sftp_default_editor",
                        &self.prefs.sftp_default_editor.clone(),
                    );
                }
                // "cancelled" / thread errors stay silent: the user just
                // closed the dialog.
            }
            SettingsMessage::ToggleSftpEditAutosave => {
                self.prefs.sftp_edit_autosave = !self.prefs.sftp_edit_autosave;
                self.persist_setting(
                    "sftp_edit_autosave",
                    if self.prefs.sftp_edit_autosave { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleSftpUploadTempName => {
                self.prefs.sftp_upload_temp_name = !self.prefs.sftp_upload_temp_name;
                self.persist_setting(
                    "sftp_upload_temp_name",
                    if self.prefs.sftp_upload_temp_name { "true" } else { "false" },
                );
            }
            SettingsMessage::SftpConsoleLayoutChanged(name) => {
                use crate::state::SftpConsoleLayout;
                // Matched against the TRANSLATED labels, like every
                // other pick_list here; an unknown label is a stale
                // frame and leaves the setting alone.
                if let Some(layout) = SftpConsoleLayout::ALL
                    .into_iter()
                    .find(|l| crate::i18n::t(l.label_key()) == name)
                {
                    self.prefs.sftp_console_layout = layout;
                    self.persist_setting("sftp_console_layout", layout.code());
                }
            }
            SettingsMessage::SftpDirDoubleClickChanged(name) => {
                // Matched against the translated labels, as above.
                if let Some(action) = crate::state::SftpDirDoubleClick::ALL
                    .into_iter()
                    .find(|a| crate::i18n::t(a.label_key()) == name)
                {
                    self.prefs.sftp_dir_double_click = action;
                    self.persist_setting("sftp_dir_double_click", action.code());
                }
            }
            SettingsMessage::SftpFileDoubleClickChanged(name) => {
                if let Some(action) = crate::state::SftpFileDoubleClick::ALL
                    .into_iter()
                    .find(|a| crate::i18n::t(a.label_key()) == name)
                {
                    self.prefs.sftp_file_double_click = action;
                    self.persist_setting("sftp_file_double_click", action.code());
                }
            }
            SettingsMessage::ToggleSftpAskDownloadDir => {
                self.prefs.sftp_ask_download_dir = !self.prefs.sftp_ask_download_dir;
                self.persist_setting(
                    "sftp_ask_download_dir",
                    if self.prefs.sftp_ask_download_dir { "true" } else { "false" },
                );
            }
            SettingsMessage::TerminalRightClickChanged(name) => {
                use crate::util::RightClickMode;
                if let Some(mode) = RightClickMode::ALL
                    .iter()
                    .find(|m| crate::i18n::t(m.label_key()) == name)
                {
                    self.prefs.terminal_right_click = *mode;
                    self.persist_setting("terminal_right_click", mode.code());
                }
            }
            SettingsMessage::SidebarDefaultTabChanged(name) => {
                use crate::state::TerminalSidebarTab;
                // "Last opened" (the sentinel label) clears the pin; any
                // tab label sets it. Match against the translated labels,
                // like the right-click picker.
                if name == crate::i18n::t("sidebar_default_last") {
                    self.prefs.sidebar_default_tab = None;
                    self.persist_setting("sidebar_default_tab", "last");
                } else if let Some(tab) = TerminalSidebarTab::ALL
                    .into_iter()
                    .find(|t| crate::i18n::t(t.label_key()) == name)
                {
                    self.prefs.sidebar_default_tab = Some(tab);
                    self.persist_setting("sidebar_default_tab", tab.code());
                }
            }
            SettingsMessage::ToggleScrollbackResetKeypress => {
                self.prefs.scrollback_reset_keypress = !self.prefs.scrollback_reset_keypress;
                self.persist_setting(
                    "scrollback_reset_keypress",
                    if self.prefs.scrollback_reset_keypress { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleScrollbackResetOutput => {
                self.prefs.scrollback_reset_output = !self.prefs.scrollback_reset_output;
                self.persist_setting(
                    "scrollback_reset_output",
                    if self.prefs.scrollback_reset_output { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleTerminalPasswordAutofill => {
                self.prefs.terminal_password_autofill =
                    !self.prefs.terminal_password_autofill;
                // Turning it off closes anything already on screen: the
                // setting means "do not offer", not "do not offer next
                // time".
                if !self.prefs.terminal_password_autofill {
                    self.dismiss_password_suggest();
                }
                self.persist_setting(
                    "terminal_password_autofill",
                    if self.prefs.terminal_password_autofill { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleTerminalLinkConfirm => {
                self.prefs.terminal_link_confirm = !self.prefs.terminal_link_confirm;
                // Turning it off does not answer a question already on
                // screen: that click is still the user's to make.
                self.persist_setting(
                    "terminal_link_confirm",
                    if self.prefs.terminal_link_confirm { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleTerminalLinkTunnel => {
                self.prefs.terminal_link_tunnel = !self.prefs.terminal_link_tunnel;
                // Live tunnels keep running: they were opened for a login
                // that is mid-flight, and pulling the port out from under
                // it would break exactly the round trip they exist for.
                // Each closes itself once its callback lands.
                self.persist_setting(
                    "terminal_link_tunnel",
                    if self.prefs.terminal_link_tunnel { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleCarefulPaste => {
                self.prefs.careful_paste = !self.prefs.careful_paste;
                // Turning the guard off releases nothing: a parked paste
                // (dialog open) still needs its explicit confirm/cancel.
                self.persist_setting(
                    "careful_paste",
                    if self.prefs.careful_paste { "true" } else { "false" },
                );
            }
            SettingsMessage::TogglePasteGuard => {
                self.prefs.paste_guard = !self.prefs.paste_guard;
                self.persist_setting(
                    "paste_guard",
                    if self.prefs.paste_guard { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleTerminalAutoTitle => {
                let on = !crate::state::auto_title_enabled();
                crate::state::set_auto_title(on);
                self.persist_setting("terminal_auto_title", if on { "true" } else { "false" });
            }
            SettingsMessage::TogglePaneBorderInactive => {
                self.prefs.pane_border_inactive = !self.prefs.pane_border_inactive;
                self.persist_setting(
                    "pane_border_inactive",
                    if self.prefs.pane_border_inactive { "true" } else { "false" },
                );
            }
            SettingsMessage::TogglePaneHeaders => {
                self.prefs.pane_headers = !self.prefs.pane_headers;
                self.persist_setting(
                    "pane_headers",
                    if self.prefs.pane_headers { "true" } else { "false" },
                );
            }
            SettingsMessage::OpenTerminalThemeGallery => {
                self.panels.terminal_theme_gallery = true;
                // A stale filter from the last visit would open the
                // gallery on a mysteriously short grid.
                self.theme_ui.gallery_filter.clear();
                self.theme_ui.gallery_tone = None;
            }
            SettingsMessage::CloseTerminalThemeGallery => {
                self.panels.terminal_theme_gallery = false;
            }
            SettingsMessage::OpenUiThemeGallery => {
                self.panels.ui_theme_gallery = true;
                // Same rule as the terminal gallery: every visit starts
                // showing everything.
                self.theme_ui.ui_gallery_filter.clear();
                self.theme_ui.ui_gallery_tone = None;
            }
            SettingsMessage::CloseUiThemeGallery => {
                self.panels.ui_theme_gallery = false;
            }
            SettingsMessage::PaneGapChanged(v) => {
                self.prefs.pane_gap = v.clone();
                self.persist_setting("pane_gap", &v);
            }
            SettingsMessage::ToggleBoldIsBright => {
                self.prefs.bold_is_bright = !self.prefs.bold_is_bright;
                self.persist_setting(
                    "bold_is_bright",
                    if self.prefs.bold_is_bright { "true" } else { "false" },
                );
            }
            SettingsMessage::TerminalOpacityChanged(v) => {
                let percent = v
                    .trim_end_matches('%')
                    .parse::<u8>()
                    .unwrap_or(100)
                    .clamp(crate::theme::MIN_TERMINAL_OPACITY, 100);
                if percent == self.prefs.terminal_opacity {
                    return Ok(Task::none());
                }
                let was_opaque = self.prefs.terminal_opacity >= 100;
                self.prefs.terminal_opacity = percent;
                crate::theme::set_terminal_opacity(percent);
                self.persist_setting("terminal_opacity", &percent.to_string());
                // A window that was created opaque has no alpha channel to
                // composite with, so the first step away from 100% is the
                // only one that needs a new window. Every later change
                // (including going back to 100%) is live, which is why the
                // prompt is gated on both halves and not on the value alone.
                if was_opaque && !crate::theme::window_transparent() {
                    self.error_dialog = Some(crate::state::ErrorDialog {
                        title: crate::i18n::t("terminal_opacity_restart_title").to_string(),
                        body: crate::i18n::t("terminal_opacity_restart_body").to_string(),
                        link: None,
                        action: Some(crate::state::ErrorDialogAction {
                            label: crate::i18n::t("renderer_restart_now").to_string(),
                            message: Box::new(Message::Settings(SettingsMessage::RelaunchApp)),
                            // Same rule as the renderer dialog: a restart with live
                            // sessions is the destructive answer, styled as one and
                            // not the default row; with nothing live Enter may take it.
                            danger: self.live_session_tab_count() > 0,
                        }),
                    });
                }
            }
            SettingsMessage::TerminalBgImageBrowse => {
                return Ok(Self::pick_background_image(|r| {
                    Message::Settings(SettingsMessage::TerminalBgImagePicked(r))
                }));
            }
            SettingsMessage::TerminalBgImagePicked(result) => {
                // "cancelled" and thread errors stay silent: the user
                // just closed the dialog.
                if let Ok(path) = result {
                    self.prefs.terminal_bg_image = path;
                    self.persist_setting(
                        "terminal_bg_image",
                        &self.prefs.terminal_bg_image.clone(),
                    );
                }
            }
            SettingsMessage::TerminalBgImageCleared => {
                self.prefs.terminal_bg_image.clear();
                self.persist_setting("terminal_bg_image", "");
            }
            SettingsMessage::TerminalBgFitChanged(label) => {
                let fit = oryxis_terminal::BgFit::ALL
                    .iter()
                    .copied()
                    .find(|f| crate::i18n::t(bg_fit_label_key(*f)) == label)
                    .unwrap_or_default();
                self.prefs.terminal_bg_fit = fit.as_str().to_string();
                self.persist_setting("terminal_bg_fit", fit.as_str());
            }
            SettingsMessage::TerminalBgDimChanged(v) => {
                let percent = v.trim_end_matches('%').parse::<u8>().unwrap_or(55).min(100);
                self.prefs.terminal_bg_dim = percent;
                self.persist_setting("terminal_bg_dim", &percent.to_string());
            }
            SettingsMessage::ToggleKeywordHighlight => {
                self.prefs.keyword_highlight = !self.prefs.keyword_highlight;
                self.persist_setting(
                    "keyword_highlight",
                    if self.prefs.keyword_highlight { "true" } else { "false" },
                );
            }
            SettingsMessage::ToggleCommandHistory => {
                self.prefs.command_history = !self.prefs.command_history;
                self.persist_setting(
                    "command_history",
                    if self.prefs.command_history { "true" } else { "false" },
                );
            }
            SettingsMessage::CopyShellIntegrationSnippet => {
                let snippet =
                    crate::shell_integration::snippet(&self.shell_integration_nonce);
                self.set_toast(crate::i18n::t("shell_integration_copied").to_string());
                return Ok(crate::dispatch_global::write_clipboard_text(snippet));
            }
            SettingsMessage::RegenerateShellIntegrationNonce => {
                let fresh = crate::shell_integration::generate_nonce();
                self.persist_setting(crate::shell_integration::SETTING, &fresh);
                // Installed immediately, so the panes that are open right
                // now start demanding the new key. Any host still sourcing
                // the old snippet stops being recorded from this moment,
                // which is exactly what retiring a leaked key means.
                oryxis_terminal::osc::set_global_command_nonce(Some(fresh.clone()));
                self.shell_integration_nonce = fresh;
                self.set_toast(crate::i18n::t("shell_integration_rotated").to_string());
            }
            SettingsMessage::HintModeChanged(name) => {
                use crate::i18n::t;
                use crate::util::HintMode;
                if let Some(mode) = HintMode::ALL.iter().find(|m| t(m.label_key()) == name) {
                    self.prefs.hint_mode = *mode;
                    self.persist_setting("terminal_hint_mode", mode.code());
                }
            }
            SettingsMessage::PaneEndActionChanged(name) => {
                use crate::i18n::t;
                use crate::util::PaneEndAction;
                if let Some(action) =
                    PaneEndAction::ALL.iter().find(|a| t(a.label_key()) == name)
                {
                    self.prefs.pane_end_action = *action;
                    self.persist_setting("pane_end_action", action.code());
                }
            }
            SettingsMessage::ToggleSmartContrast => {
                self.prefs.smart_contrast = !self.prefs.smart_contrast;
                self.persist_setting(
                    "smart_contrast",
                    if self.prefs.smart_contrast { "true" } else { "false" },
                );
            }
            SettingsMessage::SettingScrollbackChanged(val) => {
                // Cap at 1M rows, alacritty allocates lazily but >1M is
                // both unreasonable and a foot-gun for memory pressure.
                self.prefs.scrollback_rows = sanitize_uint(&val, 1_000_000);
                self.persist_setting("scrollback_rows", &self.prefs.scrollback_rows);
                // Applies to terminals opened after this point; existing
                // sessions keep their current buffer.
                oryxis_terminal::set_default_scrollback(resolve_scrollback_rows(
                    &self.prefs.scrollback_rows,
                ));
            }
            SettingsMessage::SettingWordDelimitersChanged(val) => {
                // Free-text: any character may delimit a word. Stored as
                // typed; the widget syncs it into the terminal backend on
                // the next double-click. Empty is allowed (no delimiters).
                self.prefs.word_delimiters = val;
                self.persist_setting("word_delimiters", &self.prefs.word_delimiters);
            }
            SettingsMessage::SettingResetWordDelimiters => {
                self.prefs.word_delimiters =
                    oryxis_terminal::DEFAULT_WORD_DELIMITERS.to_string();
                self.persist_setting("word_delimiters", &self.prefs.word_delimiters);
            }
            m => return Err(m),
        }
        Ok(Task::none())
    }

    /// Make sure the pack face the picked family + weight needs is on
    /// its way into the font system, returning the task that gets it
    /// there (`None` for a system / bundled family, or one already
    /// requested this session).
    ///
    /// A family with no face at the picked weight resolves to its
    /// Regular (`pack_face_for`); which face cosmic-text then draws
    /// from is its own matching decision, and the picker is what tells
    /// the user the weight could not be served exactly.
    fn ensure_pack_face(&mut self) -> Option<Task<Message>> {
        let face = crate::fonts::pack_face_for(
            &self.terminal_font_name,
            self.terminal_font_weight,
        )?;
        if !self.loaded_pack_fonts.insert(face.key().to_string()) {
            return None;
        }
        // Under offline mode the task answers at once with its own
        // toast; a "downloading" hint before it would promise what is
        // not going to happen.
        if !crate::fonts::is_face_cached(face) && !crate::offline::is_on() {
            self.set_toast(crate::i18n::t("font_pack_downloading").to_string());
        }
        Some(crate::fonts::ensure_pack_task(face))
    }
}

/// The session zoom over the font-size preference.
///
/// Two numbers, two owners: `terminal_font_size` is the preference the
/// Settings stepper edits and persists, `terminal_font_zoom` is what the
/// zoom chords add on top for this session and is never written down.
/// Every terminal canvas draws at the SUM (`terminal_font_px`), and the
/// clamp lives on the sum, so a zoom can neither push the text past the
/// range the stepper allows nor be pushed past it by a later stepper
/// edit.
impl Oryxis {
    /// Smallest and largest size a terminal cell is ever drawn at, the
    /// range the stepper and the zoom both stay inside.
    pub(crate) const TERMINAL_FONT_MIN: f32 = 10.0;
    pub(crate) const TERMINAL_FONT_MAX: f32 = 24.0;

    /// The size every terminal canvas draws at: the preference plus the
    /// session zoom, clamped to the stepper's range.
    pub(crate) fn terminal_font_px(&self) -> f32 {
        (self.terminal_font_size + self.terminal_font_zoom)
            .clamp(Self::TERMINAL_FONT_MIN, Self::TERMINAL_FONT_MAX)
    }

    /// Zoom by whole points, session only. Computed from the EFFECTIVE
    /// size so a step at the edge of the range is a no-op rather than a
    /// hidden overshoot the next opposite step would silently absorb.
    pub(crate) fn zoom_terminal_font(&mut self, steps: f32) {
        let target = (self.terminal_font_px() + steps)
            .clamp(Self::TERMINAL_FONT_MIN, Self::TERMINAL_FONT_MAX);
        self.terminal_font_zoom = target - self.terminal_font_size;
    }

    /// Back to the preference: what "reset zoom" means once the zoom is
    /// a delta over it rather than a rewrite of it.
    pub(crate) fn reset_terminal_font_zoom(&mut self) {
        self.terminal_font_zoom = 0.0;
    }

    /// Re-anchor the zoom after the preference moved, so it keeps
    /// describing what is drawn. Without it a delta the clamp was
    /// already cutting (preference raised under a zoom) keeps being cut
    /// by the next stepper edit too, and a step down changes nothing on
    /// screen: the control reads as dead while the hint under it still
    /// names the old size.
    pub(crate) fn normalize_terminal_font_zoom(&mut self) {
        self.terminal_font_zoom =
            normalized_font_zoom(self.terminal_font_size, self.terminal_font_zoom);
    }
}

/// The zoom that draws the same size as `pref + zoom` would once
/// clamped, expressed as a delta the clamp no longer has to cut.
fn normalized_font_zoom(pref: f32, zoom: f32) -> f32 {
    (pref + zoom).clamp(Oryxis::TERMINAL_FONT_MIN, Oryxis::TERMINAL_FONT_MAX) - pref
}

#[cfg(test)]
mod font_zoom_tests {
    use super::normalized_font_zoom;

    #[test]
    fn a_zoom_inside_the_range_is_left_alone() {
        assert_eq!(normalized_font_zoom(14.0, 4.0), 4.0);
        assert_eq!(normalized_font_zoom(14.0, -3.0), -3.0);
    }

    #[test]
    fn a_zoom_the_clamp_was_cutting_is_trimmed_to_what_is_drawn() {
        // Zoomed to the maximum at 14 pt, then the preference raised to
        // 20: the drawn size is 24, so the delta becomes 4 and a step
        // back down to 19 visibly shrinks the text.
        let zoom = normalized_font_zoom(20.0, 10.0);
        assert_eq!(zoom, 4.0);
        assert_eq!((19.0 + zoom).clamp(10.0, 24.0), 23.0);
        assert_eq!(normalized_font_zoom(11.0, -4.0), -1.0);
    }
}

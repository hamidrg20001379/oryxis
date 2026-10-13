//! Files sidebar tab: a compact remote browser for the focused pane's
//! SSH session (an SFTP channel multiplexed on the live handle). No
//! host header, the host is the tab's own; just the current path, the
//! follow-cwd pin, hidden/refresh/expand actions and the entry list.
//! Rows follow the History tab's conventions: hover-revealed floating
//! Copy path action, click = select (double-click folder = enter),
//! all recorded into the sidebar keynav layer. The keynav slot keeps
//! the direct action (folders navigate, files copy their path): the
//! ring has no double-press gesture, so Enter must not need one.

use iced::Widget as _;
use iced::border::Radius;
use iced::widget::{column, container, text, MouseArea, Space};
use iced::{Background, Border, Color, Element, Length, Padding};

use super::terminal::chat_header_btn;
use crate::app::{SftpMessage, SidebarFilesMessage, Message, Oryxis};
use crate::dispatch_sidebar_files::{files_join, files_parent_dir};
use crate::i18n::t;
use crate::state::TerminalSidebarTab;
use crate::theme::OryxisColors;
use crate::widgets::dir_row;

impl Oryxis {
    pub(crate) fn files_tab_content<'a>(
        &'a self,
        tab: &'a crate::state::TerminalTab,
    ) -> Element<'a, Message> {
        let pane = tab.active();
        let files = &pane.files;
        // The transfer belongs to the PANE, so its cancel is routed by
        // pane id: another SFTP surface may be the focused owner.
        let pane_id = pane.id;

        // A local shell browses the app's own filesystem (issue #145);
        // everything else needs the SSH transport. Disconnected
        // mid-view (the tab button hides on the next frame; this
        // covers the one where it hasn't yet).
        let is_local_pane = pane.session.is_none()
            && matches!(pane.origin, crate::state::PaneOrigin::Local(_));
        if !is_local_pane && pane.session.as_ref().and_then(|s| s.ssh()).is_none() {
            return sidebar_placeholder(t("files_no_session"));
        }

        // ── Header: path + follow pin + hidden / refresh / expand ──
        // Actions recorded in display order (the strip's Close came
        // first, recorded by `view_terminal_sidebar`).
        let stab = TerminalSidebarTab::Files;
        let follow = files.follow();
        // The path is clickable (owner QA: manage the directory by
        // typing, like the SFTP pane's breadcrumb): click swaps the
        // label for a text input; Enter commits (canonicalize + list),
        // Esc via the sidebar router cancels.
        let path_el: Element<'_, Message> = if let Some(editing) = &files.path_editing {
            self.sidebar_nav_slot(
                crate::keynav::SidebarRow::input(iced::widget::Id::new("sidebar-files-path")),
                stab,
                crate::widgets::INPUT_RADIUS,
                iced::widget::text_input("/", editing)
                    .id(iced::widget::Id::new("sidebar-files-path"))
                    .on_input(|v| Message::SidebarFiles(SidebarFilesMessage::SidebarFilesEditPath(v)))
                    .on_submit(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesCommitPath))
                    .padding(4)
                    .size(12)
                    .font(iced::Font::MONOSPACE)
                    .style(crate::widgets::rounded_input_style)
                    .width(Length::Fill)
                    .boxed(),
            )
        } else {
            let path_label = if files.path.is_empty() {
                String::from("…")
            } else {
                files.path.clone()
            };
            // Ellipsize the LEADING side to the actual width so the tail
            // (the directory the user is in) stays visible; a char-count
            // truncation can't know the width and let narrow sidebars
            // wrap the path onto two lines.
            let label = MouseArea::new(
                text(path_label)
                    .size(12)
                    .font(iced::Font::MONOSPACE)
                    .color(OryxisColors::t().text_secondary)
                    .wrapping(iced::widget::text::Wrapping::None)
                    .ellipsis(iced::widget::text::Ellipsis::Start)
                    .width(Length::Fill),
            )
            .on_press(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesStartEditPath))
            .interaction(iced::mouse::Interaction::Text);
            self.sidebar_nav_slot(
                crate::keynav::SidebarRow::button(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesStartEditPath)),
                stab,
                6.0,
                label.boxed(),
            )
        };
        // Combo-box arrow (issue #85, the SFTP path bar's sibling):
        // opens the visited-directory dropdown. Only once there is
        // somewhere to go back to, so it never opens an empty list.
        // Present in BOTH header modes: while editing it is exactly the
        // MobaXterm affordance (type or pick).
        let history_arrow = (!files.path_history.is_empty()).then(|| {
            self.sidebar_nav_slot(
                crate::keynav::SidebarRow::button(Message::SidebarFiles(
                    SidebarFilesMessage::SidebarFilesPathHistoryToggle,
                )),
                stab,
                6.0,
                toggle_action_btn(
                    if files.path_history_open {
                        iced_fonts::lucide::chevron_up()
                    } else {
                        iced_fonts::lucide::chevron_down()
                    },
                    files.path_history_open,
                    Message::SidebarFiles(SidebarFilesMessage::SidebarFilesPathHistoryToggle),
                    t("sftp_path_history"),
                ),
            )
        });
        // While the path is being edited the input takes the WHOLE
        // header width and the four actions hide (owner ask): they
        // return as soon as the edit ends (Enter commit, Esc, blur by
        // walking/clicking away). Built inside the branch because a
        // nav slot records at construction time.
        let header_cells: Vec<Element<'_, Message>> = if files.path_editing.is_some() {
            let mut cells = vec![path_el];
            if let Some(arrow) = history_arrow {
                cells.push(Space::new().width(4).boxed());
                cells.push(arrow);
            }
            cells
        } else {
            let pin_btn = self.sidebar_nav_slot(
                crate::keynav::SidebarRow::button(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesToggleFollow)),
                stab,
                6.0,
                // The pin fills with an accent wash when following (a clear
                // toggled-on look) so it never reads as disabled while active.
                toggle_action_btn(
                    if follow {
                        iced_fonts::lucide::pin()
                    } else {
                        iced_fonts::lucide::pin_off()
                    },
                    follow,
                    Message::SidebarFiles(SidebarFilesMessage::SidebarFilesToggleFollow),
                    if follow { t("files_follow_on_tip") } else { t("files_follow_off_tip") },
                ),
            );
            let hidden_btn = self.sidebar_nav_slot(
                crate::keynav::SidebarRow::button(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesToggleHidden)),
                stab,
                6.0,
                toggle_action_btn(
                    if files.show_hidden {
                        iced_fonts::lucide::eye()
                    } else {
                        iced_fonts::lucide::eye_off()
                    },
                    files.show_hidden,
                    Message::SidebarFiles(SidebarFilesMessage::SidebarFilesToggleHidden),
                    if files.show_hidden { t("hide_hidden_files") } else { t("show_hidden_files") },
                ),
            );
            let refresh_btn = self.sidebar_nav_slot(
                crate::keynav::SidebarRow::button(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesRefresh)),
                stab,
                6.0,
                action_btn(
                    iced_fonts::lucide::rotate_cw(),
                    Message::SidebarFiles(SidebarFilesMessage::SidebarFilesRefresh),
                    t("refresh"),
                ),
            );
            // No dual-pane SFTP surface exists for a local shell, so
            // the promote affordance hides there (issue #145).
            let expand_btn = (!is_local_pane).then(|| {
                self.sidebar_nav_slot(
                    crate::keynav::SidebarRow::button(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesExpand)),
                    stab,
                    6.0,
                    action_btn(
                        iced_fonts::lucide::folder_tree(),
                        Message::SidebarFiles(SidebarFilesMessage::SidebarFilesExpand),
                        t("open_sftp_session_here"),
                    ),
                )
            });
            let mut cells = vec![path_el, Space::new().width(4).boxed()];
            if let Some(arrow) = history_arrow {
                cells.push(arrow);
            }
            cells.extend([pin_btn, hidden_btn, refresh_btn]);
            if let Some(btn) = expand_btn {
                cells.push(btn);
            }
            cells
        };
        let header = container(
            dir_row(header_cells).align_y(iced::Alignment::Center),
        )
        .padding(Padding { top: 10.0, right: 10.0, bottom: 8.0, left: 12.0 })
        .width(Length::Fill);

        // ── Body ──
        let body: Element<'_, Message> = if let Some(err) = &files.error {
            column![
                sidebar_placeholder(err),
                container(
                    self.sidebar_nav_slot(
                        crate::keynav::SidebarRow::button(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesRefresh)),
                        stab,
                        6.0,
                        action_btn(
                            iced_fonts::lucide::rotate_cw(),
                            Message::SidebarFiles(SidebarFilesMessage::SidebarFilesRefresh),
                            t("retry"),
                        ),
                    )
                )
                .center_x(Length::Fill)
                .padding(Padding { top: 8.0, right: 0.0, bottom: 0.0, left: 0.0 }),
            ].boxed()
        } else if files.client.is_none() {
            sidebar_placeholder(t("files_mounting"))
        } else if files.loading && files.entries.is_empty() {
            // Navigation in flight (the optimistic adopt cleared the old
            // rows): a generic Loading, not the mount copy.
            sidebar_placeholder(t("loading"))
        } else {
            let mut list = iced::widget::Column::<iced::Element<'_, _>>::new()
                .spacing(4)
                .padding(Padding { top: 0.0, right: 12.0, bottom: 12.0, left: 12.0 });
            let mut pos = 0usize;
            // Inline "new file / new folder" input at the top of the
            // list (Enter creates, Esc via the sidebar router cancels).
            if let Some((kind, input)) = &files.new_entry {
                let icon = match kind {
                    crate::state::SftpEntryKind::Folder => iced_fonts::lucide::folder_plus(),
                    crate::state::SftpEntryKind::File => iced_fonts::lucide::file_plus(),
                };
                let field = iced::widget::text_input(t("name"), input)
                    .id(iced::widget::Id::new("sidebar-files-new"))
                    .on_input(|v| Message::SidebarFiles(SidebarFilesMessage::SidebarFilesNewEntryInput(v)))
                    .on_submit(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesNewEntryCommit))
                    .padding(6)
                    .size(12)
                    .style(crate::widgets::rounded_input_style);
                let row = dir_row(vec![
                    icon.size(13).color(OryxisColors::t().accent).boxed(),
                    Space::new().width(8).boxed(),
                    field.boxed(),
                ])
                .align_y(iced::Alignment::Center);
                list = list.push(self.sidebar_nav_slot(
                    crate::keynav::SidebarRow::input(iced::widget::Id::new(
                        "sidebar-files-new",
                    )),
                    TerminalSidebarTab::Files,
                    crate::widgets::INPUT_RADIUS,
                    container(row)
                        .padding(Padding { top: 2.0, right: 0.0, bottom: 2.0, left: 2.0 })
                        .boxed(),
                ));
                pos += 1;
            }
            // ".." row, hidden at the root. A row like any other (the
            // dual pane's and FileZilla's rule): a click selects it and a
            // double click goes up. The ring's Enter goes up in one step,
            // as Enter on a selected `..` does.
            if let Some(parent) = files_parent_dir(&files.path) {
                let up = Message::SidebarFiles(SidebarFilesMessage::SidebarFilesNavigate(parent));
                list = list.push(self.files_row(
                    "..",
                    true,
                    false,
                    0,
                    Message::SidebarFiles(SidebarFilesMessage::SidebarFilesSelectParent),
                    up,
                    None,
                    files.parent_selected && files.selected.is_empty(),
                    false,
                    pos,
                ));
                pos += 1;
            }
            let mut any = false;
            for entry in &files.entries {
                if !files.show_hidden && entry.name.starts_with('.') {
                    continue;
                }
                any = true;
                let full = files_join(&files.path, &entry.name);
                // Inline rename swaps this row's label for an input
                // (Enter commits, Esc via the sidebar router cancels).
                if let Some((rpath, rinput)) = &files.rename
                    && rpath == &full
                {
                    let field = iced::widget::text_input("", rinput)
                        .id(iced::widget::Id::new("sidebar-files-rename"))
                        .on_input(|v| Message::SidebarFiles(SidebarFilesMessage::SidebarFilesRenameInput(v)))
                        .on_submit(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesRenameCommit))
                        .padding(6)
                        .size(12)
                        .style(crate::widgets::rounded_input_style);
                    let row = dir_row(vec![
                        crate::views::sftp::file_icon(
                            &entry.name,
                            entry.is_dir,
                            entry.is_symlink,
                        )
                        .boxed(),
                        Space::new().width(8).boxed(),
                        field.boxed(),
                    ])
                    .align_y(iced::Alignment::Center);
                    list = list.push(self.sidebar_nav_slot(
                        crate::keynav::SidebarRow::input(iced::widget::Id::new(
                            "sidebar-files-rename",
                        )),
                        TerminalSidebarTab::Files,
                        crate::widgets::INPUT_RADIUS,
                        container(row)
                            .padding(Padding {
                                top: 2.0,
                                right: 0.0,
                                bottom: 2.0,
                                left: 2.0,
                            })
                            .boxed(),
                    ));
                    pos += 1;
                    continue;
                }
                let press = Message::SidebarFiles(SidebarFilesMessage::SidebarFilesSelectRow(
                    full.clone(),
                    entry.is_dir,
                ));
                // The ring's Enter skips the selection step: folders
                // navigate, files copy their path (with the toast as
                // feedback), as before click-select existed.
                let key_activate = if entry.is_dir {
                    Message::SidebarFiles(SidebarFilesMessage::SidebarFilesNavigate(full.clone()))
                } else {
                    Message::Sftp(SftpMessage::SftpCopyPath(full.clone()))
                };
                let is_selected = files.selected.iter().any(|s| s == &full);
                list = list.push(self.files_row(
                    &entry.name,
                    entry.is_dir,
                    entry.is_symlink,
                    entry.size,
                    press,
                    key_activate,
                    Some(full),
                    is_selected,
                    is_selected && files.selected.len() > 1,
                    pos,
                ));
                pos += 1;
            }
            if !any {
                list = list.push(sidebar_placeholder(t("files_empty")));
            }
            // Shared id with the Snippets / History lists (only one
            // renders): the sidebar keynav router snaps the ringed row
            // into view by it. Right-clicking the empty area opens the
            // directory-level menu (rows consume their own right-press
            // first).
            let scroll = iced::widget::scrollable(list)
                .id(crate::keynav::sidebar_scroll_id(crate::state::TerminalSidebarTab::Files))
                .width(Length::Fill)
                .height(Length::Fill);
            MouseArea::new(scroll)
                .on_right_press(Message::SidebarFiles(SidebarFilesMessage::ShowSidebarFilesBackgroundMenu))
                .boxed()
        };

        // Drop-to-upload hint (issue #167): shown while the OS drags
        // files anywhere over the window. The drop router already sends
        // the payload to this browser's directory whenever the tab is
        // on screen with a mounted client (`sidebar_dir` in
        // `dispatch_terminal/drop.rs`), so the hint states exactly what
        // a release will do; without it the gesture looked unsupported
        // (the report behind #167). Display only, never a gate.
        let drop_hint = (self.os_drop_hover
            && files.client.is_some()
            && !files.path.is_empty())
        .then(|| {
            let accent = OryxisColors::t().accent;
            container(
                dir_row(vec![
                    iced_fonts::lucide::upload().size(12).color(accent).boxed(),
                    Space::new().width(6).boxed(),
                    text(t("files_drop_hint")).size(11).color(accent).boxed(),
                ])
                .align_y(iced::Alignment::Center),
            )
            .padding(Padding { top: 6.0, right: 8.0, bottom: 6.0, left: 8.0 })
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(Color { a: 0.08, ..accent })),
                border: Border {
                    radius: Radius::from(6.0),
                    color: accent,
                    width: 1.0,
                },
                ..Default::default()
            })
        });

        // A running transfer, pinned under the list so it is visible
        // without scrolling. The strip is the SAME one the dual-pane
        // surface draws, in its narrow form: two surfaces rendering one
        // transfer differently is how they start disagreeing about it.
        // Cancel is owner-routed, so it cancels THIS pane's transfer even
        // when another SFTP surface is the focused one.
        // The hint fills a slot that is always there (empty while no
        // drag hovers): it comes and goes with the OS drag, and a child
        // inserted above `body` would shift it and drop an inline
        // rename's focus.
        let hint_slot = match drop_hint {
            Some(hint) => column![container(hint).padding(Padding {
                top: 0.0,
                right: 8.0,
                bottom: 4.0,
                left: 8.0,
            })],
            None => column![],
        };
        let mut col = column![header, hint_slot, body].width(Length::Fill).height(Length::Fill);
        if let Some(transfer) = files.transfer.state.as_ref() {
            let cancel = Message::SftpFor(
                pane_id,
                Box::new(SftpMessage::SftpCancelTransfer),
            );
            // Recorded LAST, which is where it is drawn: the sidebar
            // ring walks in build order, so a slot registered out of
            // place lands the cursor somewhere the eye is not.
            let strip = self.sidebar_nav_slot(
                crate::keynav::SidebarRow::button(cancel.clone()),
                stab,
                0.0,
                crate::views::sftp::transfer_progress_strip(
                    transfer,
                    files
                        .transfer
                        .bytes_done
                        .load(std::sync::atomic::Ordering::Relaxed),
                    files.transfer.bytes_total,
                    cancel,
                    true,
                ),
            );
            col = col.push(strip);
        }
        let stack: Element<'_, Message> = col.boxed();
        // Clicks on dead space blur the inline edits (owner ask: "blur
        // should exit the path input"). MouseArea only fires when no
        // child captured the press, so the inputs, rows and buttons all
        // keep their clicks and only true empty space lands here.
        let content: Element<'_, Message> = MouseArea::new(stack)
        .on_press(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesEditBlur))
        .boxed();
        // Visited-directory dropdown (issue #85): stacked over the tab
        // body, anchored under the header, scrim-closed like the SFTP
        // pane's. Guarded on history so a stale open flag can't paint
        // an empty menu.
        if files.path_history_open && !files.path_history.is_empty() {
            iced::widget::Stack::<iced::Element<'_, _>>::new()
                .push(content)
                .push(path_history_overlay(&files.path_history))
                .boxed()
        } else {
            content
        }
    }

    /// One browser row. A mouse press fires `press` (select; a quick
    /// second press on a folder enters it), while the keynav slot
    /// records `key_activate` (Enter = the direct action: folders
    /// navigate, files copy their path). `full_path` enables the
    /// hover-revealed Copy path action; the ".." row has none.
    /// `multi_selected` hides that single-row copy chip: inside a
    /// multi-selection the bulk actions live in the right-click menu,
    /// and a chip that copied only one of several highlighted paths
    /// would read as a surprise.
    #[allow(clippy::too_many_arguments)]
    fn files_row<'a>(
        &'a self,
        name: &'a str,
        is_dir: bool,
        is_symlink: bool,
        size: u64,
        press: Message,
        key_activate: Message,
        full_path: Option<String>,
        selected: bool,
        multi_selected: bool,
        pos: usize,
    ) -> Element<'a, Message> {
        let c = OryxisColors::t();
        let hovered = self.cur_nav().hover.files_row == Some(pos);

        // A folder's ICON enters it on a single click (issue #143), the
        // dual-pane affordance; the rest of the row keeps single-click
        // select / double-click enter. The inner MouseArea captures the
        // press, so the row's own select never double-fires. For a
        // folder `key_activate` IS the navigate message, which is what
        // makes this one clone instead of a new parameter.
        let icon: Element<'a, Message> =
            crate::views::sftp::file_icon(name, is_dir, is_symlink).boxed();
        let icon_cell: Element<'a, Message> = if is_dir {
            MouseArea::new(icon)
                .on_press(key_activate.clone())
                .interaction(iced::mouse::Interaction::Pointer)
                .boxed()
        } else {
            icon
        };
        let mut cells: Vec<Element<'a, Message>> = vec![
            icon_cell,
            Space::new().width(8).boxed(),
            // Long names truncate with an ellipsis at the row edge
            // instead of bleeding over the size cell and past the card
            // (the SFTP pane's data-cell rule).
            text(name)
                .size(12)
                .color(c.text_primary)
                .wrapping(iced::widget::text::Wrapping::None)
                .ellipsis(iced::widget::text::Ellipsis::End)
                .width(Length::Fill)
                .boxed(),
        ];
        if !is_dir {
            cells.push(Space::new().width(6).boxed());
            cells.push(
                text(crate::views::sftp::format_size(size))
                    .size(11)
                    .color(c.text_muted)
                    .boxed(),
            );
        }
        let card = container(dir_row(cells).align_y(iced::Alignment::Center))
            .padding(Padding { top: 6.0, right: 10.0, bottom: 6.0, left: 10.0 })
            .width(Length::Fill)
            .style(move |_| {
                let bg = if selected {
                    Color { a: 0.20, ..OryxisColors::t().accent }
                } else {
                    OryxisColors::t().bg_surface
                };
                container::Style {
                    background: Some(Background::Color(bg)),
                    border: Border { radius: Radius::from(6.0), ..Default::default() },
                    ..Default::default()
                }
            });

        // Hover-revealed floating Copy path (the card-action convention;
        // the ring border stays the keyboard affordance). Hidden on a
        // row inside a multi-selection (see `multi_selected`).
        let row_el: Element<'a, Message> = match (&full_path, hovered, multi_selected) {
            (Some(full), true, false) => {
                let actions = container(action_btn(
                    iced_fonts::lucide::clipboard_copy(),
                    Message::Sftp(SftpMessage::SftpCopyPath(full.clone())),
                    t("copy_path"),
                ))
                .padding(Padding { top: 2.0, right: 4.0, bottom: 2.0, left: 4.0 })
                .style(|_: &iced::Theme| container::Style {
                    background: Some(Background::Color(OryxisColors::t().bg_selected)),
                    border: Border { radius: Radius::from(6.0), ..Default::default() },
                    ..Default::default()
                });
                let overlay = container::<_, iced::Theme>(actions)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(iced::alignment::Horizontal::Right)
                    .align_y(iced::alignment::Vertical::Center)
                    .padding(Padding { top: 0.0, right: 4.0, bottom: 0.0, left: 0.0 });
                iced::widget::Stack::<iced::Element<'_, _>>::new().push(card.boxed()).push(overlay.boxed()).boxed()
            }
            _ => card.boxed(),
        };

        let mut area = MouseArea::new(row_el)
            .on_enter(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesRowHovered(pos)))
            .on_exit(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesRowUnhovered(pos)))
            .on_press(press)
            .interaction(iced::mouse::Interaction::Pointer);
        // The press half of the click, with its position, published ahead
        // of the row's own press message for the double-click judgement.
        let click_key = match &full_path {
            Some(full) => crate::state::FilesClickKey::Entry(full.clone()),
            None => crate::state::FilesClickKey::Parent,
        };
        // Right-click opens the row's context menu (Open / Open SFTP
        // session here / Copy path / Copy name); the ".." row has none.
        if let Some(full) = &full_path {
            area = area
                .on_right_press(Message::SidebarFiles(SidebarFilesMessage::ShowSidebarFilesRowMenu(full.clone(), is_dir)));
        }

        // The keyboard half of the right-click: Delete asks about the
        // row (through the shared confirm dialog, same rule as the
        // Snippets / History rows) and the Menu key opens the same
        // context menu. The ".." row carries neither.
        let mut row = crate::keynav::SidebarRow::list_button(key_activate).with_anchor(selected);
        if let Some(full) = &full_path {
            row = row
                .with_delete(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesDelete(
                    full.clone(),
                    is_dir,
                )))
                .with_menu(Message::SidebarFiles(SidebarFilesMessage::ShowSidebarFilesRowMenu(
                    full.clone(),
                    is_dir,
                )));
        }
        self.sidebar_nav_slot(
            row,
            TerminalSidebarTab::Files,
            6.0,
            crate::widgets::press_publisher(area.boxed(), move |at| {
                Message::SidebarFiles(SidebarFilesMessage::SidebarFilesRowPressed(
                    click_key.clone(),
                    at,
                ))
            }),
        )
    }
}

/// Visited-directory dropdown for the path combo-box (issue #85):
/// most recent first, clicking an entry navigates there, the scrim
/// closes it. Full sidebar width (the SFTP pane's fixed 360px would
/// not fit here); long paths ellipsize on the LEADING side so the
/// directory name stays readable.
fn path_history_overlay<'a>(history: &'a [String]) -> Element<'a, Message> {
    let mut col = iced::widget::Column::<iced::Element<'_, _>>::new().spacing(2).padding(4);
    for path in history {
        col = col.push(
            iced::widget::button(
                dir_row(vec![
                    iced_fonts::lucide::history()
                        .size(12)
                        .color(OryxisColors::t().text_muted)
                        .boxed(),
                    Space::new().width(8).boxed(),
                    text(path.clone())
                        .size(12)
                        .color(OryxisColors::t().text_primary)
                        .wrapping(iced::widget::text::Wrapping::None)
                        .ellipsis(iced::widget::text::Ellipsis::Start)
                        .width(Length::Fill)
                        .boxed(),
                ])
                .align_y(iced::Alignment::Center),
            )
            .on_press(Message::SidebarFiles(
                SidebarFilesMessage::SidebarFilesPathHistoryPick(path.clone()),
            ))
            .padding(Padding { top: 6.0, right: 10.0, bottom: 6.0, left: 10.0 })
            .width(Length::Fill)
            .style(|_, status| {
                use iced::widget::button::Status as St;
                let bg = match status {
                    St::Hovered | St::Pressed => OryxisColors::t().bg_hover,
                    _ => Color::TRANSPARENT,
                };
                iced::widget::button::Style {
                    background: Some(Background::Color(bg)),
                    border: Border { radius: Radius::from(4.0), ..Default::default() },
                    ..Default::default()
                }
            }).boxed(),
        );
    }
    // A long history scrolls instead of running off the sidebar.
    let menu = container(
        iced::widget::scrollable::<_, _, iced::Theme>(col)
            .height(Length::Fixed((history.len() as f32 * 30.0 + 8.0).min(320.0))),
    )
    .style(|_: &iced::Theme| container::Style {
        background: Some(Background::Color(OryxisColors::t().bg_surface)),
        border: Border {
            radius: Radius::from(8.0),
            color: OryxisColors::t().border,
            width: 1.0,
        },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.25),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
        ..Default::default()
    });
    let scrim: Element<'_, Message> = MouseArea::new(
        container(Space::new()).width(Length::Fill).height(Length::Fill),
    )
    .on_press(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesPathHistoryClose))
    .boxed();
    // Anchored right under the header band, full width bar the header's
    // side padding.
    let positioned = container::<_, iced::Theme>(menu)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(iced::alignment::Vertical::Top)
        .padding(Padding { top: 42.0, right: 10.0, bottom: 0.0, left: 12.0 });
    iced::widget::Stack::<iced::Element<'_, _>>::new().push(scrim).push(positioned.boxed()).boxed()
}

/// Centered muted text for the empty / mounting / error states.
fn sidebar_placeholder(label: &str) -> Element<'_, Message> {
    container(text(label).size(12).color(OryxisColors::t().text_muted))
        .center_x(Length::Fill)
        .padding(Padding { top: 40.0, right: 12.0, bottom: 0.0, left: 12.0 })
        .width(Length::Fill)
        .boxed()
}

/// A toggle icon action: fills with an accent wash + accent glyph when
/// `active` (a clear "engaged" look), otherwise a muted glyph with the
/// standard hover feedback. Wrapped in the same tooltip chrome.
fn toggle_action_btn<'a>(
    icon: iced::widget::Text<'a>,
    active: bool,
    msg: Message,
    tip: &'a str,
) -> Element<'a, Message> {
    let fg = if active { OryxisColors::t().accent } else { OryxisColors::t().text_muted };
    let btn = iced::widget::button(
        container(icon.size(13).color(fg))
            .center_x(Length::Fixed(28.0))
            .center_y(Length::Fixed(24.0)),
    )
    .padding(0)
    .on_press(msg)
    .style(move |_, status| {
        use iced::widget::button::Status as St;
        let c = OryxisColors::t();
        let bg = if active {
            Color { a: 0.15, ..c.accent }
        } else {
            match status {
                St::Hovered | St::Pressed => c.bg_hover,
                _ => Color::TRANSPARENT,
            }
        };
        iced::widget::button::Style {
            background: Some(Background::Color(bg)),
            border: Border { radius: Radius::from(6.0), ..Default::default() },
            ..Default::default()
        }
    });
    iced::widget::tooltip(
        btn,
        container(text(tip).size(11).color(OryxisColors::t().text_primary))
            .padding(Padding { top: 4.0, right: 8.0, bottom: 4.0, left: 8.0 })
            .style(|_| container::Style {
                background: Some(Background::Color(OryxisColors::t().bg_surface)),
                border: Border {
                    radius: Radius::from(6.0),
                    color: OryxisColors::t().border,
                    width: 1.0,
                },
                ..Default::default()
            }),
        iced::widget::tooltip::Position::Top,
    )
    .boxed()
}

/// An icon action with a tooltip (same chrome as the History-row actions).
fn action_btn<'a>(
    icon: iced::widget::Text<'a>,
    msg: Message,
    tip: &'a str,
) -> Element<'a, Message> {
    iced::widget::tooltip(
        chat_header_btn(icon, msg),
        container(text(tip).size(11).color(OryxisColors::t().text_primary))
            .padding(Padding { top: 4.0, right: 8.0, bottom: 4.0, left: 8.0 })
            .style(|_| container::Style {
                background: Some(Background::Color(OryxisColors::t().bg_surface)),
                border: Border {
                    radius: Radius::from(6.0),
                    color: OryxisColors::t().border,
                    width: 1.0,
                },
                ..Default::default()
            }),
        iced::widget::tooltip::Position::Top,
    )
    .boxed()
}

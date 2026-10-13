//! Overlay layers that build their own `Stack` around `base` (the
//! cloud-import confirmation with its group-picker popover, the
//! positioned overlay / context menus, and the SFTP row menu + drag
//! ghost). Split out of views/layout/main_layout.rs; each returns the
//! finished, resize-wrapped `Element`.

use super::*;
use iced::widget::{column, row};

impl Oryxis {
    /// Cloud-import confirmation modal: a hand-built `Stack` (scrim +
    /// centered dialog + optional positioned group-picker popover) so the
    /// picker can rise above the dialog. Not routed through `modal_overlay`.
    pub(crate) fn layer_cloud_import_confirm<'a>(
        &'a self,
        base: Element<'a, Message>,
        resize_overlay: Option<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        use oryxis_core::models::cloud::TransportKind;
        let n_ec2 = self.cloud_discover.selected_ec2.len();
        let n_ecs = self.cloud_discover.selected_ecs.len();
        let summary = if n_ec2 > 0 && n_ecs > 0 {
            format!("{} EC2 + {} ECS", n_ec2, n_ecs)
        } else if n_ec2 > 0 {
            format!("{} EC2", n_ec2)
        } else {
            format!("{} ECS", n_ecs)
        };

        // Keyboard rows in visual order: the group input (Enter
        // focuses it), the transport picker (Left/Right cycle),
        // Import (default) and Cancel.
        self.modal_nav_reset();

        // Import-into field + chevron. The suggestion dropdown
        // is no longer inline; it's a floating popover rendered
        // via the global OverlayState (`CloudDiscoverGroupPicker`)
        // injected into the modal's own Stack below so it can
        // visually rise above the dialog instead of pushing
        // siblings. Input + chevron heights are explicitly fixed
        // to 36 so they stay aligned in the row.
        const COMBO_HEIGHT: f32 = 36.0;
        let group_input = iced::widget::text_input(
            crate::i18n::t("cloud_discover_import_into_placeholder"),
            &self.cloud_discover.default_group_name,
        )
        .on_input(|v| Message::Cloud(CloudMessage::CloudDiscoverDefaultGroupNameChanged(v)))
        .id(iced::widget::Id::new("cloud-import-group-input"))
        .padding(8)
        .style(crate::widgets::rounded_input_style)
        .align_x(dir_align_x());
        // Row 0: Enter focuses the group input.
        let group_input = self.modal_nav_slot(
            crate::keynav::RowAction::input(iced::widget::Id::new(
                "cloud-import-group-input",
            )),
            10.0,
            false,
            group_input.boxed(),
        );
        let chevron_btn = iced::widget::button(
            container(
                iced_fonts::lucide::chevron_down::<iced::Theme>()
                    .size(12)
                    .color(OryxisColors::t().text_muted),
            )
            .center_x(Length::Fixed(32.0))
            .center_y(Length::Fixed(COMBO_HEIGHT)),
        )
        .on_press(Message::Cloud(CloudMessage::ToggleCloudDiscoverGroupPicker))
        .padding(0)
        .style(|_, status| {
            let bg = match status {
                iced::widget::button::Status::Hovered => OryxisColors::t().bg_hover,
                _ => OryxisColors::t().bg_surface,
            };
            iced::widget::button::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    radius: Radius::from(6.0),
                    color: OryxisColors::t().border,
                    width: 1.0,
                },
                ..Default::default()
            }
        });

        // Transport picker is always rendered. For ECS-only
        // imports the value is ignored on save (dynamic groups
        // always run ECS Exec), but keeping the row in place
        // preserves the row geometry + the explanatory hint
        // beneath it and avoids the modal looking sparse when
        // the user happens to pick zero EC2 hosts.
        let transport_section: Element<'_, Message> = {
            let transport_options = vec![
                TransportKind::Ssh,
                TransportKind::InstanceConnect,
                TransportKind::Ssm,
            ];
            let transport_pick = iced::widget::pick_list(
                Some(self.cloud_discover.default_transport),
                transport_options.clone(),
                |t| match t {
                    TransportKind::Ssh => "SSH".to_string(),
                    TransportKind::InstanceConnect => "EC2 Instance Connect".to_string(),
                    TransportKind::Ssm => "SSM Session".to_string(),
                    TransportKind::EcsExec => "ECS Exec".to_string(),
                    TransportKind::KubectlExec => "kubectl exec".to_string(),
                },
            )
            .on_select(|v| Message::Cloud(CloudMessage::CloudDiscoverDefaultTransportChanged(v)))
            .on_open(Message::Navigation(NavigationMessage::PickOpenChanged(true)))
            .on_close(Message::Navigation(NavigationMessage::PickOpenChanged(false)))
            .padding(10)
            .style(crate::widgets::rounded_pick_list_style);
            // Row 1: Left/Right cycle the transport without
            // opening the dropdown.
            let (t_prev, t_next) = crate::keynav::slots::cycle_pair(
                &transport_options,
                &self.cloud_discover.default_transport,
                |v| Message::Cloud(CloudMessage::CloudDiscoverDefaultTransportChanged(v)),
            );
            let transport_pick = self.modal_nav_slot(
                crate::keynav::RowAction::picker(t_prev, t_next),
                10.0,
                false,
                transport_pick.boxed(),
            );
            column![
                text(crate::i18n::t("cloud_dynamic_form_transport"))
                    .size(12)
                    .color(OryxisColors::t().text_secondary),
                Space::new().height(4),
                container(transport_pick).width(Length::Fixed(320.0)),
                Space::new().height(8),
                text(crate::i18n::t("cloud_import_transport_hint"))
                    .size(11)
                    .color(OryxisColors::t().text_muted),
                Space::new().height(16),
            ].boxed()
        };
        // Silence the now-unused n_ec2 binding; kept by name so
        // the summary text above can read it without re-querying.
        let _ = n_ec2;

        let dialog_content = container(
            column![
                text(crate::i18n::t("cloud_import_confirm_title"))
                    .size(16)
                    .color(OryxisColors::t().text_primary),
                Space::new().height(4),
                text(summary).size(11).color(OryxisColors::t().text_muted),
                Space::new().height(16),
                // "Import into" comes BEFORE Transport: the
                // dropdown is anchored to the chevron and opens
                // downward, so having the field higher in the
                // dialog gives the menu maximum vertical room
                // to extend without escaping the screen edge.
                text(crate::i18n::t("cloud_discover_import_into"))
                    .size(12)
                    .color(OryxisColors::t().text_secondary),
                Space::new().height(4),
                // Wrap the combo row in `bounds_reporter` so the
                // toggle handler can read its on-screen rect and
                // anchor the picker overlay right below it. The
                // cell lives on Oryxis state; the wrapper here
                // just writes to it on every draw pass. Wrapping
                // the whole row (input + chevron) means the menu
                // can mirror the full combo width by default,
                // covering the empty area between the input and
                // the chevron edge.
                crate::widgets::bounds_reporter(
                    dir_row(vec![
                        container(group_input)
                            .width(Length::Fill)
                            .height(Length::Fixed(COMBO_HEIGHT))
                            .boxed(),
                        Space::new().width(6).boxed(),
                        container(chevron_btn)
                            .height(Length::Fixed(COMBO_HEIGHT))
                            .boxed(),
                    ])
                    .width(Length::Fixed(308.0))
                    .align_y(iced::Alignment::Center).boxed(),
                    self.cloud_discover.default_group_combo_bounds.clone(),
                ),
                Space::new().height(16),
                transport_section,
                crate::widgets::dir_row(vec![
                    self.modal_nav_slot_default(
                        crate::keynav::RowAction::activate(
                            Message::Cloud(CloudMessage::CloudDiscoverImportConfirmed),
                        ),
                        6.0,
                        true,
                        styled_button(
                            crate::i18n::t("import_btn_label"),
                            Message::Cloud(CloudMessage::CloudDiscoverImportConfirmed),
                            OryxisColors::t().accent,
                        ),
                    ),
                    Space::new().width(8).boxed(),
                    self.modal_nav_slot(
                        crate::keynav::RowAction::activate(
                            Message::Cloud(CloudMessage::CloudDiscoverImportCancelled),
                        ),
                        6.0,
                        false,
                        styled_button(
                            crate::i18n::t("cancel"),
                            Message::Cloud(CloudMessage::CloudDiscoverImportCancelled),
                            OryxisColors::t().text_muted,
                        ),
                    ),
                ]),
            ]
            .padding(24),
        )
        .style(|_: &iced::Theme| container::Style {
            background: Some(Background::Color(OryxisColors::t().bg_surface)),
            border: Border {
                radius: Radius::from(12.0),
                color: OryxisColors::t().border,
                width: 1.0,
            },
            ..Default::default()
        });

        let centered = container::<_, iced::Theme>(
            MouseArea::new(dialog_content).on_press(Message::NoOp),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);

        // Intentionally NOT routed through `widgets::modal_overlay`:
        // this modal injects a positioned group-picker popover into its
        // own Stack (below) and uses a context-dependent scrim message,
        // neither of which the simple helper hosts. It stays mouse-safe
        // via `opaque` and keyboard-safe via `any_modal_blocks_input`.
        //
        // Scrim behaviour: while the group picker is open,
        // off-dialog clicks dismiss only the picker so the user
        // doesn't accidentally cancel the whole import. Wrapped
        // in `iced::widget::opaque` so hover events stop here
        // instead of bleeding through to the dashboard cards
        // beneath the modal (otherwise iced's Stack lets mouse
        // hover propagate to lower layers, lighting up rows
        // under the cursor while the modal is open).
        let on_scrim_click = if self.cloud_discover.default_group_picker_open {
            Message::Cloud(CloudMessage::ToggleCloudDiscoverGroupPicker)
        } else {
            Message::Cloud(CloudMessage::CloudDiscoverImportCancelled)
        };
        let scrim: Element<'_, Message> = iced::widget::opaque(
            MouseArea::new(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(|_| container::Style {
                        background: Some(Background::Color(Color::from_rgba(
                            0.0, 0.0, 0.0, 0.5,
                        ))),
                        ..Default::default()
                    }),
            )
            .on_press(on_scrim_click),
        ).boxed();

        // Group-picker context menu: same pattern as the
        // existing kebab menus. Built via the global
        // `OverlayState` + `render_overlay_menu` pipeline so the
        // menu styling, backdrop, and dismiss-on-click-outside
        // all behave like every other context menu in the app.
        // Injected here (inside the modal's Stack) because the
        // modal short-circuits the global overlay path further
        // down in `view_main`.
        let mut modal_stack =
            Stack::<iced::Element<'_, _>>::new().push(base).push(scrim).push(centered.boxed());
        if let Some(ref ovl) = self.overlay
            && matches!(ovl.content, OverlayContent::CloudDiscoverGroupPicker)
        {
            let menu = self.render_overlay_menu(ovl);
            // Width matches the combo's measured width from the
            // bounds_reporter (falls back to 308 on the very
            // first open when the cell is still zeroed). Height
            // clamp keeps tall menus on-screen.
            let combo = self.cloud_discover.default_group_combo_bounds.get();
            let menu_width = if combo.width > 0.0 { combo.width } else { 308.0 };
            let menu_height = 280.0_f32;
            let x = ovl
                .x
                .min((self.cur_window_size().width - menu_width).max(0.0))
                .max(0.0);
            let y = ovl
                .y
                .min((self.cur_window_size().height - menu_height).max(0.0))
                .max(0.0);
            let backdrop: Element<'_, Message> = MouseArea::new(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .on_press(Message::Cloud(CloudMessage::ToggleCloudDiscoverGroupPicker))
            .boxed();
            let positioned: Element<'_, Message> = column![
                Space::new().height(y),
                row![
                    Space::new().width(x),
                    container(menu).width(Length::Fixed(menu_width)),
                ],
            ].boxed();
            modal_stack = modal_stack.push(backdrop).push(positioned);
        }

        wrap_with_resize(
            modal_stack
                .width(Length::Fill)
                .height(Length::Fill)
                .boxed(),
            resize_overlay,
        )
    }

    /// Generic overlay dropdown / context menu, positioned and clamped to
    /// the window, with a click-dismiss backdrop for click-triggered menus
    /// (skipped for the hover-driven split popover).
    pub(crate) fn layer_overlay_menu<'a>(
        &'a self,
        base: Element<'a, Message>,
        resize_overlay: Option<Element<'a, Message>>,
        overlay: &'a OverlayState,
    ) -> Element<'a, Message> {
        let menu = self.render_overlay_menu(overlay);

        // The `+` split popover is hover-driven: it opens on hover and
        // dismisses on mouse-out (`SplitMenuLeave`), so a click-dismiss
        // backdrop is redundant for it. Worse, a full-screen backdrop sits
        // on top of the `+` button and swallows the click, so the first
        // click on `+` only closes the popover and a second is needed to
        // open a new tab. Skip the backdrop here so the click reaches the
        // button. Every other overlay through this path is click-triggered
        // and keeps its click-outside dismissal.
        //
        // The password-suggest popup (#117) skips it for the same
        // reason from the other direction: it floats over a live
        // terminal, and a full-screen backdrop would turn every click
        // on the session underneath into "close the popup" instead of
        // a click on the terminal. It dismisses through the pane's own
        // interactions (FocusPane, typing, tab switch) instead.
        let is_hover_popover = matches!(
            overlay.content,
            OverlayContent::SplitMenu | OverlayContent::PasswordSuggest { .. }
        );

        // Position the menu, clamping to window bounds to prevent clipping.
        // Under RTL, anchor by the menu's right edge so it grows toward
        // the leading (left) side, mirroring native OS dropdown behavior.
        // Width must match the value used in `render_overlay_menu` so
        // clamping lines up with the rendered box.
        let menu_width = self.overlay_menu_width(overlay);
        let menu_height = self.overlay_menu_height(overlay);
        let raw_x = if crate::i18n::is_rtl_layout() {
            overlay.x - menu_width
        } else {
            overlay.x
        };
        let x = raw_x.min(self.cur_window_size().width - menu_width).max(0.0);
        // Vertically the box flips over its anchor when it does not fit
        // under it, and falls back to the clamp; see `overlay_menu_y`.
        let y = self.overlay_menu_y(overlay, menu_height);
        let positioned_menu: Element<'_, Message> = column![
            Space::new().height(y),
            row![
                Space::new().width(x),
                menu,
            ],
        ].boxed();

        let mut stack = Stack::<iced::Element<'_, _>>::new().push(base);
        if !is_hover_popover {
            // Transparent backdrop that dismisses the menu on click.
            let backdrop: Element<'_, Message> = MouseArea::new(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .on_press(Message::Tabs(TabsMessage::HideOverlayMenu))
            .boxed();
            stack = stack.push(backdrop);
        }
        wrap_with_resize(
            stack
                .push(positioned_menu)
                .width(Length::Fill)
                .height(Length::Fill)
                .boxed(),
            resize_overlay,
        )
    }

    /// SFTP row right-click menu, rendered at the layout root so the
    /// window-coordinate click position lines up with the menu origin.
    pub(crate) fn layer_sftp_row_menu<'a>(
        &'a self,
        base: Element<'a, Message>,
        resize_overlay: Option<Element<'a, Message>>,
        row_menu: &'a crate::state::SftpRowMenu,
    ) -> Element<'a, Message> {
        // "Cross-pane action available" = the pane opposite the
        // right-clicked row is connected (remote with a client) or is
        // a local destination. The row menu uses this to decide
        // whether to offer Upload / Download / Relay; a double click's
        // Transfer reads the same answer.
        let cross = self.sftp_cross_pane(row_menu.side);
        let other = self.cur_sftp().pane(cross.other_side);
        let cross_pane_ready = cross.ready;
        let other_is_remote = cross.other_is_remote;
        let src_pane = self.cur_sftp().pane(row_menu.side);
        let source_is_remote = src_pane.is_remote;
        let other_label = other.host_label.clone();
        // Current directory of the source pane + its local path, fed to
        // the directory-level actions (Refresh / New / Open in FM).
        let pane_dir = if source_is_remote {
            src_pane.remote_path.clone()
        } else {
            src_pane.local_path.to_string_lossy().into_owned()
        };
        let local_dir = src_pane.local_path.clone();
        let show_hidden = src_pane.show_hidden;
        // Count of selected rows in the same pane as the right-
        // clicked row, drives the bulk vs single menu mode.
        let selection_count_same_pane = self
            .sftp
            .selected_rows
            .iter()
            .filter(|(s, _)| *s == row_menu.side)
            .count();
        // Archive context: what the probe found on the mounted host (or
        // the in-process codecs for a local pane) decides which archive
        // actions the menu can offer for this row.
        let archive_ctx = {
            use oryxis_archive::names::ArchiveKind;
            use oryxis_archive::remote as remote_cmd;
            let in_zip = src_pane.zip.is_some();
            let name = crate::dispatch_sftp_archive::base_name(&row_menu.path);
            let kind = ArchiveKind::from_name(&name);
            let (extractable, compress_zip, compress_tgz) = if source_is_remote {
                match src_pane.archive_tools {
                    Some((shell, tools)) => (
                        kind.is_some_and(|k| remote_cmd::can_extract(shell, tools, k)),
                        remote_cmd::can_compress(shell, tools, ArchiveKind::Zip),
                        remote_cmd::can_compress(shell, tools, ArchiveKind::TarGz),
                    ),
                    None => (false, false, false),
                }
            } else {
                (
                    matches!(
                        kind,
                        Some(ArchiveKind::Zip | ArchiveKind::TarGz | ArchiveKind::Tar)
                    ),
                    true,
                    true,
                )
            };
            crate::views::sftp::RowArchiveCtx {
                in_zip,
                copy_out_ready: in_zip
                    && other.zip.is_none()
                    && (!other.is_remote || other.client.is_some()),
                browsable: !in_zip && matches!(kind, Some(ArchiveKind::Zip)),
                extractable: !in_zip && extractable,
                compress_zip: !in_zip && compress_zip,
                compress_tgz: !in_zip && compress_tgz,
            }
        };
        // Record the menu's rows into the modal keynav layer (only one
        // such surface renders per frame) so the SFTP row menu is
        // keyboard-navigable.
        self.modal_nav_reset();
        let menu = crate::views::sftp::row_context_menu_box(
            self,
            row_menu,
            cross_pane_ready,
            source_is_remote,
            other_is_remote,
            other_label,
            selection_count_same_pane,
            archive_ctx,
            crate::views::sftp::DirActionCtx {
                pane_dir: &pane_dir,
                local_dir: &local_dir,
                show_hidden,
            },
        );
        let backdrop: Element<'_, Message> = MouseArea::new(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::Sftp(SftpMessage::SftpRowMenuClose))
        .boxed();
        // Nudge the menu a few px down/right so it doesn't sit
        // directly under the cursor, feels like the OS-native menu
        // anchoring.
        let menu_width = crate::views::sftp::ROW_CONTEXT_MENU_WIDTH;
        let rtl = crate::i18n::is_rtl_layout();
        // Under RTL, nudge toward the leading side so the menu grows
        // left-from-cursor instead of right-from-cursor.
        let nudged_x = if rtl {
            row_menu.x - 2.0 - menu_width
        } else {
            row_menu.x + 2.0
        };
        let nudged_y = row_menu.y + 2.0;
        let menu_height = crate::views::sftp::row_context_menu_height(
            self,
            row_menu,
            cross_pane_ready,
            source_is_remote,
            other_is_remote,
            selection_count_same_pane,
            archive_ctx,
        );
        let x = nudged_x
            .min(self.cur_window_size().width - menu_width)
            .max(0.0);
        let y = nudged_y
            .min(self.cur_window_size().height - menu_height)
            .max(0.0);
        let positioned_menu: Element<'_, Message> = column![
            Space::new().height(y),
            row![Space::new().width(x), menu],
        ].boxed();
        wrap_with_resize(
            Stack::<iced::Element<'_, _>>::new()
                .push(base)
                .push(backdrop)
                .push(positioned_menu)
                .width(Length::Fill)
                .height(Length::Fill)
                .boxed(),
            resize_overlay,
        )
    }

    /// A tab being dragged out of the strip and over the content area
    /// (issue #112): the split anchor it is currently proposing, painted
    /// as the space the arriving session will occupy, plus the tab's own
    /// ghost chip now free in both axes.
    ///
    /// Lives at the window root for two reasons: the proposal's rectangle
    /// is already in window coordinates, and the strip's own Stack is
    /// clipped to the bar, which is exactly why the chip could never
    /// follow the cursor down here. Purely decorative, no `MouseArea`
    /// anywhere, so the release that ends the drag reaches the app.
    pub(crate) fn layer_tab_drop<'a>(
        &'a self,
        base: Element<'a, Message>,
        resize_overlay: Option<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        let mut stack = Stack::<iced::Element<'_, _>>::new().push(base);
        let accent = OryxisColors::t().accent;
        if let Some((_, proposal)) = self.tab_drop_proposal() {
            let rect = proposal.highlight;
            // Inset by the border width so the outline reads as the edge
            // OF the region rather than a line spilling into the pane
            // next door.
            let fill: Element<'_, Message> = container(Space::new())
                .width(Length::Fixed((rect.width - 4.0).max(0.0)))
                .height(Length::Fixed((rect.height - 4.0).max(0.0)))
                .style(move |_| container::Style {
                    background: Some(Background::Color(Color { a: 0.18, ..accent })),
                    border: Border {
                        color: accent,
                        width: 2.0,
                        radius: Radius::from(4.0),
                    },
                    ..Default::default()
                })
                .boxed();
            let positioned: Element<'_, Message> = column![
                Space::new().height(rect.y + 2.0),
                row![Space::new().width(rect.x + 2.0), fill],
            ].boxed();
            stack = stack.push(positioned);
        }
        // The chip itself. Centered on the cursor horizontally like the
        // strip does, and lifted half a row so the pointer sits on it
        // rather than under it.
        if let Some((ghost, ghost_w)) = self.strip_drag_ghost_el(
            crate::views::tab_bar::TAB_NATURAL_WIDTH,
            false,
            &self.privacy_terms(),
        ) {
            let x = (self.cur_mouse().x - ghost_w / 2.0)
                .min(self.cur_window_size().width - ghost_w)
                .max(0.0);
            let y = (self.cur_mouse().y - crate::views::tab_bar::TAB_HEIGHT / 2.0)
                .min(self.cur_window_size().height - crate::views::tab_bar::TAB_HEIGHT)
                .max(0.0);
            let positioned: Element<'_, Message> =
                column![Space::new().height(y), row![Space::new().width(x), ghost]].boxed();
            stack = stack.push(positioned);
        }
        wrap_with_resize(stack.width(Length::Fill).height(Length::Fill).boxed(), resize_overlay)
    }

    /// Floating drag ghost for an in-flight file drag, tracking the
    /// cursor above everything else and non-interactive so it never
    /// swallows the release that ends the drag. Serves both drags a file
    /// row can start (cross-pane transfer, drag-out) from one pill, so
    /// the gesture looks the same until it lands.
    pub(crate) fn layer_drag_ghost<'a>(
        &'a self,
        base: Element<'a, Message>,
        resize_overlay: Option<Element<'a, Message>>,
        label: &str,
    ) -> Element<'a, Message> {
        self.layer_drag_ghost_with_icon(base, resize_overlay, iced_fonts::lucide::file(), label)
    }

    /// [`layer_drag_ghost`] with the pill's glyph chosen by the caller.
    pub(crate) fn layer_drag_ghost_with_icon<'a>(
        &'a self,
        base: Element<'a, Message>,
        resize_overlay: Option<Element<'a, Message>>,
        icon: iced::widget::Text<'a>,
        label: &str,
    ) -> Element<'a, Message> {
        let ghost = crate::views::sftp::drag_ghost_with_icon(icon, label);
        // Offset slightly off the cursor, matches OS drag previews
        // and keeps the label out from under the pointer. Direction
        // mirrors under RTL so the ghost trails the cursor on the
        // leading side instead of running off-screen at the edge.
        let ghost_width = 200.0_f32;
        let x_offset = if crate::i18n::is_rtl_layout() {
            -ghost_width - 12.0
        } else {
            12.0
        };
        let x = (self.cur_mouse().x + x_offset)
            .min(self.cur_window_size().width - ghost_width)
            .max(0.0);
        let y = (self.cur_mouse().y + 12.0)
            .min(self.cur_window_size().height - 40.0)
            .max(0.0);
        let positioned: Element<'_, Message> = column![
            Space::new().height(y),
            row![Space::new().width(x), ghost],
        ].boxed();
        wrap_with_resize(
            Stack::<iced::Element<'_, _>>::new()
                .push(base)
                .push(positioned)
                .width(Length::Fill)
                .height(Length::Fill)
                .boxed(),
            resize_overlay,
        )
    }
}

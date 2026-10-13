//! Moving around: hover, the two view toggles, refresh, and the
//! three ways out of the current directory (walk into it, promote
//! the browser to a full SFTP surface, follow the shell).
//!
//! `SidebarFilesNavigate` is the one place that unpins the OSC 7
//! follow, so manual navigation wins until the pin is re-enabled.

use super::*;

impl Oryxis {
    pub(super) fn handle_sidebar_files_navigate(
        &mut self,
        message: SidebarFilesMessage,
    ) -> Task<Message> {
        match message {
            SidebarFilesMessage::SidebarFilesRowHovered(idx) => {
                self.nav.hover.files_row = Some(idx);
            }
            SidebarFilesMessage::SidebarFilesRowUnhovered(idx) => {
                self.nav.hover.leave_files_row(idx);
            }
            SidebarFilesMessage::SidebarFilesRowPressed(key, at) => {
                let plain = !(self.modifiers.control()
                    || self.modifiers.command()
                    || self.modifiers.shift());
                let Some(pane) = self.active_pane_mut() else {
                    return Task::none();
                };
                if !plain {
                    // Selection-building: never half of a double click,
                    // and no stamp for the next plain press to pair with.
                    pane.files.click_tracker.reset();
                    pane.files.press_verdict = None;
                    return Task::none();
                }
                let rule = crate::double_click::DoubleClickRule::os();
                let report =
                    pane.files.click_tracker.press(key.clone(), at, std::time::Instant::now(), &rule);
                pane.files.press_verdict = Some((key, report));
            }
            SidebarFilesMessage::SidebarFilesSelectParent => {
                // The `..` row, the dual pane's rule: a click selects it, a
                // double click goes up. The ring's Enter still goes up in
                // one step (`key_activate`), like Enter on a selected `..`.
                self.keynav.sidebar_selected = None;
                let plain = !(self.modifiers.control()
                    || self.modifiers.command()
                    || self.modifiers.shift());
                let Some(pane) = self.active_pane_mut() else {
                    return Task::none();
                };
                pane.files.path_editing = None;
                pane.files.path_history_open = false;
                let verdict = pane
                    .files
                    .press_verdict
                    .take()
                    .filter(|(key, _)| *key == crate::state::FilesClickKey::Parent);
                if plain
                    && verdict.is_some_and(|(_, r)| r.double)
                    && let Some(parent) = files_parent_dir(&pane.files.path)
                {
                    return self.update(Message::SidebarFiles(
                        SidebarFilesMessage::SidebarFilesNavigate(parent),
                    ));
                }
                pane.files.selected.clear();
                pane.files.selection_anchor = None;
                pane.files.parent_selected = true;
            }
            SidebarFilesMessage::SidebarFilesSelectRow(path, is_dir) => {
                // Single-click selects the row (highlight); double-click
                // on a directory enters it. Ctrl/Cmd-click toggles a row
                // in/out of the selection and Shift-click extends a range
                // from the anchor, matching the dual-pane SFTP surface.
                // The click hands the cursor to the mouse: a keynav ring
                // engaged elsewhere is dropped so Enter can't act on a row
                // the user just clicked away from (the selection anchors
                // the next arrow entry right here instead).
                self.keynav.sidebar_selected = None;
                // Modifiers are read off `self` first: the pane borrow
                // below outlives the selection edit.
                let ctrl = self.modifiers.control() || self.modifiers.command();
                let shift = self.modifiers.shift();
                // Read off `self` with the modifiers, for the same
                // reason: the arm below is built while the pane borrow
                // is live.
                let press_pos = self.cur_mouse();
                let Some(pane) = self.active_pane_mut() else {
                    return Task::none();
                };
                // Clicking a row is also the path editor's blur, same
                // rule as Navigate: close the edit so the header snaps
                // back to the label + actions.
                pane.files.path_editing = None;
                pane.files.path_history_open = false;
                // Arm a drag-out (issue #167) on FILE rows where a
                // backend can serve it: crossing the movement threshold
                // while the button is still down raises the ghost, and
                // leaving the window turns the press into an OS drag
                // (see `advance_drag_out`); a plain click stays exactly
                // the select it always was.
                //
                // BEFORE the selection edit below, which is the whole
                // reason a multi-selection can be dragged at all: a
                // plain press collapses `selected` to the pressed row,
                // so a payload built after it would carry one file no
                // matter how many were highlighted, and the gesture the
                // selection exists for (pick several, drag one of them
                // out) would silently drop the rest. `SftpSelectRow`
                // arms in the same position and says so.
                // Computed here, PUBLISHED at the end: `pane` still owns
                // the borrow through the selection edit, and the value
                // is what has to be captured early, not the assignment.
                let arm = crate::drag_out::supported()
                    .then(|| super::sidebar_drag_out_payload(&pane.files, &path, is_dir))
                    .flatten()
                    .map(
                    |(payload, label)| crate::drag_out::DragOutArm {
                        press: press_pos,
                        label,
                        stage: crate::drag_out::DragOutStage::Armed(payload),
                    },
                );
                // What this press was, judged when it landed
                // (`SidebarFilesRowPressed`, published ahead of this
                // message). Only a PLAIN click can be a double-click: a
                // ctrl/shift press is selection-building, never an enter
                // (the SFTP pane's rule), and its press reset the tracker.
                let verdict = pane
                    .files
                    .press_verdict
                    .take()
                    .filter(|(key, _)| *key == crate::state::FilesClickKey::Entry(path.clone()));
                let is_double = !ctrl && !shift && verdict.is_some_and(|(_, r)| r.double);
                // An entry click takes the selection off the `..` row.
                pane.files.parent_selected = false;
                if is_double && is_dir {
                    pane.files.selected.clear();
                    pane.files.selection_anchor = None;
                    return self.update(Message::SidebarFiles(
                        SidebarFilesMessage::SidebarFilesNavigate(path),
                    ));
                }
                if shift {
                    // Range select within the visible listing. If the
                    // anchor row is gone (refresh / delete pruned it) or
                    // the target can't be indexed, fall through to a
                    // single-select instead of silently growing the
                    // range from nowhere.
                    let range = pane.files.selection_anchor.as_ref().and_then(|anchor| {
                        let entries = super::visible_entry_paths(&pane.files);
                        let a = entries.iter().position(|p| p == anchor);
                        let t = entries.iter().position(|p| p == &path);
                        match (a, t) {
                            (Some(ai), Some(ti)) => {
                                let (lo, hi) = if ai <= ti { (ai, ti) } else { (ti, ai) };
                                Some(entries[lo..=hi].to_vec())
                            }
                            _ => None,
                        }
                    });
                    if let Some(range) = range {
                        pane.files.selected = range;
                        // No early return: the shared publish at the
                        // bottom still has to run. The payload it
                        // publishes was taken BEFORE this range was
                        // built, which is the same answer the SFTP pane
                        // gives (it arms ahead of its own collapse):
                        // what a press drags is what was selected when
                        // the button went down, not what the same press
                        // then selected.
                    } else {
                        pane.files.selected = vec![path.clone()];
                        pane.files.selection_anchor = Some(path.clone());
                    }
                } else if ctrl {
                    // Ctrl-click toggle. Anchor follows the click so a
                    // subsequent shift-click extends from here.
                    if let Some(pos) = pane.files.selected.iter().position(|p| p == &path) {
                        pane.files.selected.remove(pos);
                    } else {
                        pane.files.selected.push(path.clone());
                    }
                    pane.files.selection_anchor = Some(path.clone());
                } else {
                    pane.files.selected = vec![path.clone()];
                    pane.files.selection_anchor = Some(path.clone());
                }
                self.drag_out_arm = arm;
            }
            SidebarFilesMessage::SidebarFilesToggleFollow => {
                if let Some(pane) = self.active_pane_mut() {
                    pane.files.follow_disabled = !pane.files.follow_disabled;
                }
                // Re-enabling the pin snaps the browser back to the
                // shell's directory right away.
                return self.sidebar_files_sync();
            }
            SidebarFilesMessage::SidebarFilesToggleHidden => {
                if let Some(pane) = self.active_pane_mut() {
                    pane.files.show_hidden = !pane.files.show_hidden;
                    // A row that just left the list leaves the selection
                    // with it: otherwise Ctrl+A with dotfiles shown, then
                    // the toggle, then "Delete N items" would count rows
                    // nobody can see, and delete `.ssh` on a confirm that
                    // only showed a number.
                    if !pane.files.show_hidden {
                        let hidden = |p: &String| files_basename(p).starts_with('.');
                        pane.files.selected.retain(|p| !hidden(p));
                        if pane.files.selection_anchor.as_ref().is_some_and(hidden) {
                            pane.files.selection_anchor = None;
                        }
                    }
                }
            }
            SidebarFilesMessage::SidebarFilesRefresh => {
                // Also fired from the background context menu.
                self.overlay = None;
                let Some(pane) = self.active_pane_mut() else {
                    return Task::none();
                };
                pane.files.error = None;
                // The selection is deliberately kept: the rows stay on
                // screen while the re-list is in flight, and the prune
                // on arrival keeps it only if its entry survived.
                match (&pane.files.client, pane.files.path.is_empty()) {
                    // Mounted: re-list the current directory.
                    (Some(client), false) => {
                        let client = client.clone();
                        let path = pane.files.path.clone();
                        let pane_id = pane.id;
                        pane.files.loading = true;
                        let seq = pane.files.next_req();
                        return list_dir_task(client, path, pane_id, seq);
                    }
                    // Not mounted (or a failed mount): retry from scratch.
                    _ => return self.sidebar_files_sync(),
                }
            }
            SidebarFilesMessage::SidebarFilesNavigate(path) => {
                // Also fired from the row context menu and the
                // ".." row; dismiss the overlay and clear selection.
                self.overlay = None;
                let Some(pane) = self.active_pane_mut() else {
                    return Task::none();
                };
                let Some(client) = pane.files.client.clone() else {
                    return Task::none();
                };
                // Clicking a row while the path input is open is its
                // blur: close the edit (its buffer is stale the moment
                // the listing changes) so the header snaps back to the
                // label + actions.
                pane.files.path_editing = None;
                // A manual navigation away from the shell's cwd would be
                // undone by the next follow sync, so browsing by hand
                // implies unpinning; the toggle re-enables it. The toast
                // makes the silent state flip visible (owner QA ask).
                let mut unpinned = false;
                if pane.files.follow()
                    && pane
                        .cwd
                        .as_deref()
                        .and_then(|c| expand_cwd(c, pane.files.home.as_deref()))
                        .as_deref()
                        != Some(path.as_str())
                {
                    pane.files.follow_disabled = true;
                    unpinned = true;
                }
                let pane_id = pane.id;
                // Optimistic UI: adopt the target path on screen NOW and
                // clear the old listing, so the click answers instantly
                // (the roundtrip used to look like a freeze, owner QA).
                // Clearing is also correctness: keeping the OLD rows
                // visible under the NEW path would let a rapid second
                // click join a stale entry name onto the wrong base.
                // The ".." row derives from the optimistic path, so
                // navigating up mid-load stays coherent; the listing
                // that lands (stamp-guarded) replaces everything.
                pane.files.path = path.clone();
                pane.files.entries.clear();
                pane.files.loading = true;
                pane.files.error = None;
                pane.files.selected.clear();
                pane.files.selection_anchor = None;
                pane.files.click_tracker.reset();
                pane.files.parent_selected = false;
                // Rapid clicks race their listings; the stamp makes the
                // LATEST navigation win regardless of completion order.
                let seq = pane.files.next_req();
                let list = list_dir_task(client, path, pane_id, seq);
                if unpinned {
                    self.set_toast(crate::i18n::t("files_follow_paused").to_string());
                    return Task::batch([
                        list,
                        Task::perform(
                            async {
                                tokio::time::sleep(std::time::Duration::from_millis(
                                    2500,
                                ))
                                .await;
                            },
                            |_| Message::ToastClear,
                        ),
                    ]);
                }
                return list;
            }
            SidebarFilesMessage::SidebarFilesExpand => {
                // Expand = this tab's SFTP session (the hybrid Files
                // mode) at the browser's current directory. Owner QA
                // 2026-07-05: expanding must NOT open a standalone tab.
                let path = self
                    .active_pane_mut()
                    .map(|p| p.files.path.clone())
                    .unwrap_or_default();
                return self.update(Message::SidebarFiles(SidebarFilesMessage::SidebarFilesOpenSftpAt(path)));
            }
            SidebarFilesMessage::SidebarFilesOpenSftpAt(path) => {
                // Flip the active tab into its SFTP session at `path`.
                // The one-shot hint is consumed by the toggle's mount
                // (or by a navigate when the session already exists),
                // with home fallback if the path stopped existing.
                self.overlay = None;
                let Some(tab_idx) = self.cur_active_tab() else {
                    return Task::none();
                };
                self.sftp_open_at_path = (!path.is_empty()).then_some(path);
                return self.update(Message::Tabs(TabsMessage::ToggleTabFilesMode(tab_idx)));
            }
            // The parent routed us here, so anything else is a
            // grouping mistake, not a runtime case.
            m => return crate::dispatch::unrouted(m),
        }
        Task::none()
    }
}

//! Where a directory listing actually arrives: the initial mount,
//! each subsequent listing, and the error path.
//!
//! All three carry the request's sequence number and drop anything
//! stale, so a slow listing cannot overwrite the directory the user
//! has already moved on to.

use super::*;

impl Oryxis {
    pub(super) fn handle_sidebar_files_listing(
        &mut self,
        message: SidebarFilesMessage,
    ) -> Task<Message> {
        match message {
            SidebarFilesMessage::SidebarFilesMounted(pane_id, seq, client, home, path, mut entries) => {
                let Some(pane) = self.pane_by_id_any_tab(pane_id) else {
                    return Task::none();
                };
                // Superseded (a newer request, or a disconnect reset that
                // bumped the stamp): the channel may ride a dead handle,
                // drop it instead of installing a client that can only
                // error. Also guards the reconnect race where the pane
                // has a NEW session by the time the old mount lands.
                if pane.files.req_seq != seq {
                    return Task::none();
                }
                // A local backend (issue #145) rides no transport, so
                // only an SFTP mount demands the live session.
                if !client.is_local()
                    && pane.session.as_ref().and_then(|s| s.ssh()).is_none()
                {
                    return Task::none();
                }
                sort_entries(&mut entries);
                pane.files.client = Some(client);
                pane.files.home = home;
                pane.files.mounting = false;
                pane.files.loading = false;
                pane.files.error = None;
                // Adopted-directory history (issue #85): recorded only
                // here and in Listed, i.e. once a path proved listable.
                // Unconditional: the optimistic navigate already set
                // `files.path`, so an equality guard would skip exactly
                // the visits that matter (the dedupe makes re-recording
                // the current directory a no-op).
                let previous = std::mem::take(&mut pane.files.path);
                pane.files.push_path_history(path.clone());
                pane.files.path = path.clone();
                if previous != path {
                    pane.files.push_nav(previous);
                }
                pane.files.entries = entries;
                prune_selection(&mut pane.files, &path);
                // Mount is where the stored, host-keyed history comes back
                // (the per-pane list is wiped on disconnect on purpose),
                // and where this visit joins it.
                self.hydrate_files_recent(pane_id);
                self.record_files_recent(pane_id, &path);
                // The title-fallback cwd may be `~`-relative and only
                // expandable now that the home is known; chase it.
                return self.sidebar_files_sync();
            }
            SidebarFilesMessage::SidebarFilesListed(pane_id, seq, path, mut entries) => {
                let Some(pane) = self.pane_by_id_any_tab(pane_id) else {
                    return Task::none();
                };
                // Out-of-order completion of a superseded listing: drop,
                // the newer request's result is the one that must win.
                if pane.files.req_seq != seq {
                    return Task::none();
                }
                sort_entries(&mut entries);
                pane.files.loading = false;
                pane.files.error = None;
                // Unconditional for the same optimistic-path reason as
                // the Mounted arm above.
                let previous = std::mem::take(&mut pane.files.path);
                pane.files.push_path_history(path.clone());
                pane.files.path = path.clone();
                if previous != path {
                    pane.files.push_nav(previous);
                }
                pane.files.entries = entries;
                prune_selection(&mut pane.files, &path);
                self.record_files_recent(pane_id, &path);
                // The shell may have moved again while this listing was
                // in flight; chase it so follow never sticks one step
                // behind a fast `cd a && cd b`.
                return self.sidebar_files_sync();
            }
            SidebarFilesMessage::SidebarFilesError(pane_id, seq, e) => {
                let Some(pane) = self.pane_by_id_any_tab(pane_id) else {
                    return Task::none();
                };
                // A stale error must not clear the flags (or paint the
                // banner) of a newer in-flight request.
                if pane.files.req_seq != seq {
                    return Task::none();
                }
                pane.files.mounting = false;
                pane.files.loading = false;
                pane.files.error = Some(e);
            }
            // The parent routed us here, so anything else is a
            // grouping mistake, not a runtime case.
            m => return crate::dispatch::unrouted(m),
        }
        Task::none()
    }
}

/// A listing replaced the rows: the double-click stamp is stale by
/// definition, and the selection keeps the entries still present (a
/// same-directory refresh, an op_then_list completion, a delete that
/// took some rows and not others); a listing of any other directory
/// drops it whole. The shift-click anchor follows the same rule, so a
/// later shift-click never extends from a row that is no longer there.
fn prune_selection(files: &mut crate::state::PaneFiles, path: &str) {
    files.click_tracker.reset();
    if files.selected.is_empty() {
        files.selection_anchor = None;
        return;
    }
    files
        .selected
        .retain(|s| files.entries.iter().any(|e| files_join(path, &e.name) == *s));
    if files.selected.is_empty() {
        files.selection_anchor = None;
    } else {
        files.selection_anchor = files.selection_anchor.as_ref().and_then(|a| {
            files
                .entries
                .iter()
                .any(|e| files_join(path, &e.name) == *a)
                .then(|| a.clone())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> oryxis_ssh::SftpEntry {
        oryxis_ssh::SftpEntry {
            name: name.to_string(),
            is_dir,
            is_symlink: false,
            size: 0,
            mtime: None,
            permissions: None,
            uid: None,
            gid: None,
            owner: None,
            group: None,
        }
    }

    #[test]
    fn prune_drops_deleted_rows_and_their_anchor() {
        let mut files = crate::state::PaneFiles {
            path: "/srv".to_string(),
            entries: vec![entry("a.conf", false), entry("b.conf", false)],
            selected: vec!["/srv/a.conf".to_string(), "/srv/gone".to_string()],
            selection_anchor: Some("/srv/gone".to_string()),
            ..Default::default()
        };
        prune_selection(&mut files, "/srv");
        assert_eq!(files.selected, vec!["/srv/a.conf".to_string()]);
        // The anchor's row no longer exists, so a later shift-click
        // must not extend from it.
        assert_eq!(files.selection_anchor, None);
    }

    #[test]
    fn prune_keeps_surviving_anchor() {
        let mut files = crate::state::PaneFiles {
            path: "/srv".to_string(),
            entries: vec![entry("a.conf", false), entry("b.conf", false)],
            selected: vec!["/srv/a.conf".to_string(), "/srv/b.conf".to_string()],
            selection_anchor: Some("/srv/a.conf".to_string()),
            ..Default::default()
        };
        prune_selection(&mut files, "/srv");
        assert_eq!(
            files.selected,
            vec!["/srv/a.conf".to_string(), "/srv/b.conf".to_string()]
        );
        assert_eq!(files.selection_anchor.as_deref(), Some("/srv/a.conf"));
    }

    #[test]
    fn prune_clears_an_empty_selection() {
        let mut files = crate::state::PaneFiles {
            path: "/srv".to_string(),
            entries: vec![entry("a.conf", false)],
            selection_anchor: Some("/srv/a.conf".to_string()),
            ..Default::default()
        };
        prune_selection(&mut files, "/srv");
        assert!(files.selected.is_empty());
        assert_eq!(files.selection_anchor, None);
    }
}

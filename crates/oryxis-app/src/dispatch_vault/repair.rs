//! Rebuilding a damaged vault file from the boot error screen (issue
//! #250). The screen only exists while no vault is open, so nothing
//! here can race a live handle of this process on the file.

use super::*;
use crate::state::VaultRebuild;

impl Oryxis {
    pub(super) fn handle_vault_repair(
        &mut self,
        message: VaultMessage,
    ) -> Task<Message> {
        match message {
            VaultMessage::RebuildDamagedVault => {
                // Only for a file the open called damaged, never while a
                // vault is open, and never a second time once the rebuilt
                // file is in place: a rebuild of the REBUILT file would
                // rename a healthy vault to `.damaged-*`.
                let allowed = self.vault.is_none()
                    && self.vault_ui.open_corrupt
                    && matches!(self.vault_ui.rebuild, VaultRebuild::Idle | VaultRebuild::Failed(_));
                if !allowed {
                    return Task::none();
                }
                let Some(path) = oryxis_vault::VaultStore::default_path() else {
                    self.vault_ui.rebuild = VaultRebuild::Failed("no home directory".into());
                    return Task::none();
                };
                tracing::warn!(path = %path.display(), "rebuilding the damaged vault");
                self.vault_ui.rebuild = VaultRebuild::Running;
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || oryxis_vault::rebuild_vault(&path))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r.map_err(|e| e.to_string()))
                    },
                    |r| Message::Vault(VaultMessage::DamagedVaultRebuilt(r)),
                )
            }
            VaultMessage::DamagedVaultRebuilt(result) => {
                match result {
                    Ok(report) => {
                        for t in &report.tables {
                            tracing::info!(
                                table = %t.name,
                                copied = t.copied,
                                unreadable = t.unreadable_stretches,
                                "vault rebuild"
                            );
                        }
                        tracing::warn!(
                            damaged = %report.damaged_path.display(),
                            rows = report.copied_total(),
                            ms = report.elapsed_ms,
                            "vault rebuilt"
                        );
                        self.vault_ui.rebuild = VaultRebuild::Done { report, restart_failed: false };
                    }
                    Err(e) => {
                        tracing::error!("vault rebuild failed: {e}");
                        self.vault_ui.rebuild = VaultRebuild::Failed(e);
                    }
                }
                Task::none()
            }
            VaultMessage::RestartAfterVaultRebuild => {
                if !matches!(self.vault_ui.rebuild, VaultRebuild::Done { .. }) {
                    return Task::none();
                }
                // Never returns on success; on a failed spawn the screen
                // says so instead of offering anything that would touch
                // the rebuilt file.
                self.relaunch_self();
                if let VaultRebuild::Done { restart_failed, .. } = &mut self.vault_ui.rebuild {
                    *restart_failed = true;
                }
                Task::none()
            }
            other => crate::dispatch::unrouted(other),
        }
    }
}

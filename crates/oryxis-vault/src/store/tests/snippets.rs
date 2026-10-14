use super::*;

#[test]
fn save_and_list_snippets() {
    let vault = unlocked_vault();
    let s = Snippet::new("restart-nginx", "sudo systemctl restart nginx");
    vault.save_snippet(&s).unwrap();

    let snippets = vault.list_snippets().unwrap();
    assert_eq!(snippets.len(), 1);
    assert_eq!(snippets[0].command, "sudo systemctl restart nginx");
}


#[test]
fn delete_snippet() {
    let vault = unlocked_vault();
    let s = Snippet::new("temp", "echo hi");
    vault.save_snippet(&s).unwrap();
    vault.delete_snippet(&s.id).unwrap();
    assert_eq!(vault.list_snippets().unwrap().len(), 0);
}

// ── Known Hosts ──


#[test]
fn snippet_has_updated_at() {
    let vault = unlocked_vault();
    let s = Snippet::new("test", "echo hi");
    assert!(s.updated_at.timestamp() > 0);
    vault.save_snippet(&s).unwrap();

    let snippets = vault.list_snippets().unwrap();
    assert_eq!(snippets.len(), 1);
    assert!(snippets[0].updated_at.timestamp() > 0);
}


#[test]
fn snippet_group_and_tags_roundtrip() {
    let vault = unlocked_vault();
    let mut s = Snippet::new("deploy", "make deploy");
    s.group = Some("devops".to_string());
    s.tags = vec!["prod".to_string(), "web".to_string()];
    s.hotkey = Some("ctrl+shift+k".to_string());
    vault.save_snippet(&s).unwrap();

    let back = &vault.list_snippets().unwrap()[0];
    assert_eq!(back.group.as_deref(), Some("devops"));
    assert_eq!(back.tags, vec!["prod", "web"]);
    assert_eq!(back.hotkey.as_deref(), Some("ctrl+shift+k"));

    // Clearing the group persists as NULL, not an empty string.
    let mut s2 = back.clone();
    s2.group = None;
    vault.save_snippet(&s2).unwrap();
    assert_eq!(vault.list_snippets().unwrap()[0].group, None);
}


// ── Install scripts (issue #147) ──


#[test]
fn snippet_install_flag_roundtrips() {
    let vault = unlocked_vault();
    let mut s = Snippet::new("setup", "echo install");
    s.install = true;
    vault.save_snippet(&s).unwrap();
    assert!(vault.list_snippets().unwrap()[0].install);

    // And back off again.
    let mut s2 = vault.list_snippets().unwrap()[0].clone();
    s2.install = false;
    vault.save_snippet(&s2).unwrap();
    assert!(!vault.list_snippets().unwrap()[0].install);
}


#[test]
fn install_runs_record_list_and_refresh() {
    let vault = unlocked_vault();
    let host = uuid::Uuid::new_v4();
    let snip = uuid::Uuid::new_v4();
    vault.record_install_run(&host, &snip).unwrap();
    let runs = vault.list_install_runs().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].0, host);
    assert_eq!(runs[0].1, snip);

    // A re-run REFRESHES the row (hint, not a lock): still one row,
    // with a timestamp at least as new.
    let first = runs[0].2;
    vault.record_install_run(&host, &snip).unwrap();
    let runs = vault.list_install_runs().unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].2 >= first);
}


#[test]
fn deleting_a_snippet_takes_its_install_runs_along() {
    let vault = unlocked_vault();
    let mut s = Snippet::new("setup", "echo install");
    s.install = true;
    vault.save_snippet(&s).unwrap();
    let host = uuid::Uuid::new_v4();
    vault.record_install_run(&host, &s.id).unwrap();
    vault.delete_snippet(&s.id).unwrap();
    assert!(vault.list_install_runs().unwrap().is_empty());
}


#[test]
fn deleting_a_host_takes_its_install_runs_along() {
    let vault = unlocked_vault();
    let conn = Connection::new("box", "10.0.0.1");
    vault.save_connection(&conn, None).unwrap();
    let snip = uuid::Uuid::new_v4();
    vault.record_install_run(&conn.id, &snip).unwrap();
    vault.delete_connection(&conn.id).unwrap();
    assert!(vault.list_install_runs().unwrap().is_empty());
}


#[test]
fn legacy_snippet_payload_defaults_to_not_install() {
    // A snippet serialized by an older peer (sync) or an older export
    // carries no `install` field; it must read as an ordinary snippet.
    let json = r#"{
        "id": "6dc6bfc9-9b2f-4d2f-9a94-1e2a53a24c2b",
        "label": "old",
        "command": "echo hi",
        "description": null,
        "tags": [],
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    }"#;
    let s: Snippet = serde_json::from_str(json).unwrap();
    assert!(!s.install);
}

#[test]
fn snippet_steps_round_trip_through_the_vault() {
    use oryxis_core::models::{SaveFileContent, SaveFileMode, SaveFileStep, SnippetStep, SnippetWait};
    let vault = unlocked_vault();
    let mut snip = Snippet::new("deploy", "");
    snip.set_actions(vec![
        SnippetStep::Command { text: "make deploy".into() },
        SnippetStep::Wait(SnippetWait::CommandEnd { timeout_ms: 0, continue_on_error: true }),
        SnippetStep::SaveFile(SaveFileStep {
            name: "{host}.log".into(),
            mode: SaveFileMode::Append,
            content: SaveFileContent::LastCommand,
        }),
    ]);
    vault.save_snippet(&snip).unwrap();
    let back = vault.list_snippets().unwrap().into_iter().find(|s| s.id == snip.id).unwrap();
    assert_eq!(back.steps, snip.steps);
    assert_eq!(back.command, "make deploy");
}

#[test]
fn a_snippet_of_old_keeps_no_steps_in_the_vault() {
    let vault = unlocked_vault();
    let snip = Snippet::new("ls", "ls -la");
    vault.save_snippet(&snip).unwrap();
    let back = vault.list_snippets().unwrap().into_iter().find(|s| s.id == snip.id).unwrap();
    assert!(back.steps.is_empty());
    assert_eq!(back.command, "ls -la");
}

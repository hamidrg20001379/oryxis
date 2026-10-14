use super::{HotkeyAction, HotkeyBinding};

/// A user-authored ordered list of existing shortcut actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutSequence {
    pub trigger: HotkeyBinding,
    pub steps: Vec<HotkeyAction>,
}

impl ShortcutSequence {
    pub fn serialize_list(items: &[Self]) -> String {
        items
            .iter()
            .map(|item| {
                let steps = item.steps.iter().map(|a| a.id()).collect::<Vec<_>>().join(",");
                format!("{}>{steps}", item.trigger.serialize())
            })
            .collect::<Vec<_>>()
            .join(";")
    }

    pub fn parse_list(value: &str) -> Vec<Self> {
        value
            .split(';')
            .filter_map(|entry| {
                let (trigger, steps) = entry.split_once('>')?;
                let trigger = HotkeyBinding::parse(trigger)?;
                let steps = steps
                    .split(',')
                    .filter_map(HotkeyAction::from_id)
                    .collect::<Vec<_>>();
                (!steps.is_empty()).then_some(Self { trigger, steps })
            })
            .collect()
    }
}

/// Shortcuts-panel editor state. The trigger is text so users can paste a
/// chord such as `ctrl+alt+q`; it is validated when saved.
#[derive(Debug, Clone, Default)]
pub struct ShortcutSequenceForm {
    pub open: bool,
    pub editing: Option<usize>,
    pub trigger: String,
    pub steps: Vec<HotkeyAction>,
    pub selected_step: Option<HotkeyAction>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_settings_round_trip() {
        let items = vec![ShortcutSequence {
            trigger: HotkeyBinding::parse("ctrl+alt+q").unwrap(),
            steps: vec![HotkeyAction::TerminalClearScrollback, HotkeyAction::TerminalCopyScreen],
        }];
        let encoded = ShortcutSequence::serialize_list(&items);
        assert_eq!(ShortcutSequence::parse_list(&encoded), items);
    }
}



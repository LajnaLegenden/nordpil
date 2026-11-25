use super::*;

#[test]
fn ctrl_shift_l_is_lock_not_focus_editor() {
    assert_eq!(
        press(Modifiers::COMMAND | Modifiers::SHIFT, Key::L),
        vec![AppCommand::Lock]
    );
}

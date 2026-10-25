use std::ffi::OsString;

#[test]
fn usage_is_a_command_not_a_chat_prompt() {
    let command = canter::parse([OsString::from("usage")]).unwrap();
    assert_eq!(format!("{command:?}"), "Usage");
    assert!(canter::parse([OsString::from("usage"), OsString::from("extra")]).is_err());
    assert!(matches!(
        canter::parse([OsString::from("--"), OsString::from("usage")]).unwrap(),
        canter::Command::Chat(_)
    ));
}

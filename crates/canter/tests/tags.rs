use canter::{Command, parse};

#[test]
fn repeatable_tags_are_accepted_only_for_new_sessions() {
    let parsed =
        parse(["--tag", "task=old", "--tag", "task=new=1", "--tag", "run=2"].map(Into::into));
    assert!(matches!(parsed, Ok(Command::Chat(_))), "{parsed:?}");
    let Ok(Command::Chat(args)) = parsed else {
        unreachable!()
    };
    assert_eq!(args.tags["task"], "new=1");
    assert_eq!(args.tags["run"], "2");
    for args in [
        vec!["--tag", "missing"],
        vec!["--tag", "=value"],
        vec!["--tag"],
        vec!["--tag", "task=1", "-c"],
        vec!["--tag", "task=1", "--resume", "s"],
    ] {
        assert!(parse(args.into_iter().map(Into::into)).is_err());
    }
}

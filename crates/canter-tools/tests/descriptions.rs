use canter_tools::Tool;

#[test]
fn builtin_tool_descriptions_match_the_system_prompt_plan() {
    for (name, expected) in [
        ("read", include_str!("data/descriptions/read.txt")),
        ("write", include_str!("data/descriptions/write.txt")),
        ("edit", include_str!("data/descriptions/edit.txt")),
        ("shell", include_str!("data/descriptions/shell.txt")),
        ("ask_user", include_str!("data/descriptions/ask_user.txt")),
        ("web_fetch", include_str!("data/descriptions/web_fetch.txt")),
    ] {
        let tool = Tool::from_name(name).unwrap();
        assert_eq!(tool.definition().description, expected, "{name}");
    }
}

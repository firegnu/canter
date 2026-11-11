use canter_tools::Tool;

fn interpreter() -> &'static str {
    use std::os::unix::fs::PermissionsExt;
    let bash = cfg!(target_os = "linux")
        && ["/bin/bash", "/usr/bin/bash"].iter().any(|path| {
            std::fs::metadata(path).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        });
    if bash { "bash" } else { "/bin/sh" }
}

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
        // The recorded text is macOS's interpreter; Linux says bash when installed.
        let expected = expected.replacen("/bin/sh -c", &format!("{} -c", interpreter()), 1);
        assert_eq!(
            tool.definition().description.as_ref(),
            expected.as_str(),
            "{name}"
        );
    }
}

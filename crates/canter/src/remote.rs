use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Add(String),
    List,
    Remove(String),
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["token", "add", name] => Ok(Command::Add((*name).into())),
        ["token", "list"] => Ok(Command::List),
        ["token", "remove", name] => Ok(Command::Remove((*name).into())),
        _ => Err("use canter remote token add <name>, list, or remove <name>".into()),
    }
}

pub fn run(command: Command, home: &Path) -> Result<String, String> {
    use canter_server::remote_tokens;
    match command {
        Command::Add(name) => remote_tokens::add(home, &name).map(|token| format!("{token}\n")),
        Command::List => remote_tokens::list(home).map(|records| {
            records
                .into_iter()
                .map(|(name, created)| format!("{name}\t{created}\n"))
                .collect()
        }),
        Command::Remove(name) => {
            remote_tokens::remove(home, &name).map(|()| "Token removed.\n".into())
        }
    }
    .map_err(|error| error.to_string())
}

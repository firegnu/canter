use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    AddServer { name: String, url: String },
    ListServers,
    RemoveServer(String),
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
        ["add", name, url] => Ok(Command::AddServer {
            name: (*name).into(),
            url: (*url).into(),
        }),
        ["list"] => Ok(Command::ListServers),
        ["remove", name] => Ok(Command::RemoveServer((*name).into())),
        ["token", "add", name] => Ok(Command::Add((*name).into())),
        ["token", "list"] => Ok(Command::List),
        ["token", "remove", name] => Ok(Command::Remove((*name).into())),
        _ => Err(
            "use canter remote add <name> <url>, list, remove <name>, or token add/list/remove"
                .into(),
        ),
    }
}

pub fn run(command: Command, home: &Path) -> Result<String, String> {
    use canter_server::remote_tokens;
    match command {
        Command::AddServer { name, url } => {
            return crate::remotes::add(home, name, url, std::io::stdin().lock())
                .map(|()| "Server added.\n".into());
        }
        Command::ListServers => {
            return crate::remotes::read(home).map(|records| {
                records
                    .into_iter()
                    .map(|r| format!("{}\t{}\n", r.name, r.url))
                    .collect()
            });
        }
        Command::RemoveServer(name) => {
            return crate::remotes::remove(home, &name).map(|()| "Server removed.\n".into());
        }
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

use std::{
    io::{IsTerminal, Write},
    path::Path,
};

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

pub async fn run(command: Command, home: &Path) -> Result<String, String> {
    use canter_server::remote_tokens;
    match command {
        Command::AddServer { name, url } => {
            crate::remotes::check_new(home, &name, &url)?;
            let stdin = std::io::stdin();
            let result = if stdin.is_terminal() {
                let token = terminal_token(&stdin).await?;
                crate::remotes::add(home, name, url, token.as_slice())
            } else {
                crate::remotes::add(home, name, url, stdin.lock())
            };
            return result.map(|()| "Server added.\n".into());
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

async fn terminal_token(stdin: &std::io::Stdin) -> Result<Vec<u8>, String> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::sys::termios::{LocalFlags, SetArg, Termios, tcgetattr, tcsetattr};
    struct Restore<'a>(&'a std::io::Stdin, Termios, OFlag);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = tcsetattr(self.0, SetArg::TCSANOW, &self.1);
            let _ = fcntl(self.0, FcntlArg::F_SETFL(self.2));
        }
    }
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|_| "cannot listen for terminal interrupt")?;
    let original = tcgetattr(stdin).map_err(|_| "cannot read terminal settings")?;
    let flags = OFlag::from_bits_truncate(
        fcntl(stdin, FcntlArg::F_GETFL).map_err(|_| "cannot read stdin flags")?,
    );
    let mut quiet = original.clone();
    quiet
        .local_flags
        .remove(LocalFlags::ECHO | LocalFlags::ECHONL);
    quiet.local_flags.insert(LocalFlags::ICANON);
    let _restore = Restore(stdin, original, flags);
    tcsetattr(stdin, SetArg::TCSANOW, &quiet).map_err(|_| "cannot disable terminal echo")?;
    fcntl(stdin, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
        .map_err(|_| "cannot set stdin nonblocking")?;
    let fd = nix::unistd::dup(stdin).map_err(|_| "cannot duplicate terminal input")?;
    // SAFETY: AsyncFd owns this descriptor, which is never replaced or closed
    // until it is deregistered and dropped.
    let input = unsafe {
        tokio::io::unix::AsyncFd::register_with_interest(fd, tokio::io::Interest::READABLE)
    }
    .map_err(|_| "cannot watch terminal input")?;
    {
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "Paste the token, then press Enter")
            .and_then(|()| stderr.flush())
            .map_err(|_| "cannot write token prompt")?;
    }
    // Canonical terminal reads return one line. Readiness keeps Ctrl+C
    // cancellable without leaving a blocking stdin thread behind.
    let read = async {
        let mut token = vec![0; 8195];
        loop {
            let mut ready = input
                .readable()
                .await
                .map_err(|_| "cannot read token from stdin")?;
            if let Ok(result) = ready.try_io(|fd| {
                nix::unistd::read(fd.get_ref(), &mut token).map_err(std::io::Error::from)
            }) {
                token.truncate(result.map_err(|_| "cannot read token from stdin")?);
                return Ok(token);
            }
        }
    };
    tokio::select! {
        result = read => result,
        _ = interrupt.recv() => Err("token input interrupted".into()),
    }
}

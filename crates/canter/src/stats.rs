#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub selector: Option<String>,
    pub last: bool,
    pub all: bool,
    pub json: bool,
    pub html: bool,
}

pub fn parse(words: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    for word in words {
        match word.as_str() {
            "--last" => args.last = true,
            "--all" => args.all = true,
            "--json" => args.json = true,
            "--html" => args.html = true,
            option if option.starts_with('-') => {
                return Err(format!("unknown stats option: {option}"));
            }
            _ if args.selector.is_none() => args.selector = Some(word.clone()),
            _ => return Err("stats accepts only one session number or ID".into()),
        }
    }
    if args.last && args.selector.is_some() {
        return Err("use either --last or a session number/ID".into());
    }
    if args.all && (args.last || args.selector.is_some()) {
        return Err("--all is only supported for the session list".into());
    }
    if args.json && args.html {
        return Err("use either --json or --html".into());
    }
    if args.html && !args.last && args.selector.is_none() {
        return Err("--html needs a session number/ID or --last".into());
    }
    Ok(args)
}

pub async fn run(args: Args, context: crate::Context) -> Result<(), String> {
    use std::io::Write;
    let connection = canter_server::ensure_server(&context.state_dir, &context.executable)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(warning) = &connection.version_warning {
        eprintln!("{warning}");
    }
    let client = crate::connected(connection)?.client;
    if args.selector.is_none() && !args.last {
        let list = client
            .stats_list(args.all)
            .await
            .map_err(|error| error.to_string())?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&list).map_err(|error| error.to_string())?
            );
        } else {
            print!("{}", canter_tui::stats::format_list(&list));
        }
        return Ok(());
    }
    let id = if args.last
        || args
            .selector
            .as_ref()
            .is_some_and(|id| id.bytes().all(|b| b.is_ascii_digit()))
    {
        let number = if args.last {
            1
        } else {
            args.selector
                .as_ref()
                .unwrap()
                .parse::<usize>()
                .map_err(|_| "invalid session number")?
        };
        if number == 0 {
            return Err("session numbers start at 1".into());
        }
        let list = client
            .stats_list(number > 20)
            .await
            .map_err(|error| error.to_string())?;
        list.data
            .into_iter()
            .find(|row| row.number == number)
            .ok_or("session number not found; run canter stats --all")?
            .session_id
    } else {
        args.selector.unwrap()
    };
    let report = client.stats(&id).await.map_err(|error| error.to_string())?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
    } else if args.html {
        let html = canter_tui::stats::html_report(&report);
        let directory = context.state_dir.join("stats");
        let path = tokio::task::spawn_blocking(move || -> Result<_, std::io::Error> {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&directory)?;
            let mut file = tempfile::Builder::new()
                .prefix("session-")
                .suffix(".html")
                .tempfile_in(directory)?;
            file.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file.write_all(html.as_bytes())?;
            file.as_file().sync_all()?;
            Ok(file.keep()?.1)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        println!("{}", path.display());
        std::io::stdout()
            .flush()
            .map_err(|error| error.to_string())?;
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let opened = tokio::process::Command::new(program)
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .output()
            .await;
        if !opened.is_ok_and(|output| output.status.success()) {
            return Err(format!(
                "could not open the browser; report saved at {}",
                path.display()
            ));
        }
    } else {
        print!("{}", canter_tui::stats::format_report(&report));
    }
    Ok(())
}

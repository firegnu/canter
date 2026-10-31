use std::io::Write;
use std::process::ExitCode;

use canter::{ChatArgs, Command, Context, USAGE};

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match canter::parse(arguments.clone()) {
        Ok(Command::Serve {
            replace: true,
            foreground: false,
        }) => replace().await,
        Ok(Command::Serve { replace, .. }) => canter_server::serve(replace, canter::VERSION)
            .await
            .map_err(|error| error.to_string()),
        Ok(Command::Chat(args)) => chat(args).await,
        Ok(Command::Exec(args)) => {
            let result = match Context::current() {
                Ok(context) => canter::exec::run(args, context).await,
                Err(error) => canter::exec::startup_error(args.json, &error),
            };
            return match result {
                Ok(code) => ExitCode::from(code),
                Err(error) => {
                    eprintln!("canter: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        Ok(Command::Mcp(command)) => match canter_server::state_directory() {
            Ok(home) => canter::mcp::run(command, &home)
                .await
                .map(|output| print!("{output}")),
            Err(error) => Err(error.to_string()),
        },
        Ok(Command::Usage) => match Context::current() {
            Ok(context) => canter::usage(context)
                .await
                .map(|report| print!("{report}")),
            Err(error) => Err(error),
        },
        Ok(Command::Stats(args)) => match Context::current() {
            Ok(context) => canter::stats::run(args, context).await,
            Err(error) => Err(error),
        },
        Ok(Command::Help) => {
            print!("{USAGE}");
            Ok(())
        }
        Ok(Command::Version) => {
            println!("canter {}", canter::VERSION);
            Ok(())
        }
        Err(message) => {
            if arguments.first().is_some_and(|arg| arg == "exec") {
                let json = arguments
                    .iter()
                    .take_while(|arg| *arg != "--")
                    .any(|arg| arg == "--json");
                let code = canter::exec::startup_error(json, &message).unwrap_or(1);
                return ExitCode::from(code);
            }
            eprintln!("canter: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "canter: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn replace() -> Result<(), String> {
    let home = canter_server::state_directory().map_err(|error| error.to_string())?;
    let executable = std::env::current_exe()
        .map_err(|_| "cannot find the canter executable's own path".to_owned())?;
    let connection = canter_server::replace_server(&home, &executable)
        .await
        .map_err(|error| error.to_string())?;
    println!(
        "canter server ready · {} · pid {}",
        canter::VERSION,
        connection.registration.pid
    );
    Ok(())
}

async fn chat(args: ChatArgs) -> Result<(), String> {
    let launch = canter::prepare(args, Context::current()?).await?;
    let session = canter_tui::run(launch)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(id) = session {
        // The controlling terminal may have disappeared (SIGHUP/EIO).
        let _ = writeln!(std::io::stdout(), "To resume: canter --resume {id}");
    }
    Ok(())
}

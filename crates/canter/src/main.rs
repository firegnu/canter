use std::io::Write;
use std::process::ExitCode;

use canter::{ChatArgs, Command, Context, USAGE};

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|arg| arg == canter_server::WORKSPACE_WORKER_ARGUMENT)
    {
        return canter_server::workspace_worker_main();
    }
    if arguments
        .first()
        .is_some_and(|arg| arg == canter_tools::pdf::WORKER_ARGUMENT)
    {
        return canter_tools::pdf::worker_main();
    }
    let parsed = canter::parse(arguments.clone());
    if matches!(
        parsed,
        Ok(Command::Serve {
            foreground: true,
            ..
        })
    ) && let Some(environment) = canter_server::service_environment()
    {
        for (name, value) in environment {
            // No runtime or other threads exist yet; children must inherit these values.
            unsafe {
                std::env::set_var(name, value);
            }
        }
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("create Tokio runtime")
        .block_on(run(arguments, parsed))
}

async fn run(arguments: Vec<std::ffi::OsString>, parsed: Result<Command, String>) -> ExitCode {
    let (parsed, server) = match parsed {
        Ok(Command::WithServer { name, command }) => (Ok(*command), Some(name)),
        other => (other, None),
    };
    let context = || {
        Context::current().map(|mut context| {
            context.server = server.clone();
            context
        })
    };
    let result = match parsed {
        Ok(Command::WithServer { .. }) => unreachable!(),
        Ok(Command::Serve {
            replace: true,
            foreground: false,
        }) => replace().await,
        Ok(Command::Serve { replace, .. }) => canter_server::serve(replace, canter::VERSION)
            .await
            .map_err(|error| error.to_string()),
        Ok(Command::Chat(args)) => match context() {
            Ok(context) => chat(args, context).await,
            Err(error) => Err(error),
        },
        Ok(Command::Service(command)) => {
            canter::service::run(command).map(|output| print!("{output}"))
        }
        Ok(Command::Deployment(command)) => canter::deployment::run(command).await,
        Ok(Command::Skills { force }) => canter::skills::install(force)
            .map(|output| print!("{output}"))
            .map_err(|error| error.to_string()),
        Ok(Command::Exec(args)) => {
            let result = match context() {
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
        Ok(Command::Remote(command)) => match canter_server::state_directory() {
            Ok(home) => canter::remote::run(command, &home)
                .await
                .map(|output| print!("{output}")),
            Err(error) => Err(error.to_string()),
        },
        Ok(Command::Usage) => match context() {
            Ok(context) => canter::usage(context)
                .await
                .map(|report| print!("{report}")),
            Err(error) => Err(error),
        },
        Ok(Command::Doctor) => match context() {
            Ok(context) => {
                return match canter::doctor::run(context).await {
                    Ok(code) => ExitCode::from(code),
                    Err(error) => {
                        eprintln!("canter: {error}");
                        ExitCode::FAILURE
                    }
                };
            }
            Err(error) => Err(error),
        },
        Ok(Command::Stats(args)) => match context() {
            Ok(context) => canter::stats::run(args, context).await,
            Err(error) => Err(error),
        },
        Ok(Command::Auth(command)) => match context() {
            Ok(context) => canter::auth::run(command, context).await,
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
            if arguments.first().is_some_and(|arg| arg == "auth") {
                eprintln!("canter: {message}");
                return ExitCode::FAILURE;
            }
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

async fn chat(args: ChatArgs, context: Context) -> Result<(), String> {
    let server = context
        .server
        .as_ref()
        .map(|name| format!(" --server '{}'", name.replace('\'', "'\\''")))
        .unwrap_or_default();
    let launch = canter::prepare(args, context).await?;
    let session = canter_tui::run(launch)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(id) = session {
        // The controlling terminal may have disappeared (SIGHUP/EIO).
        let _ = writeln!(std::io::stdout(), "To resume: canter{server} --resume {id}");
    }
    Ok(())
}

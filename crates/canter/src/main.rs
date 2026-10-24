use std::process::ExitCode;

use canter::{ChatArgs, Command, Context, USAGE};

#[tokio::main]
async fn main() -> ExitCode {
    let result = match canter::parse(std::env::args_os().skip(1)) {
        Ok(Command::Serve { replace }) => canter_server::serve(replace)
            .await
            .map_err(|error| error.to_string()),
        Ok(Command::Chat(args)) => chat(args).await,
        Ok(Command::Help) => {
            print!("{USAGE}");
            Ok(())
        }
        Err(message) => {
            eprintln!("canter: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("canter: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn chat(args: ChatArgs) -> Result<(), String> {
    let launch = canter::prepare(args, Context::current()?).await?;
    let session = canter_tui::run(launch)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(id) = session {
        println!("To resume: canter --resume {id}");
    }
    Ok(())
}

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let replace = match args.as_slice() {
        [serve] if serve == "serve" => false,
        [serve, replace] if serve == "serve" && replace == "--replace" => true,
        _ => {
            eprintln!("用法：canter serve [--replace]");
            return ExitCode::FAILURE;
        }
    };
    match canter_server::serve(replace).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("canter: {error}");
            ExitCode::FAILURE
        }
    }
}

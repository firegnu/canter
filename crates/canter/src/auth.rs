use canter_protocol::*;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{self, IsTerminal, Write},
    process::Stdio,
    time::Duration,
};

const LOGIN_USAGE: &str = "Usage: canter auth login <provider>\nProviders: chatgpt\n";
const MACHINE: &str = if cfg!(target_os = "macos") {
    "Mac"
} else {
    "machine"
};
const USAGE: &str = "Usage:
  canter auth login <provider>
  canter auth status [<provider>]
  canter auth use <provider> <account>
  canter auth background <provider> on|off
  canter auth logout <provider> [<account>]
Providers: chatgpt
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Login,
    Status,
    Use(String),
    Background(bool),
    Logout(Option<String>),
    Help,
    LoginHelp,
}

pub fn parse(words: &[String]) -> Result<Command, String> {
    fn provider(value: &str) -> Result<(), String> {
        if value == "chatgpt" {
            Ok(())
        } else {
            Err("Unsupported provider. Providers: chatgpt".into())
        }
    }
    let words: Vec<_> = words.iter().map(String::as_str).collect();
    Ok(match words.as_slice() {
        [] | ["--help" | "-h"] => Command::Help,
        ["login"] => Command::LoginHelp,
        ["login", name] => {
            provider(name)?;
            Command::Login
        }
        ["status"] => Command::Status,
        ["status", name] => {
            provider(name)?;
            Command::Status
        }
        ["use", name, account] => {
            provider(name)?;
            Command::Use((*account).into())
        }
        ["background", name, value @ ("on" | "off")] => {
            provider(name)?;
            Command::Background(*value == "on")
        }
        ["logout", name] => {
            provider(name)?;
            Command::Logout(None)
        }
        ["logout", name, account] => {
            provider(name)?;
            Command::Logout(Some((*account).into()))
        }
        _ => return Err(USAGE.into()),
    })
}

pub async fn run(command: Command, context: crate::Context) -> Result<(), String> {
    match command {
        Command::Help => {
            print!("{USAGE}");
            return Ok(());
        }
        Command::LoginHelp => {
            print!("{LOGIN_USAGE}");
            return Ok(());
        }
        _ => {}
    }
    let consent = if command == Command::Login {
        println!("Continue with ChatGPT\n");
        println!(
            "canter keeps your ChatGPT sign-in on this {MACHINE} only, in\n{} (readable only by you). When you use a\nChatGPT model, canter sends your prompts, the project files it\nreads and tool results to OpenAI. Run `canter auth logout chatgpt`\nto sign out and revoke access.\n",
            plain(&context.state_dir.join("auth/chatgpt").display().to_string())
        );
        if io::stdin().is_terminal() {
            print!(
                "Keep running tasks after you close the terminal?\nTasks you started keep using your ChatGPT plan on the canter\nserver. You can change this with\n`canter auth background chatgpt on|off`. [y/N] "
            );
            io::stdout()
                .flush()
                .map_err(|_| "cannot write the consent prompt")?;
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .map_err(|_| "cannot read background consent")?;
            matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
        } else {
            println!("Background tasks after closing the terminal: off.\n");
            false
        }
    } else {
        false
    };
    let connection = canter_server::ensure_server(&context.state_dir, &context.executable)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(warning) = connection.version_warning {
        eprintln!("{warning}");
    }
    let client = connection.client;
    let info = client
        .get("http://localhost/v1/info")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| "cannot read server capabilities")?
        .json::<InfoResponse>()
        .await
        .map_err(|_| "invalid server capabilities")?;
    if !info
        .features
        .unwrap_or_default()
        .iter()
        .any(|feature| feature == "accounts")
    {
        return Err("This server does not support accounts. Run `canter serve --replace` with the updated canter.".into());
    }
    let provider = "chatgpt".to_owned();
    match command {
        Command::Login => login(&client, consent).await?,
        Command::Status => {
            let status: AccountReadResponse = call(
                &client,
                routes::ACCOUNT_READ,
                &AccountReadParams { provider },
            )
            .await?;
            print_status(&status);
        }
        Command::Use(account) => {
            let status: AccountReadResponse = call(
                &client,
                routes::ACCOUNT_SELECT,
                &AccountSelectParams { provider, account },
            )
            .await?;
            print_status(&status);
        }
        Command::Background(enabled) => {
            let status: AccountReadResponse = call(
                &client,
                routes::ACCOUNT_BACKGROUND,
                &AccountBackgroundParams { provider, enabled },
            )
            .await?;
            print_status(&status);
        }
        Command::Logout(account) => {
            let result: AccountLogoutResponse = call(
                &client,
                routes::ACCOUNT_LOGOUT,
                &AccountLogoutParams {
                    provider,
                    account: account.clone(),
                },
            )
            .await?;
            let name = plain(
                result
                    .account
                    .email
                    .as_deref()
                    .unwrap_or(&result.account.id),
            );
            match result.result {
                AccountLogoutResult::Revoked => println!("Signed out {name}. Access revoked."),
                AccountLogoutResult::NoToken => {
                    println!("Signed out {name} on this {MACHINE}. No token needed revocation.")
                }
                AccountLogoutResult::Pending => {
                    println!(
                        "Signed out {name} on this {MACHINE}. OpenAI did not confirm\nthe revocation. You can disconnect canter in ChatGPT Settings."
                    );
                }
            }
        }
        Command::Help | Command::LoginHelp => unreachable!(),
    }
    Ok(())
}

async fn call<P: Serialize, R: DeserializeOwned>(
    client: &reqwest::Client,
    route: &str,
    params: &P,
) -> Result<R, String> {
    let timeout = match route {
        routes::ACCOUNT_LOGIN_WAIT => 660,
        // Revocation retries discovery and the revoke request three times;
        // relogin may also need to finish that cleanup before returning its URL.
        routes::ACCOUNT_LOGIN_START | routes::ACCOUNT_LOGOUT => 120,
        _ => 30,
    };
    let response = client
        .post(format!("http://localhost{route}"))
        .timeout(Duration::from_secs(timeout))
        .json(params)
        .send()
        .await
        .map_err(|_| "account request failed; check the local canter server")?;
    if !response.status().is_success() {
        return Err(response
            .json::<ErrorResponse>()
            .await
            .map(|error| plain(&error.message))
            .unwrap_or_else(|_| "account request failed".into()));
    }
    response
        .json()
        .await
        .map_err(|_| "invalid account response".into())
}

async fn login(client: &reqwest::Client, consent: bool) -> Result<(), String> {
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|_| "cannot listen for Ctrl+C")?;
    let started: AccountLoginStartResponse = call(
        client,
        routes::ACCOUNT_LOGIN_START,
        &AccountLoginStartParams {
            provider: "chatgpt".into(),
            account_id: None,
            background_consent: consent,
        },
    )
    .await?;
    println!("Opening your browser to continue with ChatGPT...");
    if !open_browser(&started.auth_url).await {
        println!(
            "If it does not open, paste this link into a browser on this {MACHINE}:\n{}",
            started.auth_url
        );
    }
    println!("Waiting for sign-in (Ctrl+C to cancel)...");
    io::stdout()
        .flush()
        .map_err(|_| "cannot write sign-in status")?;
    let params = AccountLoginParams {
        provider: "chatgpt".into(),
        login_id: started.login_id,
    };
    let done: AccountLoginCompleted = tokio::select! {
        biased;
        _ = interrupt.recv() => {
            let _: AccountLoginCancelResponse = call(client, routes::ACCOUNT_LOGIN_CANCEL, &params).await?;
            let _: AccountLoginCompleted = call(client, routes::ACCOUNT_LOGIN_WAIT, &params).await?;
            return Err("Sign-in canceled.".into());
        }
        result = call(client, routes::ACCOUNT_LOGIN_WAIT, &params) => result?,
    };
    if !done.success {
        return Err(done.error.unwrap_or_else(|| "Sign-in failed.".into()));
    }
    let account = done.account.ok_or("sign-in completed without an account")?;
    println!(
        "Signed in as {}.",
        plain(account.email.as_deref().unwrap_or(&account.id))
    );
    if !account.plan_authorized {
        println!("Signed in, but ChatGPT plan usage was not authorized.");
    } else if done.show_plan_notice {
        println!(
            "\nYou're using your ChatGPT plan\nRequests to ChatGPT models from canter use your ChatGPT plan.\nManage usage: https://chatgpt.com/settings/usage"
        );
    }
    Ok(())
}

async fn open_browser(url: &str) -> bool {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut command = tokio::process::Command::new(program);
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    matches!(tokio::time::timeout(Duration::from_secs(10), command.status()).await, Ok(Ok(status)) if status.success())
}

fn plain(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

fn print_status(status: &AccountReadResponse) {
    println!("{}", status.provider);
    if status.accounts.is_empty() {
        println!("No accounts. Run `canter auth login chatgpt` to sign in.");
    }
    for (index, account) in status.accounts.iter().enumerate() {
        let current = if status.current_account_id.as_deref() == Some(&account.id) {
            '*'
        } else {
            ' '
        };
        let state = match account.status {
            AccountStatus::LoggedIn => "signed in · plan usage",
            AccountStatus::PlanNotAuthorized => "signed in · plan not authorized",
            AccountStatus::NeedsLogin => "needs sign-in",
            AccountStatus::SignedOut => "signed out",
        };
        println!(
            "{current} {}  {}  {state} · background {}{}",
            index + 1,
            plain(account.email.as_deref().unwrap_or(&account.id)),
            if account.background_consent {
                "on"
            } else {
                "off"
            },
            if account.pending_revocation {
                " · revocation pending"
            } else {
                ""
            }
        );
    }
}

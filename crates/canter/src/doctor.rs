use canter_protocol::{DoctorResponse, DoctorStatus};

pub async fn run(context: crate::Context) -> Result<u8, String> {
    let report = match canter_server::discover(&context.state_dir).await {
        Ok(Some(connection)) => {
            let client = crate::connected(connection)?.client;
            match client.doctor(crate::VERSION).await {
                Ok(report) => report,
                Err(error) => {
                    let reason = match error {
                        canter_tui::client::ClientError::Protocol(message) => Some(message),
                        _ => None,
                    };
                    canter_server::doctor_offline(context.state_dir, crate::VERSION.into(), reason)
                        .await
                }
            }
        }
        Ok(None) | Err(_) => {
            canter_server::doctor_offline(context.state_dir, crate::VERSION.into(), None).await
        }
    };
    print(&report);
    Ok(u8::from(problems(&report) > 0))
}

fn problems(report: &DoctorResponse) -> usize {
    report
        .data
        .iter()
        .filter(|check| check.status == DoctorStatus::Fail)
        .count()
}

fn print(report: &DoctorResponse) {
    for check in &report.data {
        let status = match check.status {
            DoctorStatus::Ok => "ok",
            DoctorStatus::Fail => "fail",
            DoctorStatus::Skip => "skip",
        };
        println!("{status} {}", check.name);
        if let Some(reason) = &check.reason {
            for line in reason.lines() {
                println!("  {line}");
            }
        }
        if let Some(fix) = &check.fix {
            println!("  Fix: {fix}");
        }
    }
    let problems = problems(report);
    println!(
        "{problems} {}",
        if problems == 1 { "problem" } else { "problems" }
    );
}

use super::{Command, CommandCheck, Context};

pub(super) struct CriticalPaths;

impl CommandCheck for CriticalPaths {
    fn check(&self, command: &Command, context: &Context) -> Option<String> {
        if !matches!(command.program.as_str(), "rm" | "rmdir") {
            return None;
        }
        let reason = |target: &str| format!("rm/rmdir on a critical path ({target})");
        let mut options = true;
        for arg in &command.arguments {
            if let Some(value) = &arg.value {
                if options && value == "--" {
                    options = false;
                    continue;
                }
                if options && value.starts_with('-') {
                    continue;
                }
            }
            if arg.paths.as_ref().is_none_or(|paths| {
                paths.iter().any(|path| {
                    path.components().count() <= 2
                        || context.home.as_ref() == Some(path)
                        || context
                            .working_directories
                            .iter()
                            .any(|root| root.starts_with(path))
                })
            }) {
                return Some(reason(&arg.raw));
            }
        }
        command.uncertain.then(|| reason(&command.source))
    }
}

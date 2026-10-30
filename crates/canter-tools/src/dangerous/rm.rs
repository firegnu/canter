use super::{Command, CommandCheck, Context};
use std::path::{Path, PathBuf};

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
                paths
                    .iter()
                    .any(|path| critical_target(path, arg.glob, context))
            }) {
                return Some(reason(&arg.raw));
            }
        }
        command.uncertain.then(|| reason(&command.source))
    }
}

fn critical_target(path: &Path, glob: bool, context: &Context) -> bool {
    let critical = |path: &Path| {
        path.components().count() <= 2
            || context.home.as_deref() == Some(path)
            || context
                .working_directories
                .iter()
                .any(|root| root.starts_with(path))
    };
    if glob {
        let mut parent = PathBuf::new();
        for component in path.components() {
            let name = component.as_os_str().to_string_lossy();
            if name.contains(['*', '?', '[']) {
                let whole_layer = name.chars().all(|ch| matches!(ch, '*' | '.'));
                return whole_layer && critical(&parent)
                    || parent == Path::new("/")
                    || context
                        .working_directories
                        .iter()
                        .chain(context.home.iter())
                        .any(|root| root != &parent && root.starts_with(&parent));
            }
            parent.push(component);
        }
    }
    critical(path)
}

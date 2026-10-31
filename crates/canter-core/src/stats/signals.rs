use super::SavedItem;
use canter_protocol::*;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Component, Path, PathBuf},
};

pub(super) fn fill(compactions: &mut [StatsCompaction], items: &[SavedItem], cwd: &str) {
    let mut read = BTreeSet::new();
    let mut changed = BTreeSet::new();
    let mut before_plan = None;
    let mut cursor = 0;
    for index in 0..compactions.len() {
        let boundary = compactions[index].event_seq;
        let next = compactions
            .get(index + 1)
            .map_or(u64::MAX, |row| row.event_seq);
        while let Some(item) = items.get(cursor).filter(|item| item.seq < boundary) {
            match &item.item {
                Item::FileRead {
                    path,
                    status: ItemStatus::Completed,
                    ..
                } => {
                    read.insert(normalize(cwd, path));
                }
                Item::FileChange {
                    changes,
                    status: ItemStatus::Completed,
                    ..
                } => {
                    changed.extend(changes.iter().map(|change| normalize(cwd, &change.path)));
                }
                Item::Plan {
                    plan,
                    status: ItemStatus::Completed,
                    ..
                } => before_plan = Some(plan.clone()),
                _ => {}
            }
            cursor += 1;
        }
        let end = items.partition_point(|item| item.seq < next);
        let after = &items[cursor..end];
        let row = &mut compactions[index];
        if row.succeeded == Some(false) || row.summary.is_none() {
            continue;
        }
        row.signals.reread_files = Some(
            after
                .iter()
                .filter_map(|item| match &item.item {
                    Item::FileRead {
                        path,
                        status: ItemStatus::Completed,
                        ..
                    } if item.seq > boundary => {
                        let path = normalize(cwd, path);
                        read.contains(&path).then_some(path)
                    }
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        );
        row.signals.unmentioned_files = row.summary.as_ref().map(|summary| {
            changed
                .iter()
                .filter(|path| !mentioned(summary, path, cwd))
                .cloned()
                .collect()
        });
        let after_plan = after.iter().find_map(|item| match &item.item {
            Item::Plan {
                plan,
                status: ItemStatus::Completed,
                ..
            } if item.seq > boundary => Some(plan.clone()),
            _ => None,
        });
        row.signals.plan = compare_plans(before_plan.clone(), after_plan);
    }
}

fn normalize(cwd: &str, path: &str) -> String {
    let joined = Path::new(cwd).join(path);
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized.to_string_lossy().into_owned()
}

fn mentioned(summary: &str, path: &str, cwd: &str) -> bool {
    let path = Path::new(path);
    [
        path.to_str(),
        path.strip_prefix(cwd).ok().and_then(Path::to_str),
        path.file_name().and_then(|name| name.to_str()),
    ]
    .into_iter()
    .flatten()
    .filter(|name| !name.is_empty())
    .any(|name| {
        summary.match_indices(name).any(|(start, _)| {
            let before = summary[..start].chars().next_back();
            let tail = &summary[start + name.len()..];
            let after = tail.chars().next();
            let sentence_end = tail
                .strip_prefix('.')
                .is_some_and(|rest| rest.chars().next().is_none_or(|c| !path_character(c)));
            before.is_none_or(|c| !path_character(c))
                && (after.is_none_or(|c| !path_character(c)) || sentence_end)
        })
    })
}

fn path_character(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\')
}

fn compare_plans(before: Option<Vec<PlanStep>>, after: Option<Vec<PlanStep>>) -> PlanSignal {
    let mut signal = PlanSignal {
        before,
        after,
        ..Default::default()
    };
    if let (Some(before), Some(after)) = (&signal.before, &signal.after) {
        let mut by_text = BTreeMap::<&str, VecDeque<&PlanStep>>::new();
        for step in after {
            by_text.entry(step.step.trim()).or_default().push_back(step);
        }
        let mut removed = Vec::new();
        let mut changed = Vec::new();
        for step in before {
            match by_text
                .get_mut(step.step.trim())
                .and_then(VecDeque::pop_front)
            {
                None => removed.push(step.clone()),
                Some(next) if next.status != step.status => changed.push(PlanStatusChange {
                    step: step.step.clone(),
                    before: step.status,
                    after: next.status,
                }),
                _ => {}
            }
        }
        signal.removed = Some(removed);
        signal.status_changed = Some(changed);
    }
    signal
}

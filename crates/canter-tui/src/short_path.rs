//! Paths shown on one row that may not fit: cut in the middle, a directory
//! at a time, so the start and the file name stay.

use crate::text::{char_width, str_width};

/// The fewest columns a path is squeezed into to make room for what follows
/// it on its row; past that the row is cut at its end.
pub(crate) const PATH_MIN: usize = 10;

/// `path` in at most `room` columns. One that fits stays as it is.
/// Otherwise the directories between its first part and the file name go,
/// as one `…`, those nearest the first part first, as many as must:
/// `crates/canter-tui/src/app.rs`, then `crates/…/src/app.rs`, then
/// `crates/…/app.rs`. The first part is `~`, or for an absolute path `/`
/// and the directory after it. Then the first part goes too (`…/app.rs`);
/// when not even the file name fits, its middle does ([`middle`]).
pub(crate) fn fit_path(path: &str, room: usize) -> String {
    if str_width(path) <= room {
        return path.to_owned();
    }
    // A directory's own `/` stays with its name.
    let (body, slash) = match path.strip_suffix('/') {
        Some(body) if !body.is_empty() => (body, "/"),
        _ => (path, ""),
    };
    let (absolute, relative) = match body.strip_prefix('/') {
        Some(rest) => ("/", rest),
        None => ("", body),
    };
    let Some((first, rest)) = relative.split_once('/') else {
        return middle(path, room);
    };
    let (dirs, name) = match rest.rsplit_once('/') {
        Some((dirs, name)) => (dirs.split('/').collect(), name),
        None => (Vec::new(), rest),
    };
    let name = format!("{name}{slash}");
    for keep in (0..dirs.len()).rev() {
        let mut shown = format!("{absolute}{first}/…/");
        for dir in &dirs[dirs.len() - keep..] {
            shown.push_str(dir);
            shown.push('/');
        }
        shown.push_str(&name);
        if str_width(&shown) <= room {
            return shown;
        }
    }
    let shown = format!("…/{name}");
    if str_width(&shown) <= room {
        return shown;
    }
    middle(&name, room)
}

/// `paths` fit as [`fit_path`] fits them into `room` columns between them,
/// shared out evenly; what a short one does not use goes to the others.
pub(crate) fn fit_paths(paths: &[&str], room: usize) -> Vec<String> {
    let mut order: Vec<usize> = (0..paths.len()).collect();
    order.sort_by_key(|&index| str_width(paths[index]));
    let mut fitted = vec![String::new(); paths.len()];
    let mut left = room;
    for (done, &index) in order.iter().enumerate() {
        let share = left / (paths.len() - done);
        fitted[index] = fit_path(paths[index], share);
        left -= str_width(&fitted[index]);
    }
    fitted
}

/// `text` in `room` columns, cut in the middle with `…` when it is longer;
/// the end keeps as much as the start, or one column less.
pub(crate) fn middle(text: &str, room: usize) -> String {
    if str_width(text) <= room {
        return text.to_owned();
    }
    let Some(room) = room.checked_sub(1) else {
        return String::new();
    };
    let take = |chars: &mut dyn Iterator<Item = char>, budget: usize| {
        let mut used = 0;
        let mut kept = Vec::new();
        for c in chars {
            used += char_width(c);
            if used > budget {
                break;
            }
            kept.push(c);
        }
        kept
    };
    let head = take(&mut text.chars(), room - room / 2);
    let mut tail = take(&mut text.chars().rev(), room / 2);
    tail.reverse();
    head.into_iter().chain(['…']).chain(tail).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "crates/canter-tui/src/app.rs";

    #[test]
    fn a_path_that_fits_stays_as_it_is() {
        assert_eq!(fit_path(APP, 28), APP);
        assert_eq!(fit_path(APP, 80), APP);
    }

    #[test]
    fn directories_go_from_the_middle_one_at_a_time() {
        assert_eq!(fit_path(APP, 27), "crates/…/src/app.rs");
        assert_eq!(fit_path(APP, 19), "crates/…/src/app.rs");
        assert_eq!(fit_path(APP, 18), "crates/…/app.rs");
        assert_eq!(fit_path(APP, 15), "crates/…/app.rs");
        let deep = "a/b/c/d/e/f/name.rs";
        assert_eq!(fit_path(deep, 18), "a/…/d/e/f/name.rs");
        assert_eq!(fit_path(deep, 16), "a/…/e/f/name.rs");
        assert_eq!(fit_path(deep, 12), "a/…/name.rs");
    }

    #[test]
    fn then_the_first_part_goes_and_then_the_middle_of_the_name() {
        assert_eq!(fit_path(APP, 14), "…/app.rs");
        assert_eq!(fit_path(APP, 8), "…/app.rs");
        assert_eq!(fit_path(APP, 7), "app.rs");
        assert_eq!(fit_path(APP, 5), "ap…rs");
        let long = "src/a_very_long_file_name.rs";
        assert_eq!(fit_path(long, 12), "a_very…me.rs");
        assert_eq!(fit_path(long, 1), "…");
        assert_eq!(fit_path(long, 0), "");
        // Nothing to leave out between the first part and the name.
        assert_eq!(fit_path("crates/app.rs", 12), "…/app.rs");
        assert_eq!(fit_path("verylongname.rs", 9), "very…e.rs");
    }

    #[test]
    fn home_and_absolute_paths_keep_their_first_directory() {
        let home = "~/Developer/personal_projs/canter/src/app.rs";
        assert_eq!(fit_path(home, 30), "~/…/canter/src/app.rs");
        assert_eq!(fit_path(home, 12), "~/…/app.rs");
        assert_eq!(fit_path(home, 9), "…/app.rs");
        let root = "/Users/dev/projects/canter/src/app.rs";
        assert_eq!(fit_path(root, 30), "/Users/…/canter/src/app.rs");
        assert_eq!(fit_path(root, 16), "/Users/…/app.rs");
        assert_eq!(fit_path(root, 14), "…/app.rs");
        // A directory keeps its `/`.
        assert_eq!(fit_path("crates/canter-tui/src/", 18), "crates/…/src/");
    }

    #[test]
    fn wide_characters_count_two_columns_and_are_never_split() {
        let path = "文档/调研/方案/说明.md";
        assert_eq!(fit_path(path, 21), "文档/…/方案/说明.md");
        assert_eq!(fit_path(path, 16), "文档/…/说明.md");
        assert_eq!(fit_path(path, 9), "…/说明.md");
        assert_eq!(fit_path("目录/目录目录目录目录.md", 8), "目录….md");
        assert!(str_width(&fit_path("目录/目录目录目录目录.md", 8)) <= 8);
    }

    #[test]
    fn two_paths_share_the_room() {
        let from = "crates/canter-tui/src/old.rs";
        let to = "crates/canter-tui/src/new.rs";
        assert_eq!(fit_paths(&[from, to], 56), [from, to]);
        assert_eq!(
            fit_paths(&[from, to], 40),
            ["crates/…/src/old.rs", "crates/…/src/new.rs"]
        );
        // A short one leaves the rest of the room to the other.
        assert_eq!(
            fit_paths(&["a.rs", from], 30),
            ["a.rs", "crates/…/src/old.rs"]
        );
    }

    #[test]
    fn the_middle_of_text_goes() {
        assert_eq!(middle("~/short", 20), "~/short");
        assert_eq!(middle("abcdefghij", 7), "abc…hij");
        assert_eq!(middle("abcdefghij", 6), "abc…ij");
        assert_eq!(middle("abcdefghij", 1), "…");
        assert_eq!(middle("abcdefghij", 0), "");
        // Wide characters never go past the room.
        assert_eq!(middle("目录目录目录目录", 7), "目…录");
    }
}

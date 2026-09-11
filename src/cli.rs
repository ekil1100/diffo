use crate::diff::{self, DiffFile, DiffLine, DiffSnapshot};
use crate::store::{Comment, Store};
use crate::{Error, Result, git, theme, tui, util};
use std::fmt::Write as _;
use std::io::Write as _;

pub fn run(args: &[String]) -> Result<()> {
    let debug_git = args.iter().any(|a| a == "--debug-git");
    let args: Vec<String> = args
        .iter()
        .filter(|a| *a != "--debug-git")
        .cloned()
        .collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => output(HELP_TEXT),
        Some("comments") => comments_command(&args[1..], debug_git),
        Some("review") => review_command(&args[1..], debug_git),
        Some("themes") => themes_command(&args[1..]),
        _ => {
            let target = diff::make_review_target(&args);
            let repo = git::discover_repository(debug_git)?;
            let snapshot = git::load_snapshot(repo, target, debug_git)?;
            let mut store = Store::init(&snapshot.repository.repo_id)?;
            tui::run(&snapshot, &mut store, &util::default_author())
        }
    }
}

fn default_context(debug: bool) -> Result<(DiffSnapshot, Store)> {
    let repo = git::discover_repository(debug)?;
    let snapshot = git::load_snapshot(repo, diff::make_review_target(&[]), debug)?;
    let store = Store::init(&snapshot.repository.repo_id)?;
    Ok((snapshot, store))
}
fn value<'a>(args: &'a [String], index: &mut usize) -> Result<&'a str> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or(Error::InvalidArguments)
}
fn parse_line(text: &str) -> Result<u32> {
    // Zig's decimal parser permits a leading plus and digit separators.
    let negative = text.starts_with('-');
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_')
    {
        return Err(Error::InvalidArguments);
    }
    let number = digits
        .replace('_', "")
        .parse::<u32>()
        .map_err(|_| Error::InvalidArguments)?;
    if negative && number != 0 {
        return Err(Error::InvalidArguments);
    }
    Ok(number)
}
fn comments_command(args: &[String], debug: bool) -> Result<()> {
    let command = args
        .first()
        .map(String::as_str)
        .ok_or(Error::InvalidArguments)?;
    match command {
        "list" | "clean" | "cleanup" => {
            let clean = command != "list";
            let (mut file, mut json, mut dry_run, mut all) = (None, false, false, false);
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--json" => json = true,
                    "--file" => file = Some(value(args, &mut i)?),
                    "--dry-run" if clean => dry_run = true,
                    "--all" if clean => all = true,
                    _ => return Err(Error::InvalidArguments),
                }
                i += 1;
            }
            let (snapshot, mut store) = default_context(debug)?;
            store.refresh_match_status(&snapshot);
            if clean {
                let count = match (all, dry_run) {
                    (true, true) => store.all_comment_count(file),
                    (true, false) => store.remove_all_comments(file)?,
                    (false, true) => {
                        store.outdated_comment_count(&snapshot.review_target.target_id, file)
                    }
                    (false, false) => {
                        store.remove_outdated_comments(&snapshot.review_target.target_id, file)?
                    }
                };
                if json {
                    let mut out = envelope(&snapshot);
                    writeln!(out, ",\n  \"removed_count\": {count},\n  \"dry_run\": {dry_run},\n  \"all\": {all}\n}}").unwrap();
                    output(&out)
                } else {
                    output(&format!(
                        "{} {count} {}\n",
                        if dry_run { "would remove" } else { "removed" },
                        if all { "comments" } else { "outdated comments" }
                    ))
                }
            } else if json {
                let mut out = envelope(&snapshot);
                out.push_str(",\n  \"comments\": [\n");
                let comments: Vec<_> = store
                    .comments
                    .iter()
                    .filter(|c| comment_matches(c, None, file))
                    .map(|c| comment_json(c, "    "))
                    .collect();
                out.push_str(&comments.join(",\n"));
                out.push_str("\n  ]\n}\n");
                output(&out)
            } else {
                let out: String = store
                    .comments
                    .iter()
                    .filter(|c| comment_matches(c, None, file))
                    .map(comment_text)
                    .collect();
                output(if out.is_empty() {
                    "no comments\n"
                } else {
                    &out
                })
            }
        }
        "get" => {
            let id = args.get(1).ok_or(Error::InvalidArguments)?;
            // Preserve the original command's permissive trailing arguments.
            let json = args[2..].iter().any(|a| a == "--json");
            let (snapshot, mut store) = default_context(debug)?;
            store.refresh_match_status(&snapshot);
            let comment = store
                .comments
                .iter()
                .find(|c| c.comment_id == *id)
                .ok_or(Error::InvalidArguments)?;
            output(&if json {
                format!("{}\n", comment_json(comment, ""))
            } else {
                comment_text(comment)
            })
        }
        "add" => {
            let (mut file, mut line, mut end, mut body) = (None, None, 0, None);
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--file" => file = Some(value(args, &mut i)?),
                    "--line" => line = Some(parse_line(value(args, &mut i)?)?),
                    "--end" => end = parse_line(value(args, &mut i)?)?,
                    "--body" => body = Some(value(args, &mut i)?),
                    _ => return Err(Error::InvalidArguments),
                }
                i += 1;
            }
            let (file, line, body) = (
                file.ok_or(Error::InvalidArguments)?,
                line.ok_or(Error::InvalidArguments)?,
                body.ok_or(Error::InvalidArguments)?,
            );
            let (snapshot, mut store) = default_context(debug)?;
            let file = find_file(&snapshot, file)?;
            let (line, header) = find_line(file, line).ok_or(Error::InvalidArguments)?;
            let comment = store.add_comment(
                &snapshot.repository.repo_id,
                &snapshot.review_target.target_id,
                file,
                line,
                header,
                end,
                body,
                &util::default_author(),
            )?;
            output(&comment_text(&comment))
        }
        _ => Err(Error::InvalidArguments),
    }
}
fn review_command(args: &[String], debug: bool) -> Result<()> {
    let command = args
        .first()
        .map(String::as_str)
        .ok_or(Error::InvalidArguments)?;
    if !matches!(command, "status" | "mark") {
        return Err(Error::InvalidArguments);
    }
    let (mut file, mut json, mut reviewed) = (None, false, true);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => file = Some(value(args, &mut i)?),
            "--json" if command == "status" => json = true,
            "--unreviewed" if command == "mark" => reviewed = false,
            "--reviewed" if command == "mark" => reviewed = true,
            _ => return Err(Error::InvalidArguments),
        }
        i += 1;
    }
    if command == "mark" && file.is_none() {
        return Err(Error::InvalidArguments);
    }
    let (snapshot, mut store) = default_context(debug)?;
    if command == "mark" {
        let file = find_file(&snapshot, file.unwrap())?;
        store.set_reviewed(
            &snapshot.repository.repo_id,
            &snapshot.review_target.target_id,
            file,
            reviewed,
        )?;
    }
    output(&review_output(&snapshot, &store, file, json))
}
fn themes_command(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("list") => output(&theme::list_builtins()),
        Some("validate") if args.len() == 2 => {
            output(&format!("{}\n", theme::validate_base_theme_file(&args[1])?))
        }
        _ => Err(Error::InvalidArguments),
    }
}
fn find_file<'a>(snapshot: &'a DiffSnapshot, path: &str) -> Result<&'a DiffFile> {
    snapshot
        .files
        .iter()
        .find(|f| f.path == path)
        .ok_or(Error::InvalidArguments)
}
fn find_line(file: &DiffFile, number: u32) -> Option<(&DiffLine, &str)> {
    file.hunks.iter().find_map(|h| {
        h.lines
            .iter()
            .find(|l| l.new_lineno == Some(number) || l.old_lineno == Some(number))
            .map(|l| (l, h.header.as_str()))
    })
}
fn comment_matches(comment: &Comment, target: Option<&str>, file: Option<&str>) -> bool {
    target.is_none_or(|t| t == comment.review_target_id)
        && file.is_none_or(|f| f == comment.file_path)
}
fn comment_text(c: &Comment) -> String {
    format!(
        "{} {}:{}-{} [{}] {}\n{}\n",
        c.comment_id,
        c.file_path,
        c.start_line,
        c.end_line,
        c.match_status.label(),
        c.author,
        c.body
    )
}
fn quote(value: &str) -> String {
    // Match the original JSON spelling, including hexadecimal backspace/form-feed escapes.
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn envelope(snapshot: &DiffSnapshot) -> String {
    format!(
        "{{\n  \"schema_version\": 1,\n  \"repository_id\": {},\n  \"review_target_id\": {}",
        quote(&snapshot.repository.repo_id),
        quote(&snapshot.review_target.target_id)
    )
}
fn field(out: &mut String, indent: &str, key: &str, value: &str, comma: bool) {
    writeln!(
        out,
        "{indent}  \"{key}\": {value}{}",
        if comma { "," } else { "" }
    )
    .unwrap();
}
fn comment_json(c: &Comment, indent: &str) -> String {
    let mut out = format!("{indent}{{\n");
    field(&mut out, indent, "comment_id", &quote(&c.comment_id), true);
    field(&mut out, indent, "file_path", &quote(&c.file_path), true);
    field(
        &mut out,
        indent,
        "start_line",
        &c.start_line.to_string(),
        true,
    );
    field(&mut out, indent, "end_line", &c.end_line.to_string(), true);
    for (key, value) in [
        ("side", c.side.as_str()),
        ("body", &c.body),
        ("author", &c.author),
        ("match_status", c.match_status.label()),
    ] {
        field(&mut out, indent, key, &quote(value), true);
    }
    writeln!(out, "{indent}  \"anchor\": {{").unwrap();
    field(
        &mut out,
        &format!("{indent}  "),
        "hunk_header",
        &quote(&c.hunk_header),
        true,
    );
    field(
        &mut out,
        &format!("{indent}  "),
        "patch_fingerprint",
        &quote(&c.patch_fingerprint),
        true,
    );
    field(
        &mut out,
        &format!("{indent}  "),
        "stable_line_ids",
        &format!("[{}]", quote(&c.stable_line_id)),
        false,
    );
    writeln!(out, "{indent}  }},").unwrap();
    field(
        &mut out,
        indent,
        "review_target_id",
        &quote(&c.review_target_id),
        false,
    );
    write!(out, "{indent}}}").unwrap();
    out
}
fn review_output(
    snapshot: &DiffSnapshot,
    store: &Store,
    filter: Option<&str>,
    json: bool,
) -> String {
    let mut out = if json {
        format!("{},\n  \"files\": [\n", envelope(snapshot))
    } else {
        String::new()
    };
    let mut emitted = false;
    for f in &snapshot.files {
        if filter.is_some_and(|p| p != f.path) {
            continue;
        }
        let status = store.status_for_file(
            &f.path,
            &f.patch_fingerprint,
            &snapshot.review_target.target_id,
        );
        let count = store.comment_count(&f.path, &snapshot.review_target.target_id);
        if json {
            if emitted {
                out.push_str(",\n");
            }
            write!(out, "    {{\n      \"file_path\": {},\n      \"status\": {},\n      \"patch_fingerprint\": {},\n      \"comment_count\": {count}\n    }}", quote(&f.path), quote(status), quote(&f.patch_fingerprint)).unwrap();
        } else {
            writeln!(
                out,
                "{status} {} comments={count} fingerprint={}",
                f.path, f.patch_fingerprint
            )
            .unwrap();
        }
        emitted = true;
    }
    if json {
        out.push_str("\n  ]\n}\n");
    } else if !emitted {
        out.push_str("no changed files\n");
    }
    out
}
fn output(text: &str) -> Result<()> {
    std::io::stdout()
        .lock()
        .write_all(text.as_bytes())
        .map_err(Error::from)
}

const HELP_TEXT: &str = "diffo - terminal Git diff review\n\nUsage:\n  diffo [git-diff-args]\n  diffo comments list [--file <path>] [--json]\n  diffo comments get <comment-id> [--json]\n  diffo comments add --file <path> --line <n> [--end <n>] --body <text>\n  diffo comments clean [--all] [--file <path>] [--dry-run] [--json]\n  diffo review status [--file <path>] [--json]\n  diffo review mark --file <path> [--reviewed|--unreviewed]\n  diffo themes list\n  diffo themes validate <file>\n\nInteractive keys:\n  j/k line, J/K file, n/p change, C unfold/fold mode, z/Z folds, v stacked/split, r reviewed, c comment, V select, y copy, Esc clear selection, u unreviewed, ? help, q quit\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_line_arguments_follow_zig_parser() {
        for (text, expected) in [
            ("0", 0),
            ("-0", 0),
            ("+01", 1),
            ("1__0", 10),
            ("4_294_967_295", u32::MAX),
        ] {
            assert_eq!(parse_line(text).unwrap(), expected);
        }
        for text in ["", "+", "-1", "_1", "1_", "0x10", " 1", "4294967296"] {
            assert!(parse_line(text).is_err(), "{text}");
        }
    }

    #[test]
    fn json_string_spelling_preserves_control_character_compatibility() {
        let input = "\u{0008}\u{000c}\n\r\t\"\\ 文本";
        assert_eq!(quote(input), "\"\\u0008\\u000c\\n\\r\\t\\\"\\\\ 文本\"");
        assert_eq!(
            serde_json::from_str::<String>(&quote(input)).unwrap(),
            input
        );
    }
}

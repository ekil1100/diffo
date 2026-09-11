use crate::diff::{DiffFile, DiffLine, DiffLineKind, DiffSnapshot};
use crate::{Error, Result, util};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchStatus {
    Exact,
    Relocated,
    Stale,
    Missing,
}
impl MatchStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Relocated => "relocated",
            Self::Stale => "stale",
            Self::Missing => "missing",
        }
    }
    fn parse(label: &str) -> Self {
        match label {
            "relocated" => Self::Relocated,
            "stale" => Self::Stale,
            "missing" => Self::Missing,
            _ => Self::Exact,
        }
    }
    fn outdated(self) -> bool {
        matches!(self, Self::Stale | Self::Missing)
    }
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub comment_id: String,
    pub repository_id: String,
    pub review_target_id: String,
    pub file_path: String,
    pub side: String,
    pub start_line: u32,
    pub end_line: u32,
    pub stable_line_id: String,
    pub hunk_header: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
    pub patch_fingerprint: String,
    pub match_status: MatchStatus,
    pub body: String,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
    // Retain additional schema-v1 anchor IDs and extension fields when saving.
    original: Value,
}

#[derive(Clone, Debug)]
pub struct ReviewState {
    pub repository_id: String,
    pub review_target_id: String,
    pub file_path: String,
    pub status: String,
    pub patch_fingerprint: String,
    pub updated_at: String,
    original: Value,
}

#[derive(Debug)]
pub struct Store {
    pub comments: Vec<Comment>,
    pub states: Vec<ReviewState>,
    pub repo_dir: PathBuf,
    comments_root: Value,
    states_root: Value,
}

impl Store {
    pub fn init(repo_id: &str) -> Result<Self> {
        let base = match std::env::var_os("XDG_STATE_HOME") {
            Some(path) => PathBuf::from(path).join("diffo"),
            None => PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
                .join(".local/state/diffo"),
        };
        Self::at(base.join("repos").join(repo_id))
    }
    pub fn at(repo_dir: impl AsRef<Path>) -> Result<Self> {
        let repo_dir = repo_dir.as_ref().to_path_buf();
        fs::create_dir_all(&repo_dir).map_err(|_| Error::StorageWriteFailed)?;
        let comments_root = load_document(&repo_dir.join("comments.json"), "comments")?;
        let states_root = load_document(&repo_dir.join("review-states.json"), "states")?;
        let comments = entries(&comments_root, "comments")
            .iter()
            .map(Comment::from_json)
            .collect();
        let states = entries(&states_root, "states")
            .iter()
            .map(ReviewState::from_json)
            .collect();
        Ok(Self {
            comments,
            states,
            repo_dir,
            comments_root,
            states_root,
        })
    }
    pub fn save(&self) -> Result<()> {
        self.save_comments(&self.comments)?;
        self.save_states(&self.states)
    }
    fn save_comments(&self, comments: &[Comment]) -> Result<()> {
        let mut root = self.comments_root.clone();
        root["schema_version"] = json!(1);
        root["comments"] = Value::Array(comments.iter().map(Comment::to_json).collect());
        atomic_write(&self.repo_dir.join("comments.json"), &root)
    }
    fn save_states(&self, states: &[ReviewState]) -> Result<()> {
        let mut root = self.states_root.clone();
        root["schema_version"] = json!(1);
        root["states"] = Value::Array(states.iter().map(ReviewState::to_json).collect());
        atomic_write(&self.repo_dir.join("review-states.json"), &root)
    }
    pub fn comment_count(&self, file_path: &str, target_id: &str) -> usize {
        self.comments
            .iter()
            .filter(|c| c.file_path == file_path && c.review_target_id == target_id)
            .count()
    }
    pub fn is_reviewed(&self, file_path: &str, patch_fingerprint: &str, target_id: &str) -> bool {
        self.states.iter().any(|s| {
            s.file_path == file_path
                && s.review_target_id == target_id
                && s.status == "reviewed"
                && s.patch_fingerprint == patch_fingerprint
        })
    }
    pub fn status_for_file(
        &self,
        file_path: &str,
        patch_fingerprint: &str,
        target_id: &str,
    ) -> &'static str {
        if self.is_reviewed(file_path, patch_fingerprint, target_id) {
            "reviewed"
        } else {
            "unreviewed"
        }
    }
    pub fn set_reviewed(
        &mut self,
        repository_id: &str,
        target_id: &str,
        file: &DiffFile,
        reviewed: bool,
    ) -> Result<()> {
        let mut states = self.states.clone();
        let existing = states
            .iter_mut()
            .find(|s| s.file_path == file.path && s.review_target_id == target_id);
        let status = if reviewed { "reviewed" } else { "unreviewed" };
        if let Some(state) = existing {
            state.status = status.into();
            state.patch_fingerprint = file.patch_fingerprint.clone();
            state.updated_at = util::now_iso();
        } else {
            states.push(ReviewState {
                repository_id: repository_id.into(),
                review_target_id: target_id.into(),
                file_path: file.path.clone(),
                status: status.into(),
                patch_fingerprint: file.patch_fingerprint.clone(),
                updated_at: util::now_iso(),
                original: json!({}),
            });
        }
        self.save_states(&states)?;
        self.states = states;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn add_comment(
        &mut self,
        repository_id: &str,
        target_id: &str,
        file: &DiffFile,
        line: &DiffLine,
        hunk_header: &str,
        end_line: u32,
        body: &str,
        author: &str,
    ) -> Result<Comment> {
        let now = util::now_iso();
        let deleted = line.kind == DiffLineKind::Delete;
        let start_line = if deleted {
            line.old_lineno.unwrap_or(0)
        } else {
            line.new_lineno.or(line.old_lineno).unwrap_or(0)
        };
        let hash = util::hash_hex(
            format!(
                "{repository_id}:{target_id}:{}:{start_line}:{end_line}:{now}:{body}",
                file.path
            )
            .as_bytes(),
        );
        let comment = Comment {
            comment_id: format!("cmt_{}", &hash[..16]),
            repository_id: repository_id.into(),
            review_target_id: target_id.into(),
            file_path: file.path.clone(),
            side: if deleted { "old" } else { "new" }.into(),
            start_line,
            end_line: if end_line == 0 { start_line } else { end_line },
            stable_line_id: line.stable_line_id.clone(),
            hunk_header: hunk_header.into(),
            context_before: vec![],
            context_after: vec![],
            patch_fingerprint: file.patch_fingerprint.clone(),
            match_status: MatchStatus::Exact,
            body: body.into(),
            author: author.into(),
            created_at: now.clone(),
            updated_at: now,
            original: json!({}),
        };
        let mut comments = self.comments.clone();
        comments.push(comment.clone());
        self.save_comments(&comments)?;
        self.comments = comments;
        Ok(comment)
    }
    pub fn refresh_match_status(&mut self, snapshot: &DiffSnapshot) {
        for c in &mut self.comments {
            if c.review_target_id != snapshot.review_target.target_id {
                continue;
            }
            c.match_status = match snapshot.files.iter().find(|f| f.path == c.file_path) {
                Some(file) if file.patch_fingerprint == c.patch_fingerprint => MatchStatus::Exact,
                Some(_) => MatchStatus::Stale,
                None => MatchStatus::Missing,
            };
        }
    }
    pub fn outdated_comment_count(&self, target_id: &str, file_filter: Option<&str>) -> usize {
        self.comments
            .iter()
            .filter(|c| {
                c.review_target_id == target_id
                    && matches_file(c, file_filter)
                    && c.match_status.outdated()
            })
            .count()
    }
    pub fn all_comment_count(&self, file_filter: Option<&str>) -> usize {
        self.comments
            .iter()
            .filter(|c| matches_file(c, file_filter))
            .count()
    }
    pub fn remove_outdated_comments(
        &mut self,
        target_id: &str,
        file_filter: Option<&str>,
    ) -> Result<usize> {
        self.remove_comments(|c| {
            c.review_target_id == target_id
                && matches_file(c, file_filter)
                && c.match_status.outdated()
        })
    }
    pub fn remove_all_comments(&mut self, file_filter: Option<&str>) -> Result<usize> {
        self.remove_comments(|c| matches_file(c, file_filter))
    }
    fn remove_comments(&mut self, remove: impl Fn(&Comment) -> bool) -> Result<usize> {
        let kept: Vec<_> = self
            .comments
            .iter()
            .filter(|c| !remove(c))
            .cloned()
            .collect();
        let removed = self.comments.len() - kept.len();
        if removed != 0 {
            self.save_comments(&kept)?;
            self.comments = kept;
        }
        Ok(removed)
    }
}

fn matches_file(c: &Comment, filter: Option<&str>) -> bool {
    filter.is_none_or(|f| c.file_path == f)
}
fn string(v: &Value, key: &str, default: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or(default).into()
}
fn number(v: &Value, key: &str) -> u32 {
    let Some(n) = v.get(key).and_then(Value::as_f64) else {
        return 0;
    };
    if n >= 0.0 && n <= u32::MAX as f64 {
        n as u32
    } else {
        0
    }
}
fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
impl Comment {
    fn from_json(v: &Value) -> Self {
        let a = v.get("anchor").unwrap_or(v);
        Self {
            comment_id: string(v, "comment_id", ""),
            repository_id: string(v, "repository_id", ""),
            review_target_id: string(v, "review_target_id", ""),
            file_path: string(v, "file_path", ""),
            side: string(a, "side", "new"),
            start_line: number(a, "start_line"),
            end_line: number(a, "end_line"),
            stable_line_id: v
                .get("anchor")
                .and_then(|a| a.get("stable_line_ids"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            hunk_header: string(a, "hunk_header", ""),
            context_before: strings(a, "context_before"),
            context_after: strings(a, "context_after"),
            patch_fingerprint: string(a, "patch_fingerprint", ""),
            match_status: MatchStatus::parse(&string(&v["anchor"], "match_status", "exact")),
            body: string(v, "body", ""),
            author: string(v, "author", ""),
            created_at: string(v, "created_at", ""),
            updated_at: string(v, "updated_at", ""),
            original: v.clone(),
        }
    }
    fn to_json(&self) -> Value {
        let mut v = self.original.clone();
        for (key, value) in [
            ("comment_id", &self.comment_id),
            ("repository_id", &self.repository_id),
            ("review_target_id", &self.review_target_id),
            ("file_path", &self.file_path),
            ("body", &self.body),
            ("author", &self.author),
            ("created_at", &self.created_at),
            ("updated_at", &self.updated_at),
        ] {
            v[key] = json!(value);
        }
        if !v["anchor"].is_object() {
            v["anchor"] = json!({});
        }
        let a = &mut v["anchor"];
        for (key, value) in [
            ("side", self.side.as_str()),
            ("hunk_header", &self.hunk_header),
            ("patch_fingerprint", &self.patch_fingerprint),
            ("match_status", self.match_status.label()),
        ] {
            a[key] = json!(value);
        }
        a["start_line"] = json!(self.start_line);
        a["end_line"] = json!(self.end_line);
        let mut ids = a["stable_line_ids"].as_array().cloned().unwrap_or_default();
        if ids.is_empty() {
            ids.push(json!(self.stable_line_id));
        } else {
            ids[0] = json!(self.stable_line_id);
        }
        a["stable_line_ids"] = json!(ids);
        a["context_before"] = json!(self.context_before);
        a["context_after"] = json!(self.context_after);
        v
    }
}
impl ReviewState {
    fn from_json(v: &Value) -> Self {
        Self {
            repository_id: string(v, "repository_id", ""),
            review_target_id: string(v, "review_target_id", ""),
            file_path: string(v, "file_path", ""),
            status: string(v, "status", "unreviewed"),
            patch_fingerprint: string(v, "patch_fingerprint", ""),
            updated_at: string(v, "updated_at", ""),
            original: v.clone(),
        }
    }
    fn to_json(&self) -> Value {
        let mut v = self.original.clone();
        for (key, value) in [
            ("repository_id", &self.repository_id),
            ("review_target_id", &self.review_target_id),
            ("file_path", &self.file_path),
            ("status", &self.status),
            ("patch_fingerprint", &self.patch_fingerprint),
            ("updated_at", &self.updated_at),
        ] {
            v[key] = json!(value);
        }
        v
    }
}
fn entries<'a>(root: &'a Value, key: &str) -> &'a [Value] {
    root.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn load_document(path: &Path, key: &str) -> Result<Value> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"schema_version": 1}));
        }
        Err(_) => return Err(Error::StorageCorrupted),
    };
    const LIMIT: u64 = 100 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::StorageCorrupted)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::StorageCorrupted);
    }
    let root: Value = serde_json::from_slice(&bytes).map_err(|_| Error::StorageCorrupted)?;
    // Refuse unsupported or malformed documents rather than silently deleting data.
    if !root.is_object()
        || root
            .get("schema_version")
            .is_some_and(|v| v.as_u64() != Some(1))
        || root.get(key).is_some_and(|v| !v.is_array())
        || entries(&root, key).iter().any(|v| !v.is_object())
    {
        return Err(Error::StorageCorrupted);
    }
    Ok(root)
}
fn atomic_write(path: &Path, value: &Value) -> Result<()> {
    let write = || -> std::result::Result<(), Box<dyn std::error::Error>> {
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("missing parent directory"))?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temp, value)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(path)?;
        Ok(())
    };
    write().map_err(|_| Error::StorageWriteFailed)
}

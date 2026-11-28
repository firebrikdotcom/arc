//! Replay guard for `arc console` write helpers.
//!
//! irust compiles and runs every statement entered so far on each evaluation,
//! so a write statement would run again on every later line. The guard keeps a
//! per-session journal of the writes the session has performed:
//!
//! * A write is identified by the source text of the statement that makes it
//!   (read from irust's generated host `main.rs` at the caller's location),
//!   plus how many times that same text has already written in the current
//!   evaluation (so a loop or two identical statements are distinct writes).
//! * The first run of a write records it; every later run of the same
//!   statement returns the recorded result without writing.
//! * A write is recorded as pending before it runs and completed after, so an
//!   evaluation that dies in between is never retried silently.
//! * A write made by the evaluated expression itself (input without a
//!   trailing `;`, which irust does not keep) always runs and is not recorded.
//!
//! The session is one `arc console` launch: it sets `ARC_CONSOLE_SESSION` (a
//! fresh id) and `ARC_CONSOLE_DIR`, and deletes the journal when irust exits.
//!
//! This file is compiled into the generated console crate and, for its unit
//! tests, into `arc-web-cli`. It must only use `std` and `serde_json`.
use ::serde_json as json;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Where a write helper was called from, as `std::panic::Location` reports it.
#[derive(Clone, Debug)]
pub struct Site {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

impl Site {
    pub fn of(location: &std::panic::Location<'_>) -> Self {
        Self {
            file: location.file().to_string(),
            line: location.line(),
            column: location.column(),
        }
    }
}

/// The journal of one console session.
pub struct Journal {
    /// The console workspace (`target/arc-console`); host-crate paths in
    /// [`Site::file`] are relative to it.
    pub console_dir: PathBuf,
    pub session: String,
    /// Writes per statement already made by this evaluation (process).
    counts: Mutex<HashMap<String, usize>>,
}

/// How a guarded write was resolved.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The write ran now and was recorded.
    Written,
    /// The write ran now as the evaluated expression; it is not replayed.
    WrittenOnce,
    /// The statement already wrote earlier in the session; nothing ran.
    Replayed,
}

impl Journal {
    pub fn new(console_dir: PathBuf, session: String) -> Self {
        Self {
            console_dir,
            session,
            counts: Mutex::new(HashMap::new()),
        }
    }

    /// The journal of the running `arc console` session.
    pub fn current() -> Result<&'static Journal, String> {
        static CURRENT: OnceLock<Result<Journal, String>> = OnceLock::new();
        CURRENT
            .get_or_init(|| {
                let session = std::env::var("ARC_CONSOLE_SESSION").ok().filter(|s| {
                    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                });
                let dir = std::env::var_os("ARC_CONSOLE_DIR").map(PathBuf::from);
                match (session, dir) {
                    (Some(session), Some(dir)) => Ok(Journal::new(dir, session)),
                    _ => Err("console write helpers only run inside a session started by `arc console` (ARC_CONSOLE_SESSION is not set)".to_string()),
                }
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub fn path(&self) -> PathBuf {
        self.console_dir
            .join("sessions")
            .join(format!("{}.jsonl", self.session))
    }

    /// Runs `write` unless the statement at `site` already wrote in this
    /// session, in which case its first result is returned instead.
    pub fn guard(
        &self,
        site: &Site,
        label: &str,
        write: impl FnOnce() -> Result<json::Value, String>,
    ) -> (Outcome, Result<json::Value, String>) {
        let source = read_source(&self.console_dir, &site.file);
        let statement = source
            .as_deref()
            .and_then(|source| statement_at(source, site.line))
            .unwrap_or_else(|| format!("{}:{}:{}", site.file, site.line, site.column));
        if source
            .as_deref()
            .is_some_and(|source| in_evaluated_expression(source, site.line))
        {
            return (Outcome::WrittenOnce, write());
        }
        let key = format!("{label}\u{1f}{statement}");
        let occurrence = {
            let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
            let count = counts.entry(key.clone()).or_insert(0);
            *count += 1;
            *count - 1
        };
        let entries = match self.read() {
            Ok(entries) => entries,
            Err(error) => return (Outcome::Replayed, Err(error)),
        };
        let recorded = entries.iter().rev().find(|entry| {
            entry["key"].as_str() == Some(key.as_str())
                && entry["occurrence"].as_u64() == Some(occurrence as u64)
        });
        if let Some(entry) = recorded {
            if entry["state"] == "done" {
                if entry["line"].as_u64() != Some(u64::from(site.line)) {
                    eprintln!(
                        "arc console: `{statement}` already wrote earlier in this session; returning its first result without writing again"
                    );
                }
                let result = match entry.get("ok") {
                    Some(value) => Ok(value.clone()),
                    None => Err(entry["err"]
                        .as_str()
                        .unwrap_or("recorded write failed")
                        .to_string()),
                };
                return (Outcome::Replayed, result);
            }
            return (
                Outcome::Replayed,
                Err(format!(
                    "`{statement}` started writing in an earlier evaluation but its result was not recorded; it is not retried. Inspect the event store, then restart `arc console` to write again"
                )),
            );
        }
        let mut entry = json::json!({
            "key": key,
            "occurrence": occurrence,
            "line": site.line,
            "state": "pending",
        });
        if let Err(error) = self.append(&entry) {
            return (Outcome::Replayed, Err(error));
        }
        let result = write();
        entry["state"] = "done".into();
        match &result {
            Ok(value) => entry["ok"] = value.clone(),
            Err(message) => entry["err"] = message.clone().into(),
        }
        if let Err(error) = self.append(&entry) {
            eprintln!("arc console: {error}");
        }
        (Outcome::Written, result)
    }

    fn read(&self) -> Result<Vec<json::Value>, String> {
        let path = self.path();
        match fs::read_to_string(&path) {
            Ok(contents) => Ok(contents
                .lines()
                .filter_map(|line| json::from_str(line).ok())
                .collect()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(format!(
                "could not read the console write journal {}: {error}",
                path.display()
            )),
        }
    }

    fn append(&self, entry: &json::Value) -> Result<(), String> {
        let path = self.path();
        let failed = |error: std::io::Error| {
            format!(
                "could not record the write in {}: {error}; nothing was written",
                path.display()
            )
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(failed)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(failed)?;
        writeln!(file, "{entry}").map_err(failed)?;
        file.sync_data().map_err(failed)
    }
}

fn read_source(console_dir: &Path, file: &str) -> Option<String> {
    let path = Path::new(file);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        console_dir.join(path)
    };
    fs::read_to_string(path).ok()
}

/// The statement starting on 1-based `line`: that line and the following
/// ones up to the first that ends with `;`, trimmed and joined.
pub fn statement_at(source: &str, line: u32) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let start = (line as usize).checked_sub(1)?;
    lines.get(start)?;
    let mut statement = Vec::new();
    for text in lines.iter().skip(start).take(64) {
        let text = text.trim();
        statement.push(text);
        if text.ends_with(';') {
            break;
        }
    }
    Some(statement.join(" "))
}

/// irust wraps the evaluated input as `println!("{:?}", {\n<input>\n});`
/// followed by `std::process::exit(0);` and inserts it after the kept
/// statements. Reports whether 1-based `line` lies inside that wrapper.
pub fn in_evaluated_expression(source: &str, line: u32) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    let Some(end) = lines
        .iter()
        .rposition(|text| text.contains("std::process::exit(0);"))
    else {
        return false;
    };
    let Some(start) = lines[..=end]
        .iter()
        .rposition(|text| text.trim() == "println!(\"{:?}\", {")
    else {
        return false;
    };
    let index = (line as usize).wrapping_sub(1);
    start < index && index <= end
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("arc-console-guard-{}-{nonce}", std::process::id()));
            fs::create_dir_all(path.join("irust_repls/host/src")).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const HOST: &str = "irust_repls/host/src/main.rs";

    /// A host `main.rs` the way irust writes it: kept statements, then the
    /// evaluated input wrapped in its default evaluator.
    fn host(dir: &Path, statements: &[&str], input: &str) {
        let mut lines = vec!["fn main() -> () {", "use irust_prelude::*;"];
        lines.extend(statements);
        lines.extend(["println!(\"{:?}\", {", input, "});std::process::exit(0);"]);
        lines.extend([
            "#[allow(unreachable_code)]() // Do not write past this line",
            "}",
        ]);
        fs::write(dir.join(HOST), lines.join("\n")).unwrap();
    }

    fn site(line: u32) -> Site {
        Site {
            file: HOST.into(),
            line,
            column: 9,
        }
    }

    /// One evaluation: a fresh process has fresh per-statement counts.
    fn evaluation(dir: &Path) -> Journal {
        Journal::new(dir.to_path_buf(), "s1".into())
    }

    #[test]
    fn a_kept_statement_writes_once_across_evaluations() {
        let dir = TempDir::new();
        let writes = Cell::new(0);
        let write = || {
            writes.set(writes.get() + 1);
            Ok(json::json!(writes.get()))
        };
        let dispatch = "let r = dispatch::<P>(Create { id: new_id() });";
        // Evaluation 1 runs the kept statement for the first time.
        host(&dir.0, &[dispatch], "1 + 1");
        let (outcome, result) = evaluation(&dir.0).guard(&site(3), "Post", write);
        assert_eq!((outcome, result), (Outcome::Written, Ok(json::json!(1))));
        // Evaluations 2 and 3 replay it, even though its id is regenerated.
        for input in ["2 + 2", "rows(\"x\")"] {
            host(&dir.0, &[dispatch, "let x = 1;"], input);
            let (outcome, result) = evaluation(&dir.0).guard(&site(3), "Post", write);
            assert_eq!((outcome, result), (Outcome::Replayed, Ok(json::json!(1))));
        }
        assert_eq!(writes.get(), 1);
        // A different statement, or an identical second one, writes.
        let journal = evaluation(&dir.0);
        host(&dir.0, &[dispatch, dispatch], "0");
        assert_eq!(journal.guard(&site(3), "Post", write).0, Outcome::Replayed);
        assert_eq!(journal.guard(&site(4), "Post", write).0, Outcome::Written);
        assert_eq!(writes.get(), 2);
        // The same text for another aggregate is another write.
        assert_eq!(
            evaluation(&dir.0).guard(&site(3), "Order", write).0,
            Outcome::Written
        );
        assert_eq!(writes.get(), 3);
    }

    #[test]
    fn loops_failures_and_unfinished_writes() {
        let dir = TempDir::new();
        let writes = Cell::new(0);
        let write = || {
            writes.set(writes.get() + 1);
            Err::<json::Value, _>("rejected".to_string())
        };
        let looped = "for i in 0..3 { let _ = dispatch::<P>(cmd(i)); }";
        host(&dir.0, &[looped], "0");
        for _ in 0..2 {
            let journal = evaluation(&dir.0);
            for _ in 0..3 {
                // A failure is recorded and replayed too, so a later state
                // change cannot make a rejected statement succeed silently.
                assert_eq!(
                    journal.guard(&site(3), "P", write).1,
                    Err("rejected".into())
                );
            }
        }
        assert_eq!(writes.get(), 3);
        // A write that started but never recorded its result is not retried.
        let journal = evaluation(&dir.0);
        host(&dir.0, &["let a = dispatch::<P>(x);"], "0");
        let key = "P\u{1f}let a = dispatch::<P>(x);";
        journal
            .append(&json::json!({"key": key, "occurrence": 0, "line": 3, "state": "pending"}))
            .unwrap();
        let (outcome, result) = journal.guard(&site(3), "P", || panic!("must not run"));
        assert_eq!(outcome, Outcome::Replayed);
        assert!(result.unwrap_err().contains("not retried"));
        // Another session starts with an empty journal.
        let other = Journal::new(dir.0.clone(), "s2".into());
        assert_eq!(
            other.guard(&site(3), "P", || Ok(json::json!(0))).0,
            Outcome::Written
        );
    }

    #[test]
    fn the_evaluated_expression_always_writes_and_is_not_recorded() {
        let dir = TempDir::new();
        let writes = Cell::new(0);
        let write = || {
            writes.set(writes.get() + 1);
            Ok(json::json!(null))
        };
        host(&dir.0, &[], "dispatch::<P>(x)");
        for _ in 0..2 {
            assert_eq!(
                evaluation(&dir.0).guard(&site(4), "P", write).0,
                Outcome::WrittenOnce
            );
        }
        assert_eq!(writes.get(), 2);
        assert!(!evaluation(&dir.0).path().exists());
    }

    #[test]
    fn statements_span_lines_until_a_semicolon() {
        let source = "a\nlet r = dispatch::<P>(\n  Create { id: 1 },\n);\nb;";
        assert_eq!(
            statement_at(source, 2).as_deref(),
            Some("let r = dispatch::<P>( Create { id: 1 }, );")
        );
        assert_eq!(statement_at(source, 9), None);
        assert!(!in_evaluated_expression(source, 2));
    }
}

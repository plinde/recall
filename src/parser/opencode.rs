use crate::session::{Message, Role, Session, SessionSource};
use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use super::{join_consecutive_messages, sqlite_identity, SessionLocator, SessionParser};

/// OpenCode session metadata from session/<project_id>/ses_*.json
#[derive(Debug, Deserialize)]
struct OpenCodeSession {
    id: String,
    #[serde(rename = "projectID")]
    #[allow(dead_code)]
    project_id: Option<String>,
    directory: Option<String>,
    #[allow(dead_code)]
    title: Option<String>,
    time: Option<TimeInfo>,
    #[serde(rename = "parentID", alias = "parent_id")]
    parent_id: Option<String>,
}

/// OpenCode message metadata from message/ses_*/msg_*.json
#[derive(Debug, Deserialize)]
struct OpenCodeMessage {
    id: String,
    #[serde(rename = "sessionID")]
    #[allow(dead_code)]
    session_id: String,
    role: String,
    time: Option<TimeInfo>,
    #[serde(rename = "parentID")]
    #[allow(dead_code)]
    parent_id: Option<String>,
    path: Option<PathInfo>,
}

/// Time information with millisecond timestamps
#[derive(Debug, Deserialize)]
struct TimeInfo {
    created: i64,
    #[allow(dead_code)]
    updated: Option<i64>,
}

/// Path information from assistant messages
#[derive(Debug, Deserialize)]
struct PathInfo {
    cwd: Option<String>,
    #[allow(dead_code)]
    root: Option<String>,
}

/// OpenCode part (content) from part/msg_*/prt_*.json
#[derive(Debug, Deserialize)]
struct OpenCodePart {
    #[allow(dead_code)]
    id: String,
    #[serde(rename = "type")]
    part_type: String,
    text: Option<String>,
}

pub struct OpenCodeParser;

impl SessionParser for OpenCodeParser {
    fn can_parse(path: &Path) -> bool {
        path.components()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|parts| parts[0].as_os_str() == "storage" && parts[1].as_os_str() == "session")
            && path.extension().is_some_and(|extension| extension == "json")
    }

    fn parse_file(path: &Path) -> Result<Session> {
        // 1. Read session JSON
        let file = File::open(path).context("Failed to open session file")?;
        let reader = BufReader::new(file);
        let session: OpenCodeSession =
            serde_json::from_reader(reader).context("Failed to parse session JSON")?;
        if session.parent_id.is_some() {
            anyhow::bail!("OpenCode child sessions are not indexed");
        }

        // 2. Get storage root (go up from session/<project>/ses_*.json to storage/)
        let storage_root = get_storage_root(path).context("Failed to get storage root")?;

        // 3. Find and read all messages for this session
        let message_dir = storage_root.join("message").join(&session.id);
        let mut messages: Vec<Message> = Vec::new();
        let mut latest_timestamp: Option<DateTime<Utc>> = None;
        let mut cwd: Option<String> = session.directory.clone();

        if message_dir.exists() {
            // Collect and sort message files by creation time
            let mut msg_entries: Vec<(PathBuf, OpenCodeMessage)> = Vec::new();

            if let Ok(entries) = std::fs::read_dir(&message_dir) {
                for entry in entries.flatten() {
                    let msg_path = entry.path();
                    if msg_path.extension().map(|e| e == "json").unwrap_or(false) {
                        if let Ok(file) = File::open(&msg_path) {
                            let reader = BufReader::new(file);
                            if let Ok(msg) = serde_json::from_reader::<_, OpenCodeMessage>(reader) {
                                msg_entries.push((msg_path, msg));
                            }
                        }
                    }
                }
            }

            // Sort by creation time
            msg_entries.sort_by(|a, b| {
                let time_a = a.1.time.as_ref().map(|t| t.created).unwrap_or(0);
                let time_b = b.1.time.as_ref().map(|t| t.created).unwrap_or(0);
                time_a.cmp(&time_b)
            });

            // Process each message
            for (_msg_path, msg) in msg_entries {
                // Get timestamp
                let timestamp = msg
                    .time
                    .as_ref()
                    .map(|t| millis_to_datetime(t.created))
                    .unwrap_or_else(Utc::now);

                // Update latest timestamp
                if latest_timestamp.is_none() || timestamp > latest_timestamp.unwrap() {
                    latest_timestamp = Some(timestamp);
                }

                // Get cwd from message path info if available
                if cwd.is_none() {
                    if let Some(path_info) = &msg.path {
                        cwd = path_info.cwd.clone();
                    }
                }

                // Determine role
                let role = match msg.role.as_str() {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    _ => continue, // Skip unknown roles
                };

                // Read parts for this message
                let content = read_message_parts(&storage_root, &msg.id);
                if !content.is_empty() {
                    messages.push(Message {
                        role,
                        content,
                        timestamp,
                    });
                }
            }
        }

        Ok(Session {
            id: session.id,
            source: SessionSource::OpenCode,
            file_path: path.to_path_buf(),
            cwd: cwd.unwrap_or_else(|| ".".to_string()),
            git_branch: None, // OpenCode doesn't store git branch in session metadata
            timestamp: latest_timestamp.unwrap_or_else(|| {
                session
                    .time
                    .as_ref()
                    .map(|t| millis_to_datetime(t.created))
                    .unwrap_or_else(Utc::now)
            }),
            messages: join_consecutive_messages(messages),
        })
    }
}

impl OpenCodeParser {
    pub fn discover(opencode_root: &Path) -> Vec<SessionLocator> {
        let mut locators = Vec::new();
        let database = opencode_root.join("opencode.db");
        if database.exists() {
            if let Ok(connection) = open_read_only(&database) {
                if let Ok(mut statement) = connection.prepare(
                    "SELECT id, time_updated FROM session WHERE parent_id IS NULL",
                ) {
                    if let Ok(rows) = statement.query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    }) {
                        for row in rows.flatten() {
                            locators.push(SessionLocator {
                                identity: sqlite_identity(&database, &row.0),
                                path: database.clone(),
                                database_session_id: Some(row.0),
                                fingerprint: row.1.to_string(),
                                sort_timestamp: row.1,
                            });
                        }
                    }
                }
            }
        }

        let legacy = opencode_root.join("storage/session");
        if legacy.exists() {
            for entry in walkdir::WalkDir::new(legacy).into_iter().flatten() {
                let path = entry.path();
                let is_session = path.extension().is_some_and(|ext| ext == "json")
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("ses_"));
                if !is_session || legacy_is_child(path) {
                    continue;
                }
                if let Some(locator) = SessionLocator::file(path.to_path_buf()) {
                    locators.push(locator);
                }
            }
        }
        locators
    }

    pub fn parse_locator(locator: &SessionLocator) -> Result<Session> {
        let session_id = locator
            .database_session_id
            .as_deref()
            .context("SQLite OpenCode locator has no session ID")?;
        parse_database_session(&locator.path, session_id)
    }
}

fn open_read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Failed to open OpenCode database {}", path.display()))
}

fn parse_database_session(database: &Path, session_id: &str) -> Result<Session> {
    let connection = open_read_only(database)?;
    let (directory, created, updated): (String, i64, i64) = connection
        .query_row(
            "SELECT directory, time_created, time_updated FROM session \
             WHERE id = ?1 AND parent_id IS NULL",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .with_context(|| {
            format!(
                "OpenCode session {session_id} is no longer in {}",
                database.display()
            )
        })?;

    let mut message_statement = connection.prepare(
        "SELECT id, time_created, data FROM message \
         WHERE session_id = ?1 ORDER BY time_created, id",
    )?;
    let rows = message_statement.query_map([session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut messages = Vec::new();
    let mut cwd = directory;
    for row in rows {
        let (message_id, timestamp, data) = row?;
        let data: serde_json::Value = serde_json::from_str(&data)
            .with_context(|| format!("Invalid OpenCode message JSON for {message_id}"))?;
        let role = match data.get("role").and_then(|value| value.as_str()) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => continue,
        };
        if cwd.is_empty() {
            if let Some(value) = data
                .get("path")
                .and_then(|path| path.get("cwd"))
                .and_then(|value| value.as_str())
            {
                cwd = value.to_string();
            }
        }

        let mut part_statement = connection.prepare(
            "SELECT data FROM part WHERE message_id = ?1 ORDER BY id",
        )?;
        let parts = part_statement.query_map([&message_id], |row| row.get::<_, String>(0))?;
        let mut texts = Vec::new();
        for part in parts {
            let part: serde_json::Value = serde_json::from_str(&part?)
                .with_context(|| format!("Invalid OpenCode part JSON for {message_id}"))?;
            if part.get("type").and_then(|value| value.as_str()) == Some("text") {
                if let Some(text) = part.get("text").and_then(|value| value.as_str()) {
                    if !text.is_empty() {
                        texts.push(text.to_string());
                    }
                }
            }
        }
        if !texts.is_empty() {
            messages.push(Message {
                role,
                content: texts.join("\n"),
                timestamp: millis_to_datetime(timestamp),
            });
        }
    }

    Ok(Session {
        id: session_id.to_string(),
        source: SessionSource::OpenCode,
        file_path: database.to_path_buf(),
        cwd: if cwd.is_empty() { ".".to_string() } else { cwd },
        git_branch: None,
        timestamp: millis_to_datetime(updated.max(created)),
        messages: join_consecutive_messages(messages),
    })
}

fn legacy_is_child(path: &Path) -> bool {
    File::open(path)
        .ok()
        .and_then(|file| serde_json::from_reader::<_, OpenCodeSession>(BufReader::new(file)).ok())
        .is_some_and(|session| session.parent_id.is_some())
}

/// Get the storage root directory from a session file path
/// Path: storage/session/<project_id>/ses_*.json
/// Returns: storage/
fn get_storage_root(session_path: &Path) -> Option<PathBuf> {
    session_path
        .parent()? // ses_*.json -> <project_id>/
        .parent()? // <project_id> -> session/
        .parent() // session -> storage/
        .map(|p| p.to_path_buf())
}

/// Convert milliseconds timestamp to DateTime<Utc>
fn millis_to_datetime(millis: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(millis).single().unwrap_or_else(Utc::now)
}

/// Read all text parts for a message and concatenate them
fn read_message_parts(storage_root: &Path, message_id: &str) -> String {
    let parts_dir = storage_root.join("part").join(message_id);
    let mut texts: Vec<String> = Vec::new();

    if !parts_dir.exists() {
        return String::new();
    }

    // Read all part files
    let mut part_entries: Vec<(String, OpenCodePart)> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&parts_dir) {
        for entry in entries.flatten() {
            let part_path = entry.path();
            if part_path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(file) = File::open(&part_path) {
                    let reader = BufReader::new(file);
                    if let Ok(part) = serde_json::from_reader::<_, OpenCodePart>(reader) {
                        let filename = part_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_string();
                        part_entries.push((filename, part));
                    }
                }
            }
        }
    }

    // Sort by filename to maintain order (prt_* IDs are sortable)
    part_entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Extract text from text parts only
    for (_filename, part) in part_entries {
        if part.part_type == "text" {
            if let Some(text) = part.text {
                if !text.is_empty() {
                    texts.push(text);
                }
            }
        }
        // Skip step-start, step-finish, tool parts (per user preference)
    }

    texts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::TempDir;

    fn sqlite_fixture() -> (TempDir, PathBuf) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("opencode");
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("opencode.db");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (
                    id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL,
                    time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
                );
                CREATE TABLE message (
                    id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL, data TEXT NOT NULL
                );
                CREATE TABLE part (
                    id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL, data TEXT NOT NULL
                );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO session VALUES (?1, NULL, ?2, ?3, ?4)",
                params!["ses_main", "/work/project", 1_000_i64, 4_000_i64],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO session VALUES (?1, ?2, ?3, ?4, ?5)",
                params!["ses_child", "ses_main", "/work/project", 2_000_i64, 5_000_i64],
            )
            .unwrap();
        for (id, created, data) in [
            ("msg_b", 2_000_i64, r#"{"role":"assistant"}"#),
            ("msg_a", 1_000_i64, r#"{"role":"user"}"#),
            ("msg_c", 3_000_i64, r#"{"role":"assistant"}"#),
        ] {
            connection
                .execute(
                    "INSERT INTO message VALUES (?1, 'ses_main', ?2, ?3)",
                    params![id, created, data],
                )
                .unwrap();
        }
        for (id, message, data) in [
            ("prt_2", "msg_a", r#"{"type":"text","text":"second"}"#),
            ("prt_1", "msg_a", r#"{"type":"text","text":"first"}"#),
            ("prt_3", "msg_a", r#"{"type":"tool","text":"hidden"}"#),
            ("prt_4", "msg_b", r#"{"type":"text","text":"answer one"}"#),
            ("prt_5", "msg_c", r#"{"type":"text","text":"answer two"}"#),
        ] {
            connection
                .execute(
                    "INSERT INTO part VALUES (?1, ?2, 'ses_main', ?3)",
                    params![id, message, data],
                )
                .unwrap();
        }
        drop(connection);
        (temp, root)
    }

    #[test]
    fn test_can_parse_opencode_path() {
        assert!(OpenCodeParser::can_parse(Path::new(
            "/home/user/.local/share/opencode/storage/session/project123/ses_abc.json"
        )));
        assert!(!OpenCodeParser::can_parse(Path::new(
            "/home/user/.claude/projects/foo/session.jsonl"
        )));
        assert!(!OpenCodeParser::can_parse(Path::new(
            "/home/user/.codex/sessions/session.jsonl"
        )));
    }

    #[test]
    fn test_millis_to_datetime() {
        let dt = millis_to_datetime(1763499168814);
        assert!(dt.timestamp_millis() == 1763499168814);
    }

    #[test]
    fn test_get_storage_root() {
        let path = Path::new("/home/user/.local/share/opencode/storage/session/proj/ses_123.json");
        let root = get_storage_root(path);
        assert_eq!(
            root,
            Some(PathBuf::from(
                "/home/user/.local/share/opencode/storage"
            ))
        );
    }

    #[test]
    fn test_discovers_and_parses_sqlite_sessions() {
        let (_temp, root) = sqlite_fixture();
        let locators = OpenCodeParser::discover(&root);
        assert_eq!(locators.len(), 1, "child sessions must be excluded");
        assert_eq!(locators[0].database_session_id.as_deref(), Some("ses_main"));
        assert_eq!(locators[0].fingerprint, "4000");

        let session = OpenCodeParser::parse_locator(&locators[0]).unwrap();
        assert_eq!(session.id, "ses_main");
        assert_eq!(session.cwd, "/work/project");
        assert_eq!(session.timestamp.timestamp_millis(), 4_000);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, Role::User);
        assert_eq!(session.messages[0].content, "first\nsecond");
        assert_eq!(session.messages[0].timestamp.timestamp_millis(), 1_000);
        assert_eq!(session.messages[1].role, Role::Assistant);
        assert_eq!(session.messages[1].content, "answer one\n\nanswer two");
        assert_eq!(session.messages[1].timestamp.timestamp_millis(), 3_000);
    }

    #[test]
    fn test_discovers_only_top_level_legacy_sessions() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("opencode");
        let sessions = root.join("storage/session/project");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("ses_top.json"),
            r#"{"id":"ses_top","directory":"/tmp","time":{"created":1000}}"#,
        )
        .unwrap();
        std::fs::write(
            sessions.join("ses_child.json"),
            r#"{"id":"ses_child","parentID":"ses_top","directory":"/tmp","time":{"created":2000}}"#,
        )
        .unwrap();

        let locators = OpenCodeParser::discover(&root);
        assert_eq!(locators.len(), 1);
        assert!(locators[0].path.ends_with("ses_top.json"));
    }
}

#[cfg(test)]
mod real_data_tests {
    use super::*;
    
    #[test]
    #[ignore] // Run with: cargo test test_parse_real_opencode -- --ignored --nocapture
    fn test_parse_real_opencode() {
        let home = std::env::var("HOME").unwrap();
        let session_path = format!("{}/.local/share/opencode/storage/session/global/ses_5675050f7ffeivkIg0jm0b0D30.json", home);
        let path = std::path::Path::new(&session_path);
        
        println!("Testing path: {}", session_path);
        println!("Path exists: {}", path.exists());
        
        if path.exists() {
            match OpenCodeParser::parse_file(path) {
                Ok(session) => {
                    println!("Parsed session: {}", session.id);
                    println!("  Source: {:?}", session.source);
                    println!("  CWD: {}", session.cwd);
                    println!("  Messages: {}", session.messages.len());
                    for (i, msg) in session.messages.iter().enumerate() {
                        println!("  Message {}: {:?} - {} chars", i, msg.role, msg.content.len());
                        if !msg.content.is_empty() {
                            let preview: String = msg.content.chars().take(100).collect();
                            println!("    Preview: {}...", preview);
                        }
                    }
                    assert!(!session.messages.is_empty(), "Should have messages");
                }
                Err(e) => panic!("Error parsing: {}", e),
            }
        }
    }
}

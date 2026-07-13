mod claude;
mod codex;
mod factory;
mod opencode;

pub use claude::ClaudeParser;
pub use codex::CodexParser;
pub use factory::FactoryParser;
pub use opencode::OpenCodeParser;

use crate::session::{Message, Session};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A source-neutral reference to one indexable session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLocator {
    /// Stable identity used by Tantivy and the index state.
    pub identity: String,
    /// Backing session file or database.
    pub path: PathBuf,
    /// Row ID for sessions stored inside a database.
    pub database_session_id: Option<String>,
    /// Per-session change fingerprint.
    pub fingerprint: String,
    /// Millisecond timestamp used to order indexing work.
    pub sort_timestamp: i64,
}

impl SessionLocator {
    fn file(path: PathBuf) -> Option<Self> {
        let metadata = std::fs::metadata(&path).ok()?;
        let modified = metadata.modified().ok()?;
        let nanos = modified
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some(Self {
            identity: path.to_string_lossy().into_owned(),
            path,
            database_session_id: None,
            fingerprint: format!("{nanos}:{}", metadata.len()),
            sort_timestamp: (nanos / 1_000_000) as i64,
        })
    }

    pub fn from_session(session: &Session) -> Self {
        if session.source == crate::session::SessionSource::OpenCode
            && session.file_path.extension().is_some_and(|ext| ext == "db")
        {
            let id = session.id.clone();
            Self {
                identity: sqlite_identity(&session.file_path, &id),
                path: session.file_path.clone(),
                database_session_id: Some(id),
                fingerprint: String::new(),
                sort_timestamp: session.timestamp.timestamp_millis(),
            }
        } else {
            Self::file(session.file_path.clone()).unwrap_or_else(|| Self {
                identity: session.file_path.to_string_lossy().into_owned(),
                path: session.file_path.clone(),
                database_session_id: None,
                fingerprint: String::new(),
                sort_timestamp: session.timestamp.timestamp_millis(),
            })
        }
    }

    pub fn from_index_identity(identity: &str) -> Self {
        if let Some(encoded) = identity.strip_prefix("opencode-sqlite:") {
            if let Ok((path, id)) = serde_json::from_str::<(PathBuf, String)>(encoded) {
                return Self {
                    identity: identity.to_string(),
                    path,
                    database_session_id: Some(id),
                    fingerprint: String::new(),
                    sort_timestamp: 0,
                };
            }
        }
        let path = PathBuf::from(identity);
        Self {
            identity: identity.to_string(),
            path,
            database_session_id: None,
            fingerprint: String::new(),
            sort_timestamp: 0,
        }
    }
}

fn sqlite_identity(path: &Path, session_id: &str) -> String {
    format!(
        "opencode-sqlite:{}",
        serde_json::to_string(&(path, session_id)).expect("paths and IDs serialize")
    )
}

/// Join consecutive messages from the same role into single messages.
/// Uses the latest timestamp when joining.
pub fn join_consecutive_messages(messages: Vec<Message>) -> Vec<Message> {
    messages.into_iter().fold(Vec::new(), |mut acc, msg| {
        if let Some(last) = acc.last_mut() {
            if last.role == msg.role {
                last.content.push_str("\n\n");
                last.content.push_str(&msg.content);
                last.timestamp = msg.timestamp; // use latest
                return acc;
            }
        }
        acc.push(msg);
        acc
    })
}

/// Trait for parsing session files
pub trait SessionParser {
    /// Parse a session file into a Session
    fn parse_file(path: &Path) -> Result<Session>;

    /// Check if this parser can handle the given file
    fn can_parse(path: &Path) -> bool;
}

/// Discover all sessions from supported sources.
pub fn discover_sessions() -> Vec<SessionLocator> {
    let mut files = Vec::new();

    // Allow override for testing
    let home = std::env::var("RECALL_HOME_OVERRIDE")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(dirs::home_dir);

    if let Some(home) = home {
        // Claude Code: ~/.claude/projects/*/*.jsonl
        let claude_dir = home.join(".claude/projects");
        if claude_dir.exists() {
            if let Ok(projects) = std::fs::read_dir(&claude_dir) {
                for project in projects.flatten() {
                    if let Ok(sessions) = std::fs::read_dir(project.path()) {
                        for session in sessions.flatten() {
                            let path = session.path();
                            if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                                // Skip agent sidechain files (internal subagent conversations)
                                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                    if name.starts_with("agent-") {
                                        continue;
                                    }
                                }
                                if let Some(locator) = SessionLocator::file(path) {
                                    files.push(locator);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Codex CLI: ~/.codex/sessions/**/*.jsonl
        let codex_dir = home.join(".codex/sessions");
        if codex_dir.exists() {
            for entry in walkdir::WalkDir::new(&codex_dir)
                .into_iter()
                .flatten()
            {
                let path = entry.path();
                if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                    if let Some(locator) = SessionLocator::file(path.to_path_buf()) {
                        files.push(locator);
                    }
                }
            }
        }

        // Factory: ~/.factory/sessions/**/*.jsonl
        let factory_dir = home.join(".factory/sessions");
        if factory_dir.exists() {
            for entry in walkdir::WalkDir::new(&factory_dir)
                .into_iter()
                .flatten()
            {
                let path = entry.path();
                if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                    if let Some(locator) = SessionLocator::file(path.to_path_buf()) {
                        files.push(locator);
                    }
                }
            }
        }

        let opencode_root = if std::env::var_os("RECALL_HOME_OVERRIDE").is_some() {
            home.join(".local/share/opencode")
        } else {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .map(|p| p.join("opencode"))
                .unwrap_or_else(|| home.join(".local/share/opencode"))
        };
        files.extend(OpenCodeParser::discover(&opencode_root));
    }

    files
}

/// Legacy path-only discovery API.
pub fn discover_session_files() -> Vec<PathBuf> {
    discover_sessions().into_iter().map(|locator| locator.path).collect()
}

/// Parse a session file, auto-detecting the format
pub fn parse_session_file(path: &Path) -> Result<Session> {
    if ClaudeParser::can_parse(path) {
        ClaudeParser::parse_file(path)
    } else if CodexParser::can_parse(path) {
        CodexParser::parse_file(path)
    } else if FactoryParser::can_parse(path) {
        FactoryParser::parse_file(path)
    } else if OpenCodeParser::can_parse(path) {
        OpenCodeParser::parse_file(path)
    } else {
        anyhow::bail!("Unknown session file format: {:?}", path)
    }
}

pub fn parse_session(locator: &SessionLocator) -> Result<Session> {
    if locator.database_session_id.is_some() {
        OpenCodeParser::parse_locator(locator)
    } else {
        parse_session_file(&locator.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Role;
    use chrono::Utc;

    #[test]
    fn test_join_consecutive_messages_different_roles() {
        let now = Utc::now();
        let messages = vec![
            Message { role: Role::User, content: "Hello".to_string(), timestamp: now },
            Message { role: Role::Assistant, content: "Hi".to_string(), timestamp: now },
            Message { role: Role::User, content: "Bye".to_string(), timestamp: now },
        ];
        let joined = join_consecutive_messages(messages);
        assert_eq!(joined.len(), 3);
    }

    #[test]
    fn test_join_consecutive_messages_same_role() {
        let t1 = Utc::now();
        let t2 = t1 + chrono::Duration::seconds(10);
        let messages = vec![
            Message { role: Role::User, content: "Part 1".to_string(), timestamp: t1 },
            Message { role: Role::User, content: "Part 2".to_string(), timestamp: t2 },
            Message { role: Role::Assistant, content: "Response".to_string(), timestamp: t2 },
        ];
        let joined = join_consecutive_messages(messages);
        assert_eq!(joined.len(), 2);
        assert_eq!(joined[0].content, "Part 1\n\nPart 2");
        assert_eq!(joined[0].timestamp, t2); // Uses latest timestamp
        assert_eq!(joined[1].content, "Response");
    }

    #[test]
    fn test_join_consecutive_messages_multiple_same_role() {
        let now = Utc::now();
        let messages = vec![
            Message { role: Role::Assistant, content: "A".to_string(), timestamp: now },
            Message { role: Role::Assistant, content: "B".to_string(), timestamp: now },
            Message { role: Role::Assistant, content: "C".to_string(), timestamp: now },
        ];
        let joined = join_consecutive_messages(messages);
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].content, "A\n\nB\n\nC");
    }
}

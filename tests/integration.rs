use insta::assert_snapshot;
use ratatui::{backend::TestBackend, Terminal};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;

// Serialize tests since they modify env vars
static TEST_MUTEX: Mutex<()> = Mutex::new(());

fn lock_test() -> std::sync::MutexGuard<'static, ()> {
    // Handle poisoned mutex from failed tests
    TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
}

/// Get the path to test fixtures
fn fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Setup test environment with fixtures
fn setup_test_env() -> TempDir {
    let temp_dir = TempDir::new().unwrap();

    // Copy fixtures to temp dir
    let fixtures = fixtures_path();
    let temp_path = temp_dir.path();

    // Copy .claude directory
    let claude_src = fixtures.join(".claude");
    let claude_dst = temp_path.join(".claude");
    copy_dir_recursive(&claude_src, &claude_dst);

    // Copy .codex directory
    let codex_src = fixtures.join(".codex");
    let codex_dst = temp_path.join(".codex");
    copy_dir_recursive(&codex_src, &codex_dst);

    temp_dir
}

fn create_opencode_database(home: &std::path::Path) -> PathBuf {
    let root = home.join(".local/share/opencode");
    create_opencode_database_at(&root)
}

fn create_opencode_database_at(root: &std::path::Path) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
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
    for (session, message, updated, text) in [
        ("ses_sqlite_a", "msg_sqlite_a", 2_000_i64, "sqlite alpha needle"),
        ("ses_sqlite_b", "msg_sqlite_b", 3_000_i64, "sqlite beta needle"),
    ] {
        connection
            .execute(
                "INSERT INTO session VALUES (?1, NULL, '/sqlite/project', 1000, ?2)",
                params![session, updated],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO message VALUES (?1, ?2, 1500, ?3)",
                params![message, session, r#"{"role":"user"}"#],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO part VALUES (?1, ?2, ?3, ?4)",
                params![
                    format!("prt_{session}"),
                    message,
                    session,
                    serde_json::json!({"type": "text", "text": text}).to_string()
                ],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO session VALUES ('ses_sqlite_child', 'ses_sqlite_a', '/sqlite/project', 1000, 9000)",
            [],
        )
        .unwrap();
    database
}

fn create_legacy_opencode_session(home: &std::path::Path) -> PathBuf {
    let storage = home.join(".local/share/opencode/storage");
    let session = storage.join("session/project/ses_legacy.json");
    std::fs::create_dir_all(session.parent().unwrap()).unwrap();
    std::fs::create_dir_all(storage.join("message/ses_legacy")).unwrap();
    std::fs::create_dir_all(storage.join("part/msg_legacy")).unwrap();
    std::fs::write(
        &session,
        r#"{"id":"ses_legacy","directory":"/legacy/project","time":{"created":1000}}"#,
    )
    .unwrap();
    std::fs::write(
        storage.join("message/ses_legacy/msg_legacy.json"),
        r#"{"id":"msg_legacy","sessionID":"ses_legacy","role":"user","time":{"created":1100}}"#,
    )
    .unwrap();
    std::fs::write(
        storage.join("part/msg_legacy/prt_legacy.json"),
        r#"{"id":"prt_legacy","type":"text","text":"legacy pruning needle"}"#,
    )
    .unwrap();
    session
}

/// Recursively copy a directory
fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) {
    if !src.exists() {
        return;
    }
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

/// Wait for indexing to complete, polling up to max_polls times
fn wait_for_indexing(app: &mut recall::App, max_polls: usize) {
    for _ in 0..max_polls {
        app.poll_index_updates();
        if !app.indexing {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Check if buffer contains text
fn buffer_contains(terminal: &Terminal<TestBackend>, text: &str) -> bool {
    let buffer = terminal.backend().buffer();
    let content: String = buffer
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    content.contains(text)
}

/// Render app to test terminal
fn render_app(app: &mut recall::App) -> Terminal<TestBackend> {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| recall::ui::render(f, app)).unwrap();
    terminal
}

/// Convert terminal buffer to string for snapshot testing
fn buffer_to_string(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let mut result = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = buffer.cell((x, y)).unwrap();
            result.push_str(cell.symbol());
        }
        // Trim trailing whitespace from each line
        while result.ends_with(' ') {
            result.pop();
        }
        result.push('\n');
    }
    // Remove trailing empty lines
    while result.ends_with("\n\n") {
        result.pop();
    }
    result
}

// =============================================================================
// Tests
// =============================================================================

#[test]
fn test_discovers_claude_sessions() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let files = recall::parser::discover_session_files();

    std::env::remove_var("RECALL_HOME_OVERRIDE");

    assert!(!files.is_empty(), "Should discover Claude session files");
    assert!(
        files.iter().any(|f| f.to_string_lossy().contains(".claude/projects")),
        "Should find files in .claude/projects"
    );
}

#[test]
fn test_discovers_codex_sessions() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let files = recall::parser::discover_session_files();

    std::env::remove_var("RECALL_HOME_OVERRIDE");

    assert!(
        files.iter().any(|f| f.to_string_lossy().contains(".codex/sessions")),
        "Should find files in .codex/sessions"
    );
}

#[test]
fn test_search_finds_matching_content() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let mut app = recall::App::new(String::new()).unwrap();
    wait_for_indexing(&mut app, 100);

    // Search for content from Claude fixture
    for c in "hello".chars() {
        app.on_char(c);
    }
    app.flush_pending_search();

    std::env::remove_var("RECALL_HOME_OVERRIDE");

    assert!(!app.results.is_empty(), "Should find results for 'hello'");
    assert!(
        app.results.iter().any(|r| r.session.id == "test-claude-123"),
        "Should find Claude session"
    );
}

#[test]
fn test_search_no_results_shows_hint() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let mut app = recall::App::new(String::new()).unwrap();
    wait_for_indexing(&mut app, 100);

    // Toggle from the default everywhere scope to folder scope.
    app.toggle_scope();

    // Search for something that doesn't exist
    for c in "xyznonexistent".chars() {
        app.on_char(c);
    }
    app.flush_pending_search();

    let terminal = render_app(&mut app);

    std::env::remove_var("RECALL_HOME_OVERRIDE");

    assert!(app.results.is_empty(), "Should have no results");
    // When scoped with no results, shows "No results. Press / to search everywhere."
    assert!(
        buffer_contains(&terminal, "No results"),
        "Should show 'No results' hint"
    );
}

#[test]
fn test_navigation_up_down() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let mut app = recall::App::new(String::new()).unwrap();
    wait_for_indexing(&mut app, 100);

    std::env::remove_var("RECALL_HOME_OVERRIDE");

    if app.results.len() >= 2 {
        assert_eq!(app.selected, 0, "Should start at first result");

        app.on_down();
        assert_eq!(app.selected, 1, "Should move to second result");

        app.on_up();
        assert_eq!(app.selected, 0, "Should move back to first result");
    }
}

#[test]
fn test_toggle_scope() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    let codex_session = temp_dir.path().join(".codex/sessions/test-codex.jsonl");
    let newer_codex_session = std::fs::read_to_string(&codex_session)
        .unwrap()
        .replace("test-codex-456", "test-codex-789")
        .replace("2025-01-16", "2025-01-17");
    std::fs::write(
        temp_dir.path().join(".codex/sessions/test-codex-newer.jsonl"),
        newer_codex_session,
    )
    .unwrap();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());
    std::env::set_var("RECALL_CWD_OVERRIDE", "/projects/webapp");

    let mut app = recall::App::new(String::new()).unwrap();
    wait_for_indexing(&mut app, 100);

    // Should start in global scope.
    assert!(matches!(app.search_scope, recall::SearchScope::Everything));

    app.selected = app
        .results
        .iter()
        .position(|result| result.session.id == "test-codex-456")
        .unwrap();
    assert!(app.selected > 0, "older CWD session should not be globally first");
    app.list_scroll = app.selected;

    // Toggle to the launch directory.
    app.toggle_scope();
    assert!(matches!(app.search_scope, recall::SearchScope::Folder(_)));
    assert_eq!(app.selected, 0);
    assert_eq!(app.list_scroll, 0);

    app.selected = app
        .results
        .iter()
        .position(|result| result.session.id == "test-codex-456")
        .unwrap();
    assert!(app.selected > 0, "older CWD session should not be CWD first");
    app.list_scroll = app.selected;

    // Toggle back to global.
    app.toggle_scope();
    assert!(matches!(app.search_scope, recall::SearchScope::Everything));
    assert_eq!(app.selected, 0);
    assert_eq!(app.list_scroll, 0);

    std::env::remove_var("RECALL_HOME_OVERRIDE");
    std::env::remove_var("RECALL_CWD_OVERRIDE");
}

#[test]
fn test_initial_everywhere_scope() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let mut app = recall::App::new(String::new()).unwrap();
    wait_for_indexing(&mut app, 100);

    std::env::remove_var("RECALL_HOME_OVERRIDE");

    assert!(matches!(app.search_scope, recall::SearchScope::Everything));
    assert!(
        !app.results.is_empty(),
        "Everywhere scope should show fixtures outside the launch directory"
    );
}

#[test]
fn test_renders_status_bar() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let mut app = recall::App::new(String::new()).unwrap();
    wait_for_indexing(&mut app, 100);

    let terminal = render_app(&mut app);

    std::env::remove_var("RECALL_HOME_OVERRIDE");

    // Status bar should show session count
    assert!(
        buffer_contains(&terminal, "sessions"),
        "Should show session count in status bar"
    );
    assert!(buffer_contains(&terminal, "^G"));
    assert!(buffer_contains(&terminal, "cwd"));
}

#[test]
fn test_search_during_indexing() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    // Create app but don't wait for full indexing
    let mut app = recall::App::new(String::new()).unwrap();

    // Poll just once to start processing
    app.poll_index_updates();

    // Should be able to search even during indexing
    app.on_char('t');
    app.on_char('e');
    app.on_char('s');
    app.on_char('t');
    app.flush_pending_search();

    let terminal = render_app(&mut app);

    std::env::remove_var("RECALL_HOME_OVERRIDE");

    // Should render without crashing
    assert!(terminal.backend().buffer().area.width > 0);
}

#[test]
fn test_escape_clears_query() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let mut app = recall::App::new(String::new()).unwrap();
    wait_for_indexing(&mut app, 100);

    // Type a query
    app.on_char('t');
    app.on_char('e');
    app.on_char('s');
    app.on_char('t');
    assert_eq!(app.query, "test");

    // Escape should clear
    app.on_escape();
    assert!(app.query.is_empty(), "Escape should clear query");
    assert!(!app.should_quit, "First escape should not quit");

    // Second escape should quit
    app.on_escape();
    assert!(app.should_quit, "Second escape should quit");

    std::env::remove_var("RECALL_HOME_OVERRIDE");
}

#[test]
fn test_backspace_removes_char() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let mut app = recall::App::new(String::new()).unwrap();

    app.on_char('a');
    app.on_char('b');
    app.on_char('c');
    assert_eq!(app.query, "abc");

    app.on_backspace();
    assert_eq!(app.query, "ab");

    std::env::remove_var("RECALL_HOME_OVERRIDE");
}

#[test]
fn test_initial_query() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());

    let app = recall::App::new("initial".to_string()).unwrap();

    std::env::remove_var("RECALL_HOME_OVERRIDE");

    assert_eq!(app.query, "initial", "Should have initial query");
}

// =============================================================================
// UI Snapshot Tests
// =============================================================================

// Note: We only snapshot "no results" states because result ordering from Tantivy
// is non-deterministic, making snapshots with results flaky.

const TEST_CWD: &str = "/test/cwd";

fn setup_ui_test() -> TempDir {
    let temp_dir = setup_test_env();
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());
    std::env::set_var("RECALL_CWD_OVERRIDE", TEST_CWD);
    temp_dir
}

fn cleanup_ui_test() {
    std::env::remove_var("RECALL_HOME_OVERRIDE");
    std::env::remove_var("RECALL_CWD_OVERRIDE");
}

#[test]
fn test_ui_no_query_folder_scope() {
    let _lock = lock_test();
    let _temp_dir = setup_ui_test();

    let mut app = recall::App::new_with_scope(
        String::new(),
        recall::InitialSearchScope::Folder,
    )
    .unwrap();
    wait_for_indexing(&mut app, 100);

    // Stay in folder scope (no sessions match CWD).
    let terminal = render_app(&mut app);

    cleanup_ui_test();

    assert_snapshot!(buffer_to_string(&terminal));
}

#[test]
fn test_ui_no_query_everywhere_scope() {
    let _lock = lock_test();
    // Use empty temp dir (no fixtures) so there are no results
    let temp_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(temp_dir.path().join(".claude/projects")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join(".codex/sessions")).unwrap();

    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());
    std::env::set_var("RECALL_CWD_OVERRIDE", TEST_CWD);

    let mut app = recall::App::new_with_scope(
        String::new(),
        recall::InitialSearchScope::Everything,
    )
    .unwrap();
    wait_for_indexing(&mut app, 100);

    let terminal = render_app(&mut app);

    cleanup_ui_test();

    assert_snapshot!(buffer_to_string(&terminal));
}

#[test]
fn test_ui_with_query_folder_scope_no_results() {
    let _lock = lock_test();
    let _temp_dir = setup_ui_test();

    let mut app = recall::App::new_with_scope(
        String::new(),
        recall::InitialSearchScope::Folder,
    )
    .unwrap();
    wait_for_indexing(&mut app, 100);

    // Stay in folder scope and search.
    for c in "zzzznotfound".chars() {
        app.on_char(c);
    }
    app.flush_pending_search();

    let terminal = render_app(&mut app);

    cleanup_ui_test();

    assert_snapshot!(buffer_to_string(&terminal));
}

#[test]
fn test_ui_with_query_everywhere_scope_no_results() {
    let _lock = lock_test();
    let _temp_dir = setup_ui_test();

    let mut app = recall::App::new(String::new()).unwrap();
    wait_for_indexing(&mut app, 100);

    // Stay in the default everywhere scope and search for something that doesn't exist.
    for c in "zzzznotfound".chars() {
        app.on_char(c);
    }
    app.flush_pending_search();

    let terminal = render_app(&mut app);

    cleanup_ui_test();

    assert_snapshot!(buffer_to_string(&terminal));
}

// =============================================================================
// CLI Integration Tests
// =============================================================================

use std::process::Command;

fn recall_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_recall"))
}

fn run_cli(args: &[&str], home_override: &std::path::Path) -> (String, String, bool) {
    let output = Command::new(recall_bin())
        .args(args)
        .env("RECALL_HOME_OVERRIDE", home_override)
        .output()
        .expect("Failed to run recall");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

#[test]
fn test_cli_search_returns_json() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (stdout, _stderr, success) = run_cli(
        &["search", "hello", "--limit", "5"],
        temp_dir.path(),
    );

    assert!(success, "CLI search should succeed");

    // Parse as JSON
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .expect("Output should be valid JSON");

    assert_eq!(json["query"], "hello");
    assert!(json["results"].is_array());
}

#[test]
fn test_cli_search_finds_fixture_content() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (stdout, _stderr, success) = run_cli(
        &["search", "hello", "--limit", "10"],
        temp_dir.path(),
    );

    assert!(success);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let results = json["results"].as_array().unwrap();

    // Should find the Claude fixture session
    assert!(
        results.iter().any(|r| r["session_id"] == "test-claude-123"),
        "Should find Claude fixture session"
    );
}

#[test]
fn test_cli_search_with_source_filter() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (stdout, _stderr, success) = run_cli(
        &["search", "hello", "--source", "claude", "--limit", "10"],
        temp_dir.path(),
    );

    assert!(success);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let results = json["results"].as_array().unwrap();

    // All results should be Claude
    for result in results {
        assert_eq!(result["source"], "claude");
    }
}

#[test]
fn test_cli_search_no_results() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (stdout, _stderr, success) = run_cli(
        &["search", "xyznonexistent12345"],
        temp_dir.path(),
    );

    assert!(success);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let results = json["results"].as_array().unwrap();

    assert!(results.is_empty(), "Should have no results for nonexistent query");
}

#[test]
fn test_cli_list_returns_json() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (stdout, _stderr, success) = run_cli(
        &["list", "--limit", "5"],
        temp_dir.path(),
    );

    assert!(success, "CLI list should succeed");

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .expect("Output should be valid JSON");

    assert!(json["sessions"].is_array());
}

#[test]
fn test_cli_list_with_source_filter() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (stdout, _stderr, success) = run_cli(
        &["list", "--source", "codex", "--limit", "10"],
        temp_dir.path(),
    );

    assert!(success);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let sessions = json["sessions"].as_array().unwrap();

    // All sessions should be Codex
    for session in sessions {
        assert_eq!(session["source"], "codex");
    }
}

#[test]
fn test_cli_read_returns_session() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (stdout, _stderr, success) = run_cli(
        &["read", "test-claude-123"],
        temp_dir.path(),
    );

    assert!(success, "CLI read should succeed");

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .expect("Output should be valid JSON");

    assert_eq!(json["session_id"], "test-claude-123");
    assert_eq!(json["source"], "claude");
    assert!(json["messages"].is_array());
    assert!(!json["messages"].as_array().unwrap().is_empty());
}

#[test]
fn test_cli_read_nonexistent_session() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (_stdout, stderr, success) = run_cli(
        &["read", "nonexistent-session-id"],
        temp_dir.path(),
    );

    assert!(!success, "Should fail for nonexistent session");
    assert!(stderr.contains("Session not found"), "Should show error message");
}

#[test]
fn test_cli_invalid_source() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (_stdout, stderr, success) = run_cli(
        &["search", "test", "--source", "invalid"],
        temp_dir.path(),
    );

    assert!(!success, "Should fail for invalid source");
    assert!(stderr.contains("Invalid source"), "Should show error message");
}

#[test]
fn test_cli_help() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    let (stdout, _stderr, success) = run_cli(
        &["--help"],
        temp_dir.path(),
    );

    assert!(success);
    assert!(stdout.contains("search"));
    assert!(stdout.contains("list"));
    assert!(stdout.contains("read"));
}

#[test]
fn test_cli_search_with_cwd_filter() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    // Search with matching cwd
    let (stdout, _stderr, success) = run_cli(
        &["search", "hello", "--cwd", "/test/project", "--limit", "10"],
        temp_dir.path(),
    );

    assert!(success);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let results = json["results"].as_array().unwrap();

    // Should find results with matching cwd
    assert!(!results.is_empty(), "Should find results with matching cwd");
    for result in results {
        assert_eq!(result["cwd"], "/test/project");
    }
}

#[test]
fn test_cli_search_with_cwd_filter_no_match() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    // Search with non-matching cwd
    let (stdout, _stderr, success) = run_cli(
        &["search", "hello", "--cwd", "/nonexistent/path", "--limit", "10"],
        temp_dir.path(),
    );

    assert!(success);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let results = json["results"].as_array().unwrap();

    assert!(results.is_empty(), "Should have no results for non-matching cwd");
}

#[test]
fn test_cli_list_with_cwd_filter() {
    let _lock = lock_test();
    let temp_dir = setup_test_env();

    // List with matching cwd
    let (stdout, _stderr, success) = run_cli(
        &["list", "--cwd", "/test/project", "--limit", "10"],
        temp_dir.path(),
    );

    assert!(success);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let sessions = json["sessions"].as_array().unwrap();

    // Should find sessions with matching cwd
    for session in sessions {
        assert_eq!(session["cwd"], "/test/project");
    }
}

#[test]
fn test_opencode_sqlite_cli_search_list_read_and_session_scope() {
    let _lock = lock_test();
    let temp_dir = TempDir::new().unwrap();
    create_opencode_database(temp_dir.path());

    let (stdout, stderr, success) = run_cli(
        &["search", "alpha", "--source", "opencode"],
        temp_dir.path(),
    );
    assert!(success, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["results"][0]["session_id"], "ses_sqlite_a");
    assert_eq!(json["results"][0]["source"], "opencode");
    assert_eq!(json["results"][0]["cwd"], "/sqlite/project");
    assert_eq!(
        json["results"][0]["resume_command"],
        "opencode --session ses_sqlite_a"
    );

    let (stdout, stderr, success) = run_cli(
        &["search", "needle", "--session", "ses_sqlite_b"],
        temp_dir.path(),
    );
    assert!(success, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["results"][0]["session_id"], "ses_sqlite_b");

    let (stdout, stderr, success) = run_cli(&["read", "ses_sqlite_a"], temp_dir.path());
    assert!(success, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["messages"][0]["content"], "sqlite alpha needle");
    assert_eq!(json["timestamp"], "1970-01-01T00:00:02Z");

    let (stdout, stderr, success) = run_cli(
        &["list", "--source", "opencode", "--limit", "10"],
        temp_dir.path(),
    );
    assert!(success, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let ids: Vec<_> = json["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["ses_sqlite_b", "ses_sqlite_a"]);
    assert!(!ids.contains(&"ses_sqlite_child"));
}

#[test]
fn test_opencode_uses_xdg_data_home() {
    let _lock = lock_test();
    let temp_dir = TempDir::new().unwrap();
    let database = create_opencode_database_at(&temp_dir.path().join("opencode"));
    let previous = std::env::var_os("XDG_DATA_HOME");
    std::env::remove_var("RECALL_HOME_OVERRIDE");
    std::env::set_var("XDG_DATA_HOME", temp_dir.path());

    let sessions = recall::parser::discover_sessions();

    if let Some(previous) = previous {
        std::env::set_var("XDG_DATA_HOME", previous);
    } else {
        std::env::remove_var("XDG_DATA_HOME");
    }
    assert!(sessions.iter().any(|session| {
        session.path == database
            && session.database_session_id.as_deref() == Some("ses_sqlite_a")
    }));
}

#[test]
fn test_deleted_legacy_opencode_file_is_pruned() {
    let _lock = lock_test();
    let temp_dir = TempDir::new().unwrap();
    let session_file = create_legacy_opencode_session(temp_dir.path());

    let (stdout, stderr, success) = run_cli(&["search", "legacy"], temp_dir.path());
    assert!(success, "{stderr}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap()["results"][0]["session_id"],
        "ses_legacy"
    );

    std::fs::remove_file(session_file).unwrap();
    let (stdout, stderr, success) = run_cli(&["search", "legacy"], temp_dir.path());
    assert!(success, "{stderr}");
    assert!(serde_json::from_str::<serde_json::Value>(&stdout).unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(temp_dir.path().join(".cache/recall/state.json")).unwrap(),
    )
    .unwrap();
    assert!(state["indexed_sessions"].as_object().unwrap().is_empty());
}

#[test]
fn test_opencode_sqlite_updates_independently_and_prunes_rows_and_database() {
    let _lock = lock_test();
    let temp_dir = TempDir::new().unwrap();
    let database = create_opencode_database(temp_dir.path());

    let (_, stderr, success) = run_cli(&["list"], temp_dir.path());
    assert!(success, "{stderr}");
    let state_path = temp_dir.path().join(".cache/recall/state.json");
    let initial: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    let beta_key = initial["indexed_sessions"]
        .as_object()
        .unwrap()
        .keys()
        .find(|key| key.contains("ses_sqlite_b"))
        .unwrap()
        .clone();
    let beta_state = initial["indexed_sessions"][&beta_key].clone();

    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE session SET time_updated = 4000 WHERE id = 'ses_sqlite_a'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE part SET data = ?1 WHERE session_id = 'ses_sqlite_a'",
            [serde_json::json!({"type": "text", "text": "independent update"}).to_string()],
        )
        .unwrap();
    drop(connection);

    let (stdout, stderr, success) = run_cli(&["search", "independent"], temp_dir.path());
    assert!(success, "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["results"][0]["session_id"], "ses_sqlite_a");
    let updated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(updated["indexed_sessions"][&beta_key], beta_state);

    let connection = Connection::open(&database).unwrap();
    connection
        .execute("DELETE FROM part WHERE session_id = 'ses_sqlite_a'", [])
        .unwrap();
    connection
        .execute("DELETE FROM message WHERE session_id = 'ses_sqlite_a'", [])
        .unwrap();
    connection
        .execute("DELETE FROM session WHERE id = 'ses_sqlite_a'", [])
        .unwrap();
    drop(connection);
    let (stdout, stderr, success) = run_cli(&["search", "independent"], temp_dir.path());
    assert!(success, "{stderr}");
    assert!(serde_json::from_str::<serde_json::Value>(&stdout).unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
    let pruned: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert!(!pruned["indexed_sessions"]
        .as_object()
        .unwrap()
        .keys()
        .any(|key| key.contains("ses_sqlite_a")));

    std::fs::remove_file(database).unwrap();
    let (stdout, stderr, success) = run_cli(
        &["list", "--source", "opencode", "--limit", "10"],
        temp_dir.path(),
    );
    assert!(success, "{stderr}");
    assert!(serde_json::from_str::<serde_json::Value>(&stdout).unwrap()["sessions"]
        .as_array()
        .unwrap()
        .is_empty());
    let pruned: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert!(pruned["indexed_sessions"].as_object().unwrap().is_empty());
}

#[test]
fn test_unloadable_opencode_selection_shows_status_error() {
    let _lock = lock_test();
    let temp_dir = TempDir::new().unwrap();
    let database = create_opencode_database(temp_dir.path());
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());
    let mut app = recall::App::new("alpha".to_string()).unwrap();
    wait_for_indexing(&mut app, 100);
    app.search().unwrap();
    assert_eq!(app.results[0].session.id, "ses_sqlite_a");
    std::fs::remove_file(database).unwrap();
    app.on_enter();
    std::env::remove_var("RECALL_HOME_OVERRIDE");

    let status = app.status.unwrap();
    assert!(status.contains("Cannot load OpenCode session ses_sqlite_a"));
    assert!(status.contains("opencode.db"));
    assert!(app.should_resume.is_none());
}

#[test]
fn test_tui_background_indexing_prunes_deleted_sqlite_row() {
    let _lock = lock_test();
    let temp_dir = TempDir::new().unwrap();
    let database = create_opencode_database(temp_dir.path());
    std::env::set_var("RECALL_HOME_OVERRIDE", temp_dir.path());
    let mut initial = recall::App::new(String::new()).unwrap();
    wait_for_indexing(&mut initial, 100);
    drop(initial);

    let connection = Connection::open(database).unwrap();
    connection
        .execute("DELETE FROM part WHERE session_id = 'ses_sqlite_a'", [])
        .unwrap();
    connection
        .execute("DELETE FROM message WHERE session_id = 'ses_sqlite_a'", [])
        .unwrap();
    connection
        .execute("DELETE FROM session WHERE id = 'ses_sqlite_a'", [])
        .unwrap();
    drop(connection);

    let mut app = recall::App::new("alpha".to_string()).unwrap();
    wait_for_indexing(&mut app, 100);
    app.search().unwrap();
    std::env::remove_var("RECALL_HOME_OVERRIDE");
    assert!(app.results.is_empty());
}

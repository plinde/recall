//! Shared indexing logic for both background (TUI) and synchronous (CLI) modes

use super::state::IndexState;
use super::SessionIndex;
use crate::parser::{self, SessionLocator};
use anyhow::Result;
use std::collections::HashSet;
use tantivy::IndexWriter;

/// Progress information during indexing
pub struct IndexProgress {
    pub indexed: usize,
    pub total: usize,
}

/// Callback for reporting indexing progress
pub type ProgressCallback = Box<dyn FnMut(IndexProgress) + Send>;

/// Callback for notifying that the index should be reloaded
pub type ReloadCallback = Box<dyn FnMut() + Send>;

/// Discovers sessions and sorts them by their source-specific timestamp.
pub fn discover_and_sort_files() -> Vec<SessionLocator> {
    let mut locators = parser::discover_sessions();
    locators.sort_by_key(|locator| std::cmp::Reverse(locator.sort_timestamp));
    locators
}

/// Remove sessions which are no longer present, including missing SQLite rows.
pub fn prune_stale_sessions(
    index: &SessionIndex,
    writer: &mut IndexWriter,
    state: &mut IndexState,
    discovered: &[SessionLocator],
) -> usize {
    let discovered: HashSet<&str> = discovered.iter().map(|item| item.identity.as_str()).collect();
    let stale: Vec<String> = state
        .identities()
        .filter(|identity| !discovered.contains(identity.as_str()))
        .cloned()
        .collect();
    for identity in &stale {
        index.delete_session(writer, identity);
        state.remove(identity);
    }
    stale.len()
}

/// Index a batch of files, calling progress callbacks as work proceeds.
///
/// - `on_progress`: Called every 50 files with current progress
/// - `on_reload`: Called every 200 files after a commit (for incremental updates)
///
/// Returns the number of files successfully indexed.
pub fn index_files(
    index: &SessionIndex,
    writer: &mut IndexWriter,
    state: &mut IndexState,
    files: &[SessionLocator],
    mut on_progress: Option<ProgressCallback>,
    mut on_reload: Option<ReloadCallback>,
) -> Result<usize> {
    let total = files.len();
    let mut indexed = 0;

    for (i, locator) in files.iter().enumerate() {
        // Delete existing documents for this file (in case of update)
        index.delete_session(writer, &locator.identity);

        // Parse and index
        match parser::parse_session(locator) {
            Ok(session) => {
                if !session.messages.is_empty() {
                    let _ = index.index_session(writer, &session);
                }
                // Mark as indexed even if empty (so we don't reprocess it)
                state.mark_indexed(locator);
                indexed += 1;
            }
            Err(_) => {
                // Skip failed files (they might be incomplete/corrupted)
                // Don't mark as indexed so we retry next time
                state.remove(&locator.identity);
            }
        }

        // Progress update every 50 files or at the end
        if (i + 1) % 50 == 0 || i + 1 == total {
            if let Some(ref mut callback) = on_progress {
                callback(IndexProgress {
                    indexed: i + 1,
                    total,
                });
            }
        }

        // Commit and notify for reload every 200 files
        if (i + 1) % 200 == 0 {
            writer.commit()?;
            if let Some(ref mut callback) = on_reload {
                callback();
            }
        }
    }

    // Final commit
    writer.commit()?;

    Ok(indexed)
}

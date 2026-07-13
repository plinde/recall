//! Synchronous indexing for CLI mode

use super::indexer::{discover_and_sort_files, index_files, prune_stale_sessions, IndexProgress};
use super::schema::default_index_path;
use super::state::IndexState;
use super::SessionIndex;
use anyhow::Result;
use std::io::Write;

/// Ensure index is up-to-date before running CLI queries.
/// Discovers new/modified session files and indexes them synchronously.
/// Progress is printed to stderr.
pub fn ensure_index_fresh(index: &SessionIndex) -> Result<()> {
    // state.json lives alongside the index directory
    let index_path = default_index_path();
    let state_path = index_path
        .parent()
        .map(|p| p.join("state.json"))
        .unwrap_or_else(|| index_path.join("state.json"));

    let mut state = IndexState::load(&state_path)?;

    // Discover all session files
    let files = discover_and_sort_files();

    // Find files that need indexing
    let files_to_index: Vec<_> = files
        .iter()
        .filter(|f| state.needs_reindex(f))
        .cloned()
        .collect();

    let total = files_to_index.len();
    let rebuild = state.take_rebuild_required();
    let known: std::collections::HashSet<_> = files.iter().map(|f| f.identity.as_str()).collect();
    let has_stale = state.identities().any(|id| !known.contains(id.as_str()));
    if total == 0 && !has_stale && !rebuild {
        return Ok(());
    }

    eprintln!(
        "Indexing {} session{}...",
        total,
        if total == 1 { "" } else { "s" }
    );

    let mut writer = index.writer()?;
    if rebuild {
        index.clear(&mut writer)?;
    }
    prune_stale_sessions(index, &mut writer, &mut state, &files);

    // Progress callback prints to stderr
    let on_progress = Box::new(|p: IndexProgress| {
        eprint!("\rIndexing {}/{}...", p.indexed, p.total);
        let _ = std::io::stderr().flush();
    });

    if total > 0 {
        index_files(
            index,
            &mut writer,
            &mut state,
            &files_to_index,
            Some(on_progress),
            None,
        )?;
    } else {
        writer.commit()?;
    }

    state.save(&state_path)?;

    // Clear progress line and print completion
    if total > 0 {
        eprintln!(
            "\rIndexed {} session{}.    ",
            total,
            if total == 1 { "" } else { "s" }
        );
    }

    // Reload index to see new data
    index.reload()?;

    Ok(())
}

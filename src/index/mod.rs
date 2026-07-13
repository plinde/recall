mod indexer;
mod schema;
mod state;
mod sync;

pub use indexer::{discover_and_sort_files, index_files, prune_stale_sessions, IndexProgress};
pub use schema::SessionIndex;
pub use state::IndexState;
pub use sync::ensure_index_fresh;

// Relationships:
// - `external:sessions-db`: Reads and writes session rows
// - `src/storage/audit.rs`: Records every write for later review

//! Session records.

use super::audit;

pub struct Repo;

impl Repo {
    pub fn open() -> Self {
        Repo
    }

    pub fn save(&self, token: &str) {
        audit::record(token);
    }
}

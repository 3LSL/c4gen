// Relationships:
// - `src/session/token.rs`: Signs and verifies session tokens
// - `src/storage/repo.rs`: Stores and retrieves session records

//! The session service.

use super::token;
use crate::storage::{audit, Repo};

pub struct Service {
    repo: Repo,
}

impl Service {
    pub fn new() -> Self {
        Service { repo: Repo::open() }
    }

    pub fn run(&self) {
        let signed = token::sign("ada");
        if token::verify(&signed) {
            self.repo.save(&signed);
            audit::record("login");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::checks::helper;

    #[test]
    fn runs() {
        helper();
        Service::new().run();
    }
}

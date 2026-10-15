// Relationships:
// None.

//! Token signing.

pub fn sign(user: &str) -> String {
    format!("signed:{user}")
}

pub fn verify(token: &str) -> bool {
    token.starts_with("signed:")
}

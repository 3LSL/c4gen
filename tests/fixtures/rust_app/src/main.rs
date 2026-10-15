// Relationships:
// - `external:operator` (inbound): Starts the application from a terminal
// - `src/session/service.rs`: Starts the session service

//! Entry point.

mod session;
mod storage;

use session::Service;

fn main() {
    let service = Service::new();
    service.run();
}

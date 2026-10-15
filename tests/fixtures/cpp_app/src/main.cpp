// Relationships:
// - `external:operator` (inbound): Starts the application from a terminal
// - `src/session.h`: Starts a login session

#include "all.h"

int main() {
    app::Session session;
    session.login("ada");
    return 0;
}

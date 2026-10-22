// Relationships:
// - `external:operator` (inbound): Starts the application from a terminal
// - `src/devtools.ts`: Loads developer tools during development
// - `src/session/service.ts`: Starts the session service
// - `src/ui/App.tsx`: Renders the application shell

import { SessionService } from "@/session";
import { renderApp } from "./ui/App.js";

const service = new SessionService();
service.login("ada");
renderApp(service);

if (import.meta.env?.DEV) {
  void import("./devtools");
}

// Relationships:
// - `src/session/service.ts`: Logs users in from the sign-in form
// - `src/ui/Button.tsx`: Draws the sign-in button

import { createRoot } from "react-dom/client";
import type { SessionService } from "../session";
import { Button } from "./Button";

export function renderApp(service: SessionService): void {
  const root = createRoot(document.getElementById("root")!);
  root.render(
    <main className="app">
      <h1>Don't wait, sign in</h1>
      <Button label="Sign in" onClick={() => service.login("ada")} />
    </main>,
  );
}

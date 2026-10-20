// Relationships:
// - `external:web-client` (inbound): Submits login and logout requests
// - `src/session/tokens.ts`: Signs and verifies session tokens
// - `src/session/types.ts`: Describes returned session records
// - `src/storage/repo.ts`: Stores and retrieves session records

import * as storage from "@/storage";
import TokenSigner from "./tokens";
import type { Session } from "./types";

export class SessionService {
  private readonly signer = new TokenSigner();

  login(user: string): Session {
    const token = this.signer.sign(user);
    storage.repo.saveSession({ user, token });
    storage.cache.remember(user, token);
    return { user, token };
  }

  logout(session: Session): void {
    if (this.signer.verify(session.token)) {
      storage.repo.deleteSession(session.token);
    }
  }
}

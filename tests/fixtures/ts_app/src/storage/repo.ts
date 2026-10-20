// Relationships:
// - `external:orders-db`: Reads and writes session rows
// - `src/session/types.ts`: Describes stored session records

import type { Session } from "@/session";

const rows: Session[] = [];

export function saveSession(session: Session): void {
  rows.push(session);
}

export function deleteSession(token: string): void {
  const index = rows.findIndex((row) => row.token === token);
  if (index >= 0) {
    rows.splice(index, 1);
  }
}

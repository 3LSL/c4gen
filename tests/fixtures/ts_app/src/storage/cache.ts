// Relationships:
// None.

const entries = new Map<string, string>();

export const cache = {
  remember(user: string, token: string): void {
    entries.set(user, token);
  },
};

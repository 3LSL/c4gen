import { describe, it } from "vitest";
import { SessionService } from "./service";

describe("SessionService", () => {
  it("logs in", () => {
    new SessionService().login("ada");
  });
});

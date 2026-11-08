import { describe, expect, test } from "vitest";
import { commandErrorMessage } from "./commandError";

describe("commandErrorMessage", () => {
  test("uses the message from typed command errors", () => {
    expect(commandErrorMessage({ code: "NotFound", message: "Native item not found", remedy: null })).toBe("Native item not found");
  });

  test("never renders object coercion or empty messages", () => {
    expect(commandErrorMessage({ code: "Internal", message: "" }, "Fallback explanation")).toBe("Fallback explanation");
    expect(commandErrorMessage({ code: "Internal" }, "Fallback explanation")).toBe("Fallback explanation");
  });

  test("renders only the message; extra fields (secrets, env, paths) never leak", () => {
    // A malformed or future command error must not print fields the UI does
    // not know about — in particular anything resembling credentials or env
    // values that a Debug dump would carry.
    const error = { code: "Internal", message: "Save failed", env: { TOKEN: "sk-secret" }, token: "sk-secret", detail: { password: "hunter2" } };
    expect(commandErrorMessage(error, "Fallback explanation")).toBe("Save failed");
    expect(commandErrorMessage(new Error("boom"), "Fallback explanation")).toBe("boom");
    expect(commandErrorMessage(undefined, "Fallback explanation")).toBe("Fallback explanation");
    expect(commandErrorMessage(null, "Fallback explanation")).toBe("Fallback explanation");
    expect(commandErrorMessage({ message: "   " }, "Fallback explanation")).toBe("Fallback explanation");
  });
});

import { expect, test } from "./support/tauri-mock";

test("Skills list renders populated table, filter chips, native banner and shadowed rows", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  const main = page.getByRole("main", { name: "Skills" });

  await expect(main.getByText("code-review")).toBeVisible();
  await expect(main.getByText("native-helper", { exact: true })).toBeVisible();
  await expect(page.getByRole("region", { name: "Unmanaged skills banner" })).toBeVisible();
  await expect(page).toHaveScreenshot("skills-populated.png", { animations: "disabled", caret: "hide" });

  // Filter chips
  const filterGroup = page.getByRole("group", { name: "Filter chips" });
  await filterGroup.getByText("Project").click();
  await expect(main.getByText("native-helper", { exact: true })).toBeVisible();
  await expect(main.getByText("code-review")).toBeHidden();
  await expect(page).toHaveScreenshot("skills-filtered.png", { animations: "disabled", caret: "hide" });
});

test("MCP list renders servers matrix and filter chips", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  const main = page.getByRole("main", { name: "MCP Servers" });

  await expect(main.getByText("Visual server")).toBeVisible();
  await expect(main.getByText("Native server")).toBeVisible();
  await expect(page.getByRole("region", { name: "Unmanaged servers banner" })).toBeVisible();
  await expect(page).toHaveScreenshot("mcp-list-populated.png", { animations: "disabled", caret: "hide" });
});

test("MCP detail renders configuration, tools, health history and Loaded by", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  const main = page.getByRole("main", { name: "MCP Servers" });

  // Open menu on Visual server and click Open
  await page.getByRole("button", { name: "Actions for Visual server" }).click();
  await page.getByRole("menuitem", { name: "Open" }).click();

  const detailMain = page.getByRole("main", { name: "MCP Server Detail" });
  await expect(detailMain.getByRole("heading", { name: "Visual server" })).toBeVisible();
  await expect(detailMain.getByText("stdio")).toBeVisible();
  await expect(detailMain.getByText("npx")).toBeVisible();
  await expect(detailMain.getByText("Health")).toBeVisible();
  await expect(detailMain.getByText("Loaded by")).toBeVisible();
  await expect(page).toHaveScreenshot("mcp-detail-populated.png", { animations: "disabled", caret: "hide" });
});

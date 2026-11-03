import { expect, test } from "@playwright/test";

test("fresh preview PostgreSQL is opt-in, runtime-only and can be disabled", async ({ page }) => {
  await page.goto("/");
  const server = page.getByTestId("found-optiplex-7050");
  await server.getByRole("button", { name: "Connect" }).click();
  await server.getByTestId("short-code").fill("K7QF-M2XD");
  await server.getByTestId("short-submit").click();
  await page.getByTestId("papp-shop").getByRole("button", { name: /shop/ }).first().click();
  await page.getByTestId("tab-previews").click();
  const card = page.getByTestId("preview-database-card");
  await expect(card).toBeVisible();
  await expect(card.getByTestId("preview-database-enabled")).not.toBeChecked();
  await card.getByTestId("preview-database-enabled").check();
  await card.getByLabel("Migration command").fill("npm run migrate");
  await card.getByLabel("Synthetic seed command").fill("npm run seed:preview");
  await card.getByTestId("save-preview-database").click();
  await expect(page.getByText("Fresh PostgreSQL enabled for previews", { exact: true })).toBeVisible();
  await page.getByTestId("tab-env").click();
  await expect(page.getByTestId("preview-var-DATABASE_URL")).toContainText("Fresh PostgreSQL · runtime only");
  await page.getByTestId("tab-previews").click();
  await card.getByTestId("preview-database-enabled").uncheck();
  await card.getByTestId("save-preview-database").click();
  await expect(page.getByText("Preview database cleanup requested", { exact: true })).toBeVisible();
  await expect(card.getByTestId("preview-database-enabled")).not.toBeChecked();
});

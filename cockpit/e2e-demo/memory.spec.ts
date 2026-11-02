import { expect, test } from "@playwright/test";

test("memory budget separates measured zero, unavailable readings and promised headroom", async ({ page }) => {
  await page.setViewportSize({ width: 1040, height: 680 });
  await page.goto("/");
  const server = page.getByTestId("found-dell-e7450");
  await server.getByRole("button", { name: "Connect" }).click();
  await server.getByTestId("short-code").fill("K7QF-M2XD");
  await server.getByTestId("short-submit").click();
  await page.getByTestId("nav-health").click();
  const card = page.getByTestId("memory-budget");
  await expect(card).toBeVisible();
  await expect(card.getByText("Fresh capacity")).toHaveCount(1);
  await card.getByRole("combobox", { name: "Memory server" }).selectOption("node_demopi5000000000");
  await expect(page.getByTestId("capacity-node_demopi5000000000-apps")).toHaveText("Unknown");
  await expect(page.getByTestId("capacity-node_demopi5000000000-databases")).toHaveText("0 MiB");
  await expect(card.getByText("Safe admission headroom")).toHaveCount(1);
  await expect(card.getByText(/Swap is not added to capacity/)).toHaveCount(1);
  await card.screenshot({ path: "/tmp/vfy-m4-memory-card.png" });
});

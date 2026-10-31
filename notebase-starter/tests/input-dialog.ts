import { expect, type Page } from '@playwright/test';
export async function submitInput(page: Page, value: string) {
  await expect(page.locator('#inputDialog')).toBeVisible();
  await page.locator('#inputDialogValue').fill(value);
  await page.locator('#inputDialogConfirm').click();
  await expect(page.locator('#inputDialog')).toHaveCount(0);
}

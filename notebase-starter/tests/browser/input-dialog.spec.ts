import { test, expect } from '@playwright/test';
import { submitInput } from '../input-dialog';

test('应用输入弹窗居中，空标题就地提示，取消和Escape保留当前笔记', async ({ page }, testInfo) => {
  const native: string[] = []; page.on('dialog', async d => { native.push(d.type()); await d.dismiss(); });
  await page.goto('/'); await expect(page.locator('#new')).toBeEnabled();
  await expect(page.locator('.library-sidebar>.brand')).toBeVisible();
  await page.locator('#sidebarTools>summary').click(); await expect(page.locator('label').filter({ has: page.locator('#vaultPath') })).toHaveText('笔记库路径');
  await page.getByRole('button', { name: '欢迎.md', exact: true }).click(); const original = await page.locator('.cm-content').textContent();
  const count = await page.locator('#notes button').count(); await page.keyboard.press('Control+n');
  await expect(page.locator('#inputDialogValue')).toBeFocused();
  await page.locator('#inputDialogValue').fill('   '); await page.keyboard.press('Enter');
  await expect(page.locator('#inputDialogError')).toHaveText('笔记标题不能为空'); await expect(page.locator('#inputDialogValue')).toHaveAttribute('aria-invalid', 'true');
  await expect(page.locator('#inputDialogConfirm')).toHaveClass('primary');
  const box = (await page.locator('#inputDialog').boundingBox())!, size = page.viewportSize()!;
  expect(Math.abs(box.x + box.width / 2 - size.width / 2)).toBeLessThan(2); expect(Math.abs(box.y + box.height / 2 - size.height / 2)).toBeLessThan(2);
  await page.screenshot({ path: testInfo.outputPath('input-warning-light.png') });
  await page.locator('#inputDialogCancel').click(); await expect(page.locator('#inputDialog')).toHaveCount(0);
  await expect(page.locator('.cm-content')).toHaveText(original!); await expect(page.locator('#notes button')).toHaveCount(count);
  await page.keyboard.press('Control+n'); await page.keyboard.press('Escape'); await expect(page.locator('#inputDialog')).toHaveCount(0);
  await page.keyboard.press('Control+n'); await submitInput(page, '应用内输入'); await expect(page.locator('#path')).toHaveText('应用内输入.md');
  await page.locator('#settings').click(); await page.locator('#theme').selectOption('dark'); await page.locator('#settingsSave').click();
  await page.keyboard.press('Control+n'); await page.locator('#inputDialogConfirm').click(); await expect(page.locator('#inputDialogError')).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('input-warning-dark.png') }); await page.locator('#inputDialogCancel').click(); expect(native).toEqual([]);
});

test('重命名后端校验显示在输入框下方，修正后保留原有计划确认流程', async ({ page }) => {
  await page.goto('/'); await page.getByRole('button', { name: '欢迎.md', exact: true }).click(); await page.locator('#rename').click();
  await expect(page.locator('#inputDialogValue')).toHaveValue('欢迎'); await page.locator('#inputDialogValue').fill('知识组织'); await page.locator('#inputDialogConfirm').click();
  await expect(page.locator('#inputDialogError')).toContainText('目标已存在'); await expect(page.locator('#path')).toHaveText('欢迎.md');
  await submitInput(page, '新的名称'); await expect(page.locator('#renameDialog')).toContainText('新的名称.md'); await page.locator('#renameCancel').click();
  await expect(page.locator('#notes')).toContainText('欢迎.md'); await expect(page.locator('#notes')).not.toContainText('新的名称.md');
});

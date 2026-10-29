import { test, expect } from '@playwright/test';
test('主题和编辑器偏好持久化，冲突快捷键不覆盖旧设置', async ({ page }) => {
  await page.goto('/'); await expect(page.locator('#new')).toBeEnabled(); await page.locator('#settings').click();
  await page.locator('#theme').selectOption('dark'); await page.locator('#fontSize').fill('18'); await page.locator('#settingsSave').click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark'); await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark'); await page.locator('#settings').click(); await expect(page.locator('#fontSize')).toHaveValue('18');
  await page.locator('#key-new').fill('Ctrl+S'); await page.locator('#settingsSave').click(); await expect(page.locator('#settingsError')).toContainText('E_KEYMAP_CONFLICT');
  await page.locator('#settingsClose').click(); expect(await page.evaluate(() => JSON.parse(localStorage.getItem('notebase-demo-preferences')!).keymap.new)).toBe('Ctrl+N');
});
test('恢复标签页和当前笔记，缓存中不存正文', async ({ page }) => {
  await page.goto('/'); await page.getByRole('button', { name: '欢迎.md', exact: true }).click(); await page.getByRole('button', { name: '知识组织.md', exact: true }).click();
  await page.reload(); await expect(page.locator('#path')).toHaveText('知识组织.md'); await expect(page.locator('#tabs .tab')).toHaveCount(2);
  const stored = await page.evaluate(() => Object.entries(localStorage).filter(([k]) => k.startsWith('notebase-workspace:')).map(([, v]) => JSON.parse(v)));
  expect(stored).toEqual([{ tabs: ['欢迎.md', '知识组织.md'], active: '知识组织.md' }]);
});
test('命令面板可全键盘操作，专注模式保留退出入口', async ({ page }) => {
  await page.goto('/'); await expect(page.locator('#new')).toBeEnabled(); await page.keyboard.press('Control+Shift+p');
  await expect(page.locator('#commandDialog')).toBeVisible(); await page.getByLabel('查找命令').fill('专注'); await page.keyboard.press('ArrowDown'); await page.keyboard.press('Enter');
  await expect(page.locator('html')).toHaveClass(/focus-mode/); await expect(page.locator('#focus')).toBeVisible(); await page.locator('#focus').click(); await expect(page.locator('html')).not.toHaveClass(/focus-mode/);
});

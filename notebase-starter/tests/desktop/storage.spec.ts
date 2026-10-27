import { test as base, expect, chromium, type Page, type Browser } from '@playwright/test';
import { spawn, spawnSync } from 'node:child_process';
import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { resolve } from 'node:path';

async function availablePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>(done => server.listen(0, '127.0.0.1', done));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('未取得测试端口');
  const port = address.port;
  await new Promise<void>((done, reject) => server.close(error => error ? reject(error) : done()));
  return port;
}

const test = base.extend<{ desktopPage: Page; vault: string }>({
  desktopPage: async ({}, use, testInfo) => {
    const executable = resolve(process.env.NOTEBASE_EXE ?? 'src-tauri/target/release/notebase.exe');
    await access(executable);
    const port = await availablePort();
    const app = spawn(executable, [], {
      windowsHide: true,
      stdio: 'ignore',
      env: {
        ...process.env,
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
        WEBVIEW2_USER_DATA_FOLDER: testInfo.outputPath('webview-profile'),
      },
    });
    let spawnError: Error | undefined;
    app.once('error', error => { spawnError = error; });
    let browser: Browser | undefined;
    try {
      await expect.poll(async () => {
        if (spawnError) throw spawnError;
        if (app.exitCode !== null) throw new Error(`桌面进程退出：${app.exitCode}`);
        try {
          const response = await fetch(`http://127.0.0.1:${port}/json/version`, { signal: AbortSignal.timeout(1_000) });
          return response.ok;
        } catch { return false; }
      }, { timeout: 30_000 }).toBe(true);
      browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
      const context = browser.contexts()[0];
      await expect.poll(() => context.pages().length).toBeGreaterThan(0);
      const page = context.pages()[0];
      await expect(page.locator('#mode')).toContainText('桌面模式');
      await use(page);
      await page.screenshot({ path: testInfo.outputPath('desktop.png'), fullPage: true });
    } finally {
      await browser?.close().catch(() => {});
      if (app.pid && app.exitCode === null) {
        // 只终止本测试启动的进程树，不按应用名清理用户正在运行的实例。
        spawnSync('taskkill', ['/PID', String(app.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
      }
    }
  },
  vault: async ({}, use, testInfo) => {
    const vault = testInfo.outputPath('vault');
    await mkdir(vault, { recursive: true });
    await use(vault);
  },
});

test.skip(process.platform !== 'win32', '真实桌面测试使用 Windows WebView2');

async function openVault(page: Page, vault: string) {
  await page.getByLabel('笔记库绝对路径').fill(vault);
  await page.getByRole('button', { name: '打开已有文件夹' }).click();
  await expect(page.locator('#status')).toHaveText('笔记库已打开');
}

test('真实桌面读取 BOM 文件 保存并重新打开', async ({ desktopPage: page, vault }) => {
  const target = resolve(vault, '中文.md');
  await writeFile(target, '\ufeff# 标题\r\n原文\r\n', 'utf8');
  await openVault(page, vault);
  await page.getByRole('button', { name: '中文.md', exact: true }).click();
  await expect(page.locator('#path')).toHaveText('中文.md');
  await page.locator('.cm-content').click();
  await page.keyboard.press('Control+End');
  await page.keyboard.insertText('新增内容');
  await page.getByRole('button', { name: '保存 Ctrl+S' }).click();
  await expect(page.locator('#dirty')).toHaveText('已保存');
  expect(await readFile(target, 'utf8')).toBe('# 标题\r\n原文\r\n新增内容');
  await page.reload();
  await openVault(page, vault);
  await page.getByRole('button', { name: '中文.md', exact: true }).click();
  await expect(page.locator('.cm-content')).toContainText('新增内容');
});

test('真实桌面新建保留名 自动修正且不覆盖同名文件', async ({ desktopPage: page, vault }) => {
  await writeFile(resolve(vault, '_COM1.md'), '保留内容');
  await openVault(page, vault);
  page.once('dialog', dialog => dialog.accept('COM¹'));
  await page.getByRole('button', { name: '新建笔记' }).click();
  await expect(page.locator('#path')).toHaveText('_COM1 (1).md');
  await expect(page.locator('#status')).toContainText('文件名已修正或避开同名');
  expect(await readFile(resolve(vault, '_COM1.md'), 'utf8')).toBe('保留内容');
  expect(await readFile(resolve(vault, '_COM1 (1).md'), 'utf8')).toBe('');
});

test('真实桌面非法编码与超大文件只读预览', async ({ desktopPage: page, vault }) => {
  const invalid = Buffer.from([0xff, 0xfe, 0x61]);
  const large = Buffer.alloc(10 * 1024 * 1024 + 1, 0x61);
  await writeFile(resolve(vault, 'invalid.md'), invalid);
  await writeFile(resolve(vault, 'large.md'), large);
  await openVault(page, vault);
  await page.getByRole('button', { name: 'invalid.md', exact: true }).click();
  await expect(page.locator('#readNotice')).toContainText('非法 UTF-8');
  await expect(page.locator('#save')).toBeDisabled();
  await expect(page.locator('.cm-content')).toHaveAttribute('contenteditable', 'false');
  await page.keyboard.press('Control+s');
  expect(await readFile(resolve(vault, 'invalid.md'))).toEqual(invalid);
  await page.getByRole('button', { name: 'large.md', exact: true }).click();
  await expect(page.locator('#readNotice')).toContainText('仅预览前 256 KiB');
  await expect(page.locator('#save')).toBeDisabled();
  expect(await readFile(resolve(vault, 'large.md'))).toEqual(large);
});

test('真实桌面拒绝覆盖外部修改', async ({ desktopPage: page, vault }) => {
  const target = resolve(vault, 'A.md');
  await writeFile(target, '原文');
  await openVault(page, vault);
  await page.getByRole('button', { name: 'A.md', exact: true }).click();
  await page.locator('.cm-content').click();
  await page.keyboard.press('Control+End');
  await page.keyboard.insertText('本窗口修改');
  await writeFile(target, '外部修改');
  await page.getByRole('button', { name: '保存 Ctrl+S' }).click();
  await expect(page.locator('#status')).toContainText('E_EXTERNAL_MODIFIED');
  expect(await readFile(target, 'utf8')).toBe('外部修改');
  await expect(page.locator('#dirty')).toHaveText('有未保存修改');
});

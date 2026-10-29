import { test as base, expect, chromium, type Page, type Browser } from '@playwright/test';
import { spawn, spawnSync } from 'node:child_process';
import { access, mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
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
      stdio: ['ignore', 'pipe', 'pipe'],
      env: {
        ...process.env,
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
        WEBVIEW2_USER_DATA_FOLDER: testInfo.outputPath('webview-profile'),
        NOTEBASE_DATA_DIR: testInfo.outputPath('app-data'),
      },
    });
    let diagnostics='';
    app.stdout?.on('data',data=>{diagnostics=(diagnostics+data.toString('utf8')).slice(-8000);});
    app.stderr?.on('data',data=>{diagnostics=(diagnostics+data.toString('utf8')).slice(-8000);});
    let spawnError: Error | undefined;
    app.once('error', error => { spawnError = error; });
    let browser: Browser | undefined;
    try {
      await expect.poll(async () => {
        if (spawnError) throw spawnError;
        if (app.exitCode !== null) throw new Error(`桌面进程退出：${app.exitCode} ${diagnostics}`);
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
      if(!page.isClosed()) await page.screenshot({ path: testInfo.outputPath('desktop.png'), fullPage: true });
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
  await expect(page.locator('#path')).toHaveText('_COM1 (2).md');
  await expect(page.locator('#status')).toContainText('文件名已修正或避开同名');
  expect(await readFile(resolve(vault, '_COM1.md'), 'utf8')).toBe('保留内容');
  expect(await readFile(resolve(vault, '_COM1 (2).md'), 'utf8')).toBe('');
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
  await expect(page.locator('#dirty')).toContainText('冲突暂停');
});

test('真实桌面在库外构建索引 保存后提示刷新且重开能更新索引', async ({ desktopPage: page, vault }, testInfo) => {
  const raw='---\ntitle: 索引标题\ntags: [数据库]\n---\n# 数据库测试\n中文连续搜索内容';
  await writeFile(resolve(vault,'索引.md'),raw);
  await openVault(page,vault);
  await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');
  expect(await readdir(vault)).toEqual(['索引.md']);
  expect(await readFile(resolve(vault,'索引.md'),'utf8')).toBe(raw);
  const cacheRoot=testInfo.outputPath('app-data','vaults');
  const ids=await readdir(cacheRoot);expect(ids).toHaveLength(1);
  const db=new DatabaseSync(resolve(cacheRoot,ids[0],'index.db'),{readOnly:true});
  try{
    expect(db.prepare('SELECT title FROM notes').get()?.title).toBe('索引标题');
    expect(db.prepare("SELECT count(*) AS n FROM note_fts WHERE note_fts MATCH '连续搜'").get()?.n).toBe(1);
    expect(db.prepare('SELECT tag FROM tags').get()?.tag).toBe('数据库');
    await page.getByRole('button',{name:'索引.md',exact:true}).click();
    await page.locator('.cm-content').click();await page.keyboard.press('Control+End');await page.keyboard.insertText('\n保存后的新内容');
    await page.getByRole('button',{name:'保存 Ctrl+S'}).click();
    await expect(page.locator('#dirty')).toHaveText('已保存');
    await expect(page.locator('#indexStatus')).toContainText('索引需刷新');
    await openVault(page,vault);
    await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');
    expect(db.prepare("SELECT count(*) AS n FROM note_fts WHERE note_fts MATCH '后的新'").get()?.n).toBe(1);
  }finally{db.close();}
});

test('真实桌面切换库后索引状态与数据相互隔离',async({desktopPage:page,vault},testInfo)=>{
  await writeFile(resolve(vault,'A.md'),'第一库正文');await openVault(page,vault);
  await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');
  const second=testInfo.outputPath('second-vault');await mkdir(second);
  await writeFile(resolve(second,'B.md'),'第二库正文');await writeFile(resolve(second,'C.md'),'另一篇');
  await openVault(page,second);await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 2 篇');
  expect(await readdir(testInfo.outputPath('app-data','vaults'))).toHaveLength(2);
  await openVault(page,vault);await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');
});

async function invokeCore(page:Page,command:string,payload:Record<string,unknown>={}) {
  return page.evaluate(async({command,payload})=>{
    const internal=(window as unknown as {__TAURI_INTERNALS__:{invoke:(command:string,args:unknown)=>Promise<any>}}).__TAURI_INTERNALS__;
    return internal.invoke(command,{request:{id:crypto.randomUUID(),command,payload,clientSeq:0}});
  },{command,payload});
}
test('成员二真实桌面自动保存 属性与回收站恢复',async({desktopPage:page,vault})=>{
 const target=resolve(vault,'A.md');await writeFile(target,'# A\n原文','utf8');await openVault(page,vault);
 await page.getByRole('button',{name:'A.md',exact:true}).click();await page.locator('.cm-content').click();await page.keyboard.press('Control+End');await page.keyboard.insertText('自动保存内容');
 await expect.poll(()=>readFile(target,'utf8'),{timeout:5000}).toContain('自动保存内容');await expect(page.locator('#dirty')).toHaveText('已保存');
 await page.locator('#tags').fill('课程/算法');await page.locator('#propertyTitle').fill('属性标题');await page.locator('#propertyStatus').fill('进行中');await page.locator('#propertiesSave').click();
 await expect(page.locator('#status')).toHaveText('属性已保存');expect(await readFile(target,'utf8')).toContain('课程/算法');expect(await readFile(target,'utf8')).toContain('自动保存内容');
 page.once('dialog',d=>d.accept());await page.locator('#delete').click();await expect(page.locator('#status')).toContainText('已移到回收站');
 await expect.poll(async()=>{try{await access(target);return true;}catch{return false;}}).toBe(false);
 await page.getByText('回收站',{exact:true}).click();await expect(page.locator('#trash')).toContainText('A.md');await page.locator('#trash').getByRole('button',{name:'恢复',exact:true}).click();await expect(page.locator('#path')).toHaveText('A.md');expect(await readFile(target,'utf8')).toContain('自动保存内容');
});
test('成员二真实IPC持久草稿刷新恢复 双链重命名和图数据',async({desktopPage:page,vault})=>{
 await writeFile(resolve(vault,'A.md'),'# A');await writeFile(resolve(vault,'B.md'),'[[A|显示]] and A');await openVault(page,vault);
 const note=await invokeCore(page,'note_read',{path:'A.md',clientId:'ipc-recovery'});expect(note.ok).toBe(true);
 const draft=await invokeCore(page,'note_save',{action:'draft',path:'A.md',body:'恢复的真实草稿',baseHash:note.data.contentHash,clientId:'ipc-recovery',clientSeq:1});expect(draft.ok).toBe(true);expect(await readFile(resolve(vault,'A.md'),'utf8')).toBe('# A');
 await page.reload();await openVault(page,vault);await expect(page.locator('#drafts')).toContainText('恢复的真实草稿');await page.locator('#drafts').getByRole('button',{name:'恢复原位'}).click();await expect(page.locator('.cm-content')).toContainText('恢复的真实草稿');expect(await readFile(resolve(vault,'A.md'),'utf8')).toBe('恢复的真实草稿');
 const backlinks=await invokeCore(page,'link_backlinks',{path:'A.md'});expect(backlinks.ok).toBe(true);expect(backlinks.data.items[0].sourcePath).toBe('B.md');
 const plan=await invokeCore(page,'note_rename',{path:'A.md',newName:'C',planOnly:true});expect(plan.ok).toBe(true);expect(await readFile(resolve(vault,'B.md'),'utf8')).toBe('[[A|显示]] and A');
 const renamed=await invokeCore(page,'note_rename',{path:'A.md',newName:'C',planOnly:false,planId:plan.data.plan.id});expect(renamed.ok).toBe(true);expect(await readFile(resolve(vault,'B.md'),'utf8')).toBe('[[C|显示]] and A');expect(await readFile(resolve(vault,'C.md'),'utf8')).toBe('恢复的真实草稿');
 const graph=await invokeCore(page,'link_graph',{maxNodes:500});expect(graph.ok).toBe(true);expect(graph.data.edges.some((e:{from:string;to:string})=>e.from==='B.md'&&e.to==='C.md')).toBe(true);
});

test('成员二真实窗口关闭会先冲刷未保存内容',async({desktopPage:page,vault})=>{
 const target=resolve(vault,'Close.md');await writeFile(target,'original');await openVault(page,vault);await page.getByRole('button',{name:'Close.md',exact:true}).click();await page.locator('.cm-content').click();await page.keyboard.press('Control+End');await page.keyboard.insertText(' saved on close');
 const closed=page.waitForEvent('close',{timeout:10000});
 await page.evaluate(async()=>{const internal=(window as unknown as {__TAURI_INTERNALS__:{invoke:(name:string,args:unknown)=>Promise<unknown>}}).__TAURI_INTERNALS__;await internal.invoke('plugin:window|close',{label:'main'});}).catch(error=>{if(!/closed|destroyed/i.test(String(error)))throw error;});
 await closed;expect(await readFile(target,'utf8')).toBe('original saved on close');
});

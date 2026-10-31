import { test as base, expect, chromium, type Page, type Browser } from '@playwright/test';
import { spawn, spawnSync } from 'node:child_process';
import { access, mkdir, readFile, writeFile, readdir } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

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
      cwd: dirname(executable),
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
      page.on('pageerror', error => { diagnostics += `\n${error.stack ?? error.message}`; });
      page.on('console', message => { if (message.type() === 'error') diagnostics += `\n${message.text()}`; });
      await expect(page.locator('#mode')).toContainText('桌面模式', { timeout: 30_000 }).catch(error => { throw new Error(String(error) + diagnostics); });
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

test('真实桌面已注册原生文件夹选择插件且具有打开权限',async({desktopPage:page})=>{
 await expect(page.getByRole('button',{name:'选择笔记库文件夹…',exact:true})).toBeEnabled();
 // Fail option decoding before displaying a modal. This verifies real plugin/ACL
 // wiring without pretending to automate the Windows dialog itself.
 const error=await page.evaluate(async()=>{const internal=(window as unknown as {__TAURI_INTERNALS__:{invoke:(name:string,args:unknown)=>Promise<unknown>}}).__TAURI_INTERNALS__;try{await internal.invoke('plugin:dialog|open',{options:{directory:'invalid'}});return 'unexpected success';}catch(error){return String(error);}});
 expect(error).toMatch(/invalid type.*expected.*boolean/i);
});

test('真实桌面搜索中文并打开命中笔记',async({desktopPage:page,vault})=>{
 await writeFile(resolve(vault,'搜索.md'),'# 数据结构\n跨词边界的动态规划内容');await openVault(page,vault);
 await expect(page.locator('#indexStatus')).toContainText('索引已就绪');
 await page.getByLabel('搜索笔记',{exact:true}).fill('态规划');await page.getByRole('button',{name:'搜索',exact:true}).click();
 await expect(page.locator('#searchInfo')).toContainText('1 条结果');await page.locator('#searchResults button').click();await expect(page.locator('#path')).toHaveText('搜索.md');
 await page.getByLabel('搜索笔记',{exact:true}).fill('动态');await page.getByRole('button',{name:'搜索',exact:true}).click();await expect(page.locator('#searchInfo')).toContainText('短查询扫描');
 await page.getByLabel('搜索笔记',{exact:true}).fill('不存在');await page.getByRole('button',{name:'搜索',exact:true}).click();await expect(page.locator('#searchInfo')).toContainText('0 条结果');
});

async function openVault(page: Page, vault: string) {
  await page.getByLabel('笔记库绝对路径').fill(vault);
  await page.getByRole('button', { name: '按路径打开', exact: true }).click();
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

test('真实桌面在库外构建索引 保存后自动更新并可检索', async ({ desktopPage: page, vault }, testInfo) => {
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
    await expect.poll(()=>db.prepare("SELECT count(*) AS n FROM note_fts WHERE note_fts MATCH '后的新'").get()?.n).toBe(1);
    await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');
    expect(db.prepare("SELECT count(*) AS n FROM note_fts WHERE note_fts MATCH '后的新'").get()?.n).toBe(1);
    await page.getByLabel('搜索笔记',{exact:true}).fill('后的新');await page.getByRole('button',{name:'搜索',exact:true}).click();await expect(page.locator('#searchInfo')).toContainText('1 条结果');
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

test('真实桌面外部文件变化自动进入搜索和文件列表',async({desktopPage:page,vault})=>{
 await openVault(page,vault);await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 0 篇');
 await writeFile(resolve(vault,'外部.md'),'外部创建可检索');
 await expect(page.locator('#notes')).toContainText('外部.md');await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');
 await page.getByLabel('搜索笔记',{exact:true}).fill('外部创建');await page.getByRole('button',{name:'搜索',exact:true}).click();await expect(page.locator('#searchInfo')).toContainText('1 条结果');
 await page.locator('#notes button').click();await page.locator('.cm-content').click();await page.keyboard.press('Control+End');await page.keyboard.insertText(' 本地未保存');
 await writeFile(resolve(vault,'外部.md'),'外部第二次改写');
 await expect(page.locator('#externalNotice')).toContainText('磁盘内容已修改');await expect(page.locator('.cm-content')).toContainText('本地未保存');await expect(page.locator('#dirty')).toContainText('冲突暂停');expect(await readFile(resolve(vault,'外部.md'),'utf8')).toBe('外部第二次改写');
});

test('真实桌面后台重建保留快照索引并恢复搜索',async({desktopPage:page,vault},testInfo)=>{
 await writeFile(resolve(vault,'A.md'),'后台重建可搜索');await openVault(page,vault);await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');
 const folder=testInfo.outputPath('app-data','vaults');const ids=await readdir(folder);const db=new DatabaseSync(resolve(folder,ids[0],'index.db'));
 try{
  db.prepare("INSERT INTO snapshot_index VALUES('A.md','v1','snapshots/a',1,1)").run();
  const before=db.prepare("SELECT value FROM meta WHERE key='last_full_build_ms'").get()?.value;
  await page.getByRole('button',{name:'后台重建索引',exact:true}).click();await expect(page.locator('#status')).toContainText('已启动后台重建：rebuild-');
  await expect.poll(()=>db.prepare("SELECT value FROM meta WHERE key='last_full_build_ms'").get()?.value).not.toBe(before);
  await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');expect(db.prepare('SELECT count(*) AS n FROM snapshot_index').get()?.n).toBe(1);
  await page.getByLabel('搜索笔记',{exact:true}).fill('后台重建');await page.getByRole('button',{name:'搜索',exact:true}).click();await expect(page.locator('#searchInfo')).toContainText('1 条结果');
 }finally{db.close();}
});

test('真实桌面迁移校验文件并保留原目录',async({desktopPage:page,vault},testInfo)=>{
 await writeFile(resolve(vault,'迁移.md'),'迁移后的检索内容');await mkdir(resolve(vault,'attachments'));await writeFile(resolve(vault,'attachments','image.bin'),new Uint8Array([0,255,1]));await openVault(page,vault);await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');
 const target=testInfo.outputPath('migrated-vault');page.once('dialog',dialog=>dialog.accept(target));await page.getByRole('button',{name:'迁移笔记库目录',exact:true}).click();
 await expect(page.locator('#status')).toContainText('迁移完成，已切换到新目录');
 expect(await readFile(resolve(vault,'迁移.md'),'utf8')).toBe('迁移后的检索内容');expect(await readFile(resolve(target,'迁移.md'),'utf8')).toBe('迁移后的检索内容');expect(await readFile(resolve(target,'attachments','image.bin'))).toEqual(await readFile(resolve(vault,'attachments','image.bin')));
 await expect(page.locator('#vaultPath')).toHaveValue(/migrated-vault$/);await expect(page.locator('#indexStatus')).toContainText('索引已就绪 · 1 篇');
 const settings=JSON.parse(await readFile(testInfo.outputPath('app-data','settings.json'),'utf8'));expect(settings.vaultPath).toMatch(/migrated-vault$/);
 await page.getByLabel('搜索笔记',{exact:true}).fill('迁移后的');await page.getByRole('button',{name:'搜索',exact:true}).click();await expect(page.locator('#searchInfo')).toContainText('1 条结果');
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

test('成员四自动保存后无需重开库即可检索，日志不包含笔记正文',async({desktopPage:page,vault},testInfo)=>{
 const target=resolve(vault,'Flow.md');await writeFile(target,'initial');await openVault(page,vault);await page.getByRole('button',{name:'Flow.md',exact:true}).click();
 await page.locator('.cm-content').click();await page.keyboard.press('Control+End');await page.keyboard.insertText(' 成员四秘密正文');
 await expect.poll(()=>readFile(target,'utf8')).toContain('成员四秘密正文');await expect(page.locator('#dirty')).toHaveText('已保存');await expect(page.locator('#indexStatus')).toContainText('索引已就绪');
 await page.locator('#searchQuery').fill('成员四秘密');await page.locator('#searchButton').click();await expect(page.locator('#searchInfo')).toContainText('1 条结果');
 const folder=testInfo.outputPath('app-data','logs');const logs=(await Promise.all((await readdir(folder)).map(name=>readFile(resolve(folder,name),'utf8')))).join('\n');
 expect(logs).toContain('note_save');expect(logs).not.toContain('秘密正文');expect(logs).not.toContain(vault);
});

test('成员四桌面设置落盘，冲突快捷键保持旧配置',async({desktopPage:page,vault},testInfo)=>{
 await openVault(page,vault);await page.locator('#settings').click();await page.locator('#theme').selectOption('dark');await page.locator('#fontSize').fill('17');await page.locator('#settingsSave').click();
 await expect(page.locator('#settingsDialog')).toBeHidden();const file=testInfo.outputPath('app-data','settings.json');const original=await readFile(file,'utf8');expect(JSON.parse(original).theme).toBe('dark');
 await page.reload();await expect(page.locator('html')).toHaveAttribute('data-theme','dark');await page.locator('#settings').click();await page.locator('#key-new').fill('Ctrl+S');await page.locator('#settingsSave').click();await expect(page.locator('#settingsError')).toContainText('E_KEYMAP_CONFLICT');expect(await readFile(file,'utf8')).toBe(original);
});

test('成员四第二窗口读写共享库且禁止切库，两个窗口内容不互相覆盖',async({desktopPage:page,vault},testInfo)=>{
 await writeFile(resolve(vault,'Shared.md'),'shared');await openVault(page,vault);await page.getByRole('button',{name:'Shared.md',exact:true}).click();
 const created=page.context().waitForEvent('page');await page.locator('#newWindow').click();const second=await created;await second.waitForLoadState();
 await expect(second.locator('#notes')).toContainText('Shared.md');await second.getByRole('button',{name:'Shared.md',exact:true}).click();
 const before=await invokeCore(page,'note_read',{path:'Shared.md',clientId:'window-one'});const other=await invokeCore(second,'note_read',{path:'Shared.md',clientId:'window-two'});
 expect((await invokeCore(page,'note_save',{path:'Shared.md',body:'first window',baseHash:before.data.contentHash,clientId:'window-one',clientSeq:1})).ok).toBe(true);
 expect((await invokeCore(second,'note_save',{path:'Shared.md',body:'second overwrite',baseHash:other.data.contentHash,clientId:'window-two',clientSeq:1})).ok).toBe(false);expect(await readFile(resolve(vault,'Shared.md'),'utf8')).toBe('first window');
 const target=testInfo.outputPath('another');await mkdir(target);const blocked=await invokeCore(second,'vault_open',{path:target});expect(blocked.ok).toBe(false);expect(blocked.error.code).toBe('E_CANCELLED');
 const restoreBlocked=await invokeCore(second,'backup_restore',{filePath:testInfo.outputPath('not-used.zip'),mode:'original'});expect(restoreBlocked.ok).toBe(false);expect(restoreBlocked.error.code).toBe('E_CANCELLED');expect(await readFile(resolve(vault,'Shared.md'),'utf8')).toBe('first window');
});

test('全员界面原位恢复不会将恢复前编辑内容再次保存回文件',async({desktopPage:page,vault},testInfo)=>{
 const path=resolve(vault,'Restore.md');await writeFile(path,'原位恢复基线');await openVault(page,vault);await page.getByRole('button',{name:'Restore.md',exact:true}).click();
 const backup=await invokeCore(page,'backup_create',{});expect(backup.ok).toBe(true);
 await page.locator('.cm-content').click();await page.keyboard.press('Control+End');await page.keyboard.insertText(' 恢复前追加');await page.keyboard.press('Control+s');
 await expect.poll(()=>readFile(path,'utf8')).toContain('恢复前追加');await expect(page.locator('#dirty')).toHaveText('已保存');
 await page.locator('#restoreFile').fill(backup.data.filePath);await page.locator('#restoreMode').selectOption('original');page.once('dialog',dialog=>dialog.accept());await page.locator('#restoreRun').click();
 await expect.poll(()=>readFile(path,'utf8')).toBe('原位恢复基线');await expect(page.locator('#status')).toHaveText('笔记库已打开');
 await page.getByRole('button',{name:'Restore.md',exact:true}).click();await expect(page.locator('.cm-content')).toHaveText('原位恢复基线');
 const retained=(await readdir(testInfo.outputPath())).filter(name=>name.startsWith('notebase-before-restore-'));expect(retained).toHaveLength(1);expect(await readFile(resolve(testInfo.outputPath(),retained[0],'Restore.md'),'utf8')).toContain('恢复前追加');
});

test('成员三真实IPC导出 快照 备份校验与新位置恢复',async({desktopPage:page,vault},testInfo)=>{
 const body='# 真实成员三\n备份恢复内容';await writeFile(resolve(vault,'A.md'),body);await openVault(page,vault);
 const preview=await invokeCore(page,'export_note',{path:'A.md',preview:true,format:'html',body});expect(preview.ok).toBe(true);expect(preview.data.html).toContain('真实成员三');
 const html=testInfo.outputPath('export.html');const exported=await invokeCore(page,'export_note',{path:'A.md',format:'html',targetPath:html});expect(exported.ok).toBe(true);expect(await readFile(html,'utf8')).toContain('真实成员三');
 const pdf=testInfo.outputPath('export.pdf');const printed=await invokeCore(page,'export_note',{path:'A.md',format:'pdf',targetPath:pdf});expect(printed.ok).toBe(true);expect((await readFile(pdf)).subarray(0,5).toString()).toBe('%PDF-');
 const snapshot=await invokeCore(page,'backup_create',{scope:'note',path:'A.md'});expect(snapshot.ok).toBe(true);
 const versions=await invokeCore(page,'note_list',{scope:'versions',path:'A.md'});expect(versions.ok).toBe(true);expect(versions.data.items.length).toBeGreaterThan(0);
 const backup=await invokeCore(page,'backup_create',{});expect(backup.ok).toBe(true);
 const verified=await invokeCore(page,'backup_restore',{filePath:backup.data.filePath,verifyOnly:true});expect(verified.ok).toBe(true);expect(verified.data.verified).toBe(true);
 const target=testInfo.outputPath('restored');const restored=await invokeCore(page,'backup_restore',{filePath:backup.data.filePath,mode:'new',targetPath:target});expect(restored.ok).toBe(true);expect(await readFile(resolve(target,'A.md'),'utf8')).toBe(body);expect(await readFile(resolve(vault,'A.md'),'utf8')).toBe(body);
});

test('真实exe重命名保留历史且旧路径新建不串版本',async({desktopPage:page,vault})=>{
 await writeFile(resolve(vault,'A.md'),'original history');await openVault(page,vault);await page.getByRole('button',{name:'A.md',exact:true}).click();
 const snap=await invokeCore(page,'backup_create',{scope:'note',path:'A.md',description:'rename history'});expect(snap.ok).toBe(true);
 const plan=await invokeCore(page,'note_rename',{path:'A.md',newName:'B',planOnly:true});expect(plan.ok).toBe(true);
 const renamed=await invokeCore(page,'note_rename',{path:'A.md',newName:'B',planOnly:false,planId:plan.data.plan.id});expect(renamed.ok).toBe(true);
 await page.locator('#refresh').click();await page.getByRole('button',{name:'B.md',exact:true}).click();await page.locator('#versionsRefresh').click();await expect(page.locator('#versions')).toContainText('rename history');
 const body=await invokeCore(page,'note_read',{path:'B.md',versionId:snap.data.versionId});expect(body.data.body).toBe('original history');
 expect((await invokeCore(page,'note_create',{title:'A',body:'new owner'})).ok).toBe(true);expect((await invokeCore(page,'backup_create',{scope:'note',path:'A.md'})).ok).toBe(true);
 const versions=await invokeCore(page,'note_list',{scope:'versions',path:'A.md'});expect(versions.data.items.some((v:{versionId:string})=>v.versionId===snap.data.versionId)).toBe(false);
 expect(await readFile(resolve(vault,'B.md'),'utf8')).toBe('original history');
});

test('真实exe版本保留设置可关闭且保存结果不伪报快照',async({desktopPage:page,vault})=>{
 await writeFile(resolve(vault,'A.md'),'before');await openVault(page,vault);await page.getByRole('button',{name:'A.md',exact:true}).click();
 await page.locator('#versionRetentionDays').fill('0');await page.locator('#backupSettings').click();await expect(page.locator('#backupResult')).toContainText('版本历史已关闭');
 await expect(page.locator('#versions')).toContainText('版本历史已关闭');
 const config=await invokeCore(page,'settings_get',{keys:['backup']});expect(config.data.values.backup.versionRetentionDays).toBe(0);
 const note=await invokeCore(page,'note_read',{path:'A.md'});const saved=await invokeCore(page,'note_save',{path:'A.md',baseHash:note.data.contentHash,body:'saved without snapshot'});expect(saved.ok).toBe(true);expect(saved.data.snapshotCreated).toBe(false);expect(await readFile(resolve(vault,'A.md'),'utf8')).toBe('saved without snapshot');
 await page.locator('#versionRetentionDays').fill('5');await page.locator('#backupSettings').click();await expect(page.locator('#backupResult')).toContainText('版本保留 5 天');
 expect((await invokeCore(page,'settings_get',{keys:['backup']})).data.values.backup.versionRetentionDays).toBe(5);
});

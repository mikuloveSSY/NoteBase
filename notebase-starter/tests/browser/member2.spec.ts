import { test, expect, type Page } from '@playwright/test';
const browserErrors = new WeakMap<Page, string[]>();
test.beforeEach(({ page }) => { const errors: string[] = []; browserErrors.set(page, errors); page.on('pageerror', error => errors.push(error.message)); });
test.afterEach(({ page }) => { expect(browserErrors.get(page)).toEqual([]); });

async function reliableStorage(page: Page, initialDraft = false) {
  await page.route('**/src/bridge.ts*', route => route.fulfill({ contentType: 'application/javascript', body:
    'export const desktopMode=false; const notes=new Map([["a.md","起始"],["b.md","第二篇"]]);' +
    'const state=window.__member2={writes:[],drafts:[],delay:false,fail:false,release:null,external:null};' +
    'const note=(path,body)=>({path,title:path.slice(0,-3),body,contentHash:body,frontmatter:{},frontmatterError:null,mtimeMs:0,sizeBytes:body.length,encoding:"utf-8",lineEnding:"lf",readOnly:false,links:[{target:"Missing",label:"Missing",line:1,col:1,start:0,end:0}]});' +
    'export async function call(command,p={}){' +
    'if(command==="vault_open")return {}; if(command==="note_list"){if(p.scope==="drafts")return {items:' + (initialDraft ? '[{id:"draft-1",path:"a.md",body:"恢复草稿",baseHash:"起始",clientId:"old",clientSeq:2,savedAtMs:1}]' : '[]') + '}; if(p.scope)return {items:[]}; return {items:[...notes].map(([path])=>({path,title:path,mtimeMs:0,hasAttachment:false})),nextCursor:null};}' +
    'if(command==="note_read"){const body=state.external?.path===p.path?state.external.body:notes.get(p.path);if(body===undefined)throw {code:"E_NOT_FOUND",message:"外部删除"};return note(p.path,body);}' +
    'if(command==="note_save"){if(p.action==="draft"){state.drafts.push({...p});return {};}' +
    'if(p.action==="conflict"){state.fail=false;state.external=null;notes.set(p.path,p.choice==="disk"?"磁盘内容":p.body);return note(p.path,notes.get(p.path));}' +
    'state.writes.push({...p});if(state.delay)await new Promise(r=>state.release=r);if(state.fail)throw {code:"E_EXTERNAL_MODIFIED",message:"外部修改",detail:{diskHash:"disk"}};notes.set(p.path,p.body);return {path:p.path,contentHash:p.body,mtimeMs:1,bytesWritten:p.body.length,warnings:[]};}' +
    'if(command==="note_restore"){notes.set("a.md","恢复草稿");return {path:"a.md",recovered:true,copied:false};}' +
    'if(command==="link_backlinks"){if(p.resolveTarget)return {targetPath:null,candidates:[],ambiguous:false};return {items:[]};}' +
    'if(command==="link_mentions")return {items:[]};if(command==="link_graph")return {nodes:[{path:"a.md"},{path:"b.md"}],edges:[{from:"a.md",to:"b.md"}]};' +
    'throw new Error("unsupported "+command);}'
  }));
}

test('异步保存不冻结输入，旧回包不会把后续输入标为已保存', async ({ page }) => {
  await reliableStorage(page); await page.goto('/'); await page.getByRole('button', { name: 'a.md', exact: true }).click();
  await page.evaluate('window.__member2.delay=true');
  const editor = page.locator('.cm-content'); await editor.click(); await page.keyboard.press('Control+End'); await page.keyboard.type(' first'); await page.locator('#save').click();
  await expect(editor).toHaveAttribute('contenteditable', 'true'); await editor.click(); await page.keyboard.type(' second');
  await page.evaluate('window.__member2.delay=false;window.__member2.release()');
  await expect(page.locator('#dirty')).toHaveText('有未保存修改'); await expect(editor).toContainText('second');
  await page.locator('#save').click(); await expect(page.locator('#dirty')).toHaveText('已保存');
  expect(await page.evaluate('window.__member2.writes.map(x=>x.body)')).toEqual(['起始 first', '起始 first second']);
});

test('冲突提供三个显式选择，关闭弹窗后仍暂停且保留本地输入', async ({ page }) => {
  await reliableStorage(page); await page.goto('/'); await page.getByRole('button', { name: 'a.md', exact: true }).click();
  await page.evaluate('window.__member2.fail=true'); await page.locator('.cm-content').click(); await page.keyboard.type('local'); await page.locator('#save').click();
  const dialog = page.locator('#conflictDialog'); await expect(dialog).toBeVisible();
  for (const label of ['保留本地并覆盖', '使用磁盘内容', '另存冲突副本']) await expect(dialog.getByRole('button', { name: label, exact: true })).toBeVisible();
  await dialog.getByRole('button', { name: '稍后处理（保持暂停）' }).click(); await expect(dialog).not.toBeVisible();
  await expect(page.locator('#dirty')).toContainText('暂停'); await page.locator('.cm-content').click(); await page.keyboard.type('kept');
  await page.getByRole('button', { name: 'b.md', exact: true }).click(); await expect(page.locator('#path')).toHaveText('a.md');
  await page.locator('#showConflict').click(); await dialog.getByRole('button', { name: '保留本地并覆盖' }).click();
  await expect(page.locator('#dirty')).toHaveText('已保存'); await expect(page.locator('.cm-content')).toContainText('kept');
});

test('切换轻量标签前flush，并从启动恢复列表显式恢复草稿', async ({ page }) => {
  await reliableStorage(page, true); await page.goto('/'); await expect(page.locator('#drafts')).toContainText('a.md');
  await page.locator('#drafts').getByRole('button', { name: '恢复原位' }).click(); await expect(page.locator('.cm-content')).toContainText('恢复草稿');
  await page.locator('.cm-content').click(); await page.keyboard.type('changed'); await page.getByRole('button', { name: 'b.md', exact: true }).click();
  await expect(page.locator('#path')).toHaveText('b.md'); expect(await page.evaluate('window.__member2.writes.length')).toBe(1);
  await page.locator('#tabs').getByRole('button', { name: '打开 a.md', exact: true }).click(); await expect(page.locator('.cm-content')).toContainText('changed');
});

test('周期检查clean自动重读，dirty保留本地并暂停', async ({ page }) => {
  await page.clock.install({ time: new Date('2026-01-01T00:00:00Z') }); await page.clock.pauseAt(new Date('2026-01-01T00:00:01Z'));
  await reliableStorage(page); await page.goto('/'); await page.getByRole('button', { name: 'a.md', exact: true }).click();
  await page.evaluate('window.__member2.external={path:"a.md",body:"磁盘新内容"}'); await page.clock.runFor(2200);
  await expect(page.locator('.cm-content')).toHaveText('磁盘新内容');
  await page.locator('.cm-content').click(); await page.keyboard.type('local'); await page.evaluate('window.__member2.external={path:"a.md",body:"再次修改"}'); await page.clock.runFor(1800);
  await expect(page.locator('#dirty')).toContainText('暂停'); await expect(page.locator('.cm-content')).toContainText('local');
});

test('demo支持模板、属性、rename计划取消零写、回收站和二次确认删除', async ({ page }) => {
  await page.goto('/'); await page.getByText('新建目录与模板', { exact: true }).click(); await expect(page.locator('#template')).toBeEnabled(); await page.locator('#template').selectOption('templates/每日记录.md'); await page.locator('#newDir').fill('日记');
  page.once('dialog', d => d.accept('今日')); await page.locator('#new').click(); await expect(page.locator('#path')).toHaveText('日记/今日.md'); await expect(page.locator('.cm-content')).toContainText('# 今日');
  await page.locator('#propertyTitle').fill('今日总结'); await page.locator('#propertyStatus').fill('进行中'); await page.locator('#created').fill('2026-01-01T09:00:00+08:00'); await page.locator('#updated').fill('2026-01-01T11:00:00Z');
  await page.locator('#tags').fill('研究/软件, 记录'); await page.locator('#aliases').fill('今天'); await page.locator('#favorite').check(); await page.locator('#pinned').check(); await page.locator('#propertiesSave').click();
  await expect(page.locator('.cm-content')).toContainText('研究/软件'); await expect(page.locator('#favorite')).toBeChecked();
  await expect(page.locator('#title')).toHaveText('今日总结'); await expect(page.locator('.cm-content')).toContainText('status: "进行中"'); await expect(page.locator('.cm-content')).toContainText('created: "2026-01-01T09:00:00+08:00"');
  await page.locator('#created').fill(''); await page.locator('#updated').fill(''); await page.locator('#propertiesSave').click(); await expect(page.locator('.cm-content')).not.toContainText('created:'); await expect(page.locator('.cm-content')).not.toContainText('updated:');
  page.once('dialog', d => d.accept('改名')); await page.locator('#rename').click(); await expect(page.locator('#renameDialog')).toContainText('日记/改名.md'); await page.locator('#renameCancel').click(); await expect(page.locator('#path')).toHaveText('日记/今日.md');
  page.once('dialog', d => d.accept('改名')); await page.locator('#rename').click(); await page.locator('#renameConfirm').click(); await expect(page.locator('#path')).toHaveText('日记/改名.md');
  page.once('dialog', d => d.accept()); await page.locator('#delete').click(); await expect(page.locator('#trash')).toContainText('日记/改名.md');
  await page.getByText('回收站', { exact: true }).click(); await page.locator('#trash').getByRole('button', { name: '恢复', exact: true }).click(); await expect(page.locator('#path')).toHaveText('日记/改名.md');
  page.once('dialog', d => d.accept()); await page.locator('#delete').click(); let confirmations = 0; page.on('dialog', async d => { confirmations++; await d.accept(); });
  await page.locator('#trash').getByRole('button', { name: '彻底删除' }).click(); await expect(page.locator('#trash')).not.toContainText('日记/改名.md'); expect(confirmations).toBe(2);
});

test('粘贴与拖放File插入光标并自动保存，链接面板与SVG图可导航缩放', async ({ page }, testInfo) => {
  await page.goto('/'); await page.getByRole('button', { name: '欢迎.md', exact: true }).click(); await page.locator('.cm-content').click(); await page.keyboard.press('Control+End');
  await page.locator('.cm-content').evaluate(el => { const data = new DataTransfer(); data.items.add(new File(['image'], '测试.png', { type: 'image/png' })); el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true })); });
  await expect(page.locator('.cm-content')).toContainText('![测试.png]'); await expect(page.locator('#dirty')).toHaveText('已保存');
  await page.locator('.cm-content').evaluate(el => { const data = new DataTransfer(); data.items.add(new File(['attachment'], '记录.txt', { type: 'text/plain' })); const rect = el.getBoundingClientRect(); el.dispatchEvent(new DragEvent('drop', { dataTransfer: data, clientX: rect.x + 10, clientY: rect.y + 10, bubbles: true, cancelable: true })); });
  await expect(page.locator('.cm-content')).toContainText('[记录.txt]'); await expect(page.locator('#dirty')).toHaveText('已保存');
  await page.getByText('关系图谱', { exact: true }).click(); await page.locator('#graphRefresh').click(); await expect(page.locator('#graph svg')).toBeVisible(); const before = await page.locator('#graph svg').getAttribute('viewBox'); await page.locator('#graphZoomIn').click(); expect(await page.locator('#graph svg').getAttribute('viewBox')).not.toBe(before);
  await page.locator('#linksRefresh').click(); await expect(page.locator('#outgoing')).toContainText('知识组织'); await page.locator('#outgoing').getByRole('button', { name: /知识组织/ }).first().click(); await expect(page.locator('#path')).toHaveText('知识组织.md');
  await page.locator('#mentions').getByRole('button', { name: '转为双链' }).first().click(); await expect(page.locator('#status')).toContainText('已转为双链');
  await page.screenshot({ path: testInfo.outputPath('member2-interface.png'), fullPage: true });
});

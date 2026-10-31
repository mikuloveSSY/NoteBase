import { EditorView, basicSetup } from 'codemirror';
import { markdown } from '@codemirror/lang-markdown';
import { Compartment, EditorState } from '@codemirror/state';
import { call, desktopMode } from './bridge';
import { EditorSession, replaceUtf8Range, type SessionState } from './editor-session';
import type { NoteContent, NoteEntry, SaveResult, IndexStatus, DraftEntry, TrashEntry, RenamePlan, Backlink, Mention, LinkResolution, GraphData, AttachmentResult, Command, SearchPage, SearchHit, IndexRebuildResult, VaultSettings } from './contracts';
import { transportEventName } from './contracts';
import { version } from '../package.json';
import { mountShell } from './shell-ui';
import { restoreWorkspace, type Preferences } from './preferences';
import './style.css';
import './shell.css';
import './member3.css';
import { mountDataTools } from './member3';
import { chooseVaultFolder } from './vault-dialog';

document.querySelector<HTMLDivElement>('#app')!.innerHTML = [
  '<aside><div class="brand"><b>N</b><div>NoteBase<small>本地优先 · Markdown</small></div></div>',
  '<button id="open" class="primary">选择笔记库文件夹…</button><label class="field">笔记库路径（也可手动输入）<input id="vaultPath" placeholder="选择文件夹后自动填入" aria-label="笔记库绝对路径"></label><button id="openPath">按路径打开</button>',
  '<label class="field">搜索笔记<input id="searchQuery" aria-label="搜索笔记" placeholder="关键词、title:标题、tag:标签"></label><div class="compact-actions"><button id="searchButton">搜索</button><button id="rebuild">后台重建索引</button></div><details><summary>搜索筛选</summary><label class="field">路径前缀<input id="searchPath"></label><label class="field">标签<input id="searchTag"></label><label class="field">排序<select id="searchSort"><option value="relevance">相关性</option><option value="mtime">最近修改</option></select></label></details><div id="searchInfo" role="status" aria-live="polite"></div><nav id="searchResults" aria-label="搜索结果"></nav><button id="searchMore" hidden>更多搜索结果</button><button id="migrate">迁移笔记库目录</button><p id="indexWarning" role="alert" hidden></p>',
  '<div class="section-title">笔记 <button id="new" aria-label="新建笔记">＋ 新建</button></div>',
  '<details class="new-options"><summary>新建目录与模板</summary><label class="field">目录<input id="newDir" placeholder="留空为根目录"></label><label class="field">模板<select id="template"><option value="">空白笔记</option></select></label><small id="templateStatus"></small></details>',
  '<label class="check"><input type="checkbox" id="favoriteFilter">仅收藏</label><label class="check"><input type="checkbox" id="groupTree" checked>按目录分组</label>',
  '<nav id="notes" aria-label="笔记列表"></nav><button id="more" hidden>加载更多笔记</button><button id="refresh">刷新文件列表</button>',
  '<div class="sidebar-foot">文件是唯一数据源<br>主干版本 ' + version + '</div></aside>',
  '<main><header><div><span class="eyebrow">工作空间</span><h1 id="title">选择或新建一篇笔记</h1><div id="path" class="muted">从左侧打开已有的 Markdown 文件夹</div></div>',
  '<div class="actions"><span id="dirty">未打开</span><button id="save" class="primary" disabled>保存 Ctrl+S</button></div></header>',
  '<div id="mode" class="notice"></div><div id="readNotice" class="notice" role="status" hidden></div>',
  '<nav id="tabs" class="tabs" aria-label="已打开的笔记"></nav><div class="member2-tools"><button id="rename" disabled>重命名</button><button id="delete" disabled>移到回收站</button><label class="file-button">插入附件<input type="file" id="attachment" multiple></label><button id="showConflict" hidden>处理冲突</button></div>',
  '<div id="externalNotice" class="notice" role="status" hidden></div><div class="editor-bar"><span>Markdown 源文</span><span>500ms 草稿 · 2s 停输入 · 10s 强制保存</span></div><section id="editor" aria-label="Markdown 编辑器"></section>',
  '<footer><span id="status" role="status" aria-live="polite">准备就绪</span><span id="count">0 字符</span></footer></main>',
  '<aside class="right"><h2>索引状态</h2><p id="indexStatus" role="status">打开笔记库后建立索引</p>',
  '<details open><summary>笔记属性</summary><form id="properties"><label class="field">标题<input id="propertyTitle" placeholder="留空移除标题属性"></label><label class="field">状态<input id="propertyStatus" placeholder="例如 进行中"></label><label class="field">创建时间（含时区）<input id="created" placeholder="2026-01-01T09:00:00+08:00"></label><label class="field">更新时间（含时区）<input id="updated" placeholder="2026-01-01T01:00:00Z"></label><small>时间使用带时区的 ISO 8601；留空移除字段。</small><label class="field">标签（逗号分隔）<input id="tags"></label><label class="field">别名（逗号分隔）<input id="aliases"></label><label class="check"><input type="checkbox" id="favorite">收藏</label><label class="check"><input type="checkbox" id="pinned">置顶</label><button id="propertiesSave" disabled>保存属性</button><small id="propertiesStatus"></small></form></details>',
  '<details open><summary>链接与引用</summary><button id="linksRefresh">刷新链接</button><h3>跳转到</h3><div id="outgoing" class="panel-list">打开笔记后查看</div><h3>反向链接</h3><div id="backlinks" class="panel-list"></div><h3>未链接提及</h3><div id="mentions" class="panel-list"></div></details>',
  '<details><summary>关系图谱</summary><label class="field">目录范围<input id="graphPrefix" placeholder="全库"></label><label class="check"><input type="checkbox" id="graphLocal">当前笔记邻域</label><div class="compact-actions"><button id="graphRefresh">加载图谱</button><button id="graphZoomIn" aria-label="放大图谱">＋</button><button id="graphZoomOut" aria-label="缩小图谱">－</button></div><div id="graph" class="panel-list"></div></details>',
  '<details open><summary>启动草稿恢复</summary><p class="muted">恢复时若磁盘已变化，将保留为副本。</p><button id="draftsRefresh">刷新草稿</button><div id="drafts" class="panel-list">尚未打开库</div></details>',
  '<details><summary>回收站</summary><button id="trashRefresh">刷新回收站</button><div id="trash" class="panel-list"></div></details>',
  '<details><summary>未完成重命名</summary><button id="renamesRefresh">刷新任务</button><div id="renames" class="panel-list"></div></details>',
  '<p class="muted">笔记以 Markdown 文件保存在本地；预览、导出、版本历史和备份可在上方面板使用。</p></aside>',
  '<dialog id="conflictDialog" aria-labelledby="conflictTitle"><h2 id="conflictTitle">外部修改冲突</h2><p id="conflictMessage"></p><p>自动保存已暂停。本地文本保留，请明确选择一种处理方式。</p><div class="dialog-actions"><button data-choice="local">保留本地并覆盖</button><button data-choice="disk">使用磁盘内容</button><button data-choice="copy">另存冲突副本</button></div><button id="conflictLater">稍后处理（保持暂停）</button></dialog>',
  '<dialog id="renameDialog" aria-labelledby="renameTitle"><h2 id="renameTitle">确认重命名与链接更新</h2><div id="renameDetails"></div><div class="dialog-actions"><button id="renameConfirm" class="primary">确认执行</button><button id="renameCancel">取消</button></div></dialog>',
  '<dialog id="targetDialog" aria-labelledby="targetTitle"><h2 id="targetTitle">选择链接目标</h2><div id="targetCandidates"></div><button id="targetClose">关闭</button></dialog>',
].join('');
const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
let active: NoteContent | null = null, busy = false, opened = false, loading = false, switching = false, bootReady = false;
let nextCursor: string | null = null, entries: NoteEntry[] = [];
let vaultGeneration = 0, externalTreeDirty = false;
let searchCursor: string | null = null, searchText = '';
const staleVault = new Error('笔记库已切换，忽略旧请求结果。');
const tabs = new Set<string>();
let lastSave: SaveResult | null = null, lastError: unknown = null, conflictKey = '', lastFrontmatter: unknown = null;
let pendingRename: { plan: RenamePlan; path: string; newName: string } | null = null;
const editable = new Compartment();
const editorPreferences = new Compartment();
let vaultIdentity = '', windowLabel = 'main', restoringWorkspace = false;
const view = new EditorView({ parent: $('editor'), extensions: [basicSetup, markdown(), editorPreferences.of([]), editable.of([EditorView.editable.of(false), EditorState.readOnly.of(true)]), EditorView.updateListener.of(u => { if (u.docChanged && !loading) session.edit(u.state.doc.toString()); })] });
const session = new EditorSession({
  save: s => vaultCall<SaveResult>('note_save', { path: s.path, body: s.body, baseHash: s.baseHash, expectedMtimeMs: s.expectedMtimeMs }),
  draft: s => vaultCall('note_save', { action: 'draft', path: s.path, body: s.body, baseHash: s.baseHash }),
  probe: path => vaultCall<NoteContent>('note_read', { path, probe: true }),
  reload: path => vaultCall<NoteContent>('note_read', { path, includeRaw: true }),
  resolve: async s => {
    const result = await vaultCall<{ path?: string; body?: string }>('note_save', { action: 'conflict', path: s.path, choice: s.choice, diskHash: s.diskHash, body: s.body });
    return vaultCall<NoteContent>('note_read', { path: result.path ?? s.path, includeRaw: true });
  },
  onChange: sync,
});
function message(error: unknown) { return error instanceof Error ? error.message : String((error as { message?: string })?.message ?? error); }
function status(text: string, error = false) { $('status').textContent = text; $('status').classList.toggle('error', error); }
function button(text: string, action: () => void) { const b = document.createElement('button'); b.textContent = text; b.addEventListener('click', action); return b; }
function row(text: string) { const el = document.createElement('div'); el.className = 'panel-row'; const label = document.createElement('div'); label.textContent = text; el.append(label); return el; }
function update() {
  const s = session.state;
  $('dirty').textContent = active ? active.readOnly ? '只读' : s.paused ? '冲突暂停 · 本地保留' : s.dirty ? '有未保存修改' : '已保存' : '未打开';
  $<HTMLButtonElement>('save').disabled = !active || !s.dirty || s.saving || s.paused || active.readOnly || switching;
  for (const id of ['new', 'more', 'refresh']) $<HTMLButtonElement>(id).disabled = !opened || busy;
  $<HTMLButtonElement>('open').disabled = busy || !bootReady;
  $<HTMLButtonElement>('openPath').disabled = busy || !bootReady || !desktopMode;
  $<HTMLInputElement>('vaultPath').disabled = busy || !desktopMode || !bootReady;
  for (const id of ['rebuild', 'migrate', 'searchButton', 'searchMore']) $<HTMLButtonElement>(id).disabled = busy || !opened || !desktopMode;
  $<HTMLInputElement>('searchQuery').disabled = !opened || busy || !desktopMode;
  for (const id of ['rename', 'delete', 'propertiesSave']) $<HTMLButtonElement>(id).disabled = !active || busy || s.saving || s.paused || active.readOnly || (id === 'propertiesSave' && !!active.frontmatterError);
  $<HTMLInputElement>('attachment').disabled = !active || active.readOnly || switching;
  $('showConflict').hidden = !s.conflict;
  $('count').textContent = view.state.doc.length + ' 字符';
  const canEdit = !!active && !active.readOnly && !switching;
  view.dispatch({ effects: editable.reconfigure([EditorView.editable.of(canEdit), EditorState.readOnly.of(!canEdit)]) });
}
function sync(s: SessionState) {
  const oldPath = active?.path; active = s.note;
  if (view.state.doc.toString() !== s.body) { loading = true; view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: s.body } }); loading = false; }
  $('title').textContent = active?.title ?? '选择或新建一篇笔记'; $('path').textContent = active?.path ?? 'Markdown 文件库';
  if (active) {
    tabs.add(active.path); showReadNotice(active);
    if (lastFrontmatter !== active.frontmatter) { lastFrontmatter = active.frontmatter; fillProperties(active); }
  } else { $('readNotice').hidden = true; lastFrontmatter = null; }
  if (oldPath !== active?.path) { renderTabs(); markSelected(); persistWorkspace(); }
  if (s.error && s.error !== lastError) { lastError = s.error; status(s.error.message, true); }
  if (s.lastSave && s.lastSave !== lastSave) { lastSave = s.lastSave; status(s.lastSave.warnings?.join('；') || '已写入磁盘'); }
  $('externalNotice').hidden = !s.paused;
  $('externalNotice').textContent = s.externalDeleted ? '文件已被外部删除。本地文本仍保留，自动保存已暂停；请先在外部恢复文件，或复制源文另存。' : s.conflict?.message ?? '自动保存暂停，请处理冲突。';
  if (s.conflict) { const key = s.path + ':' + s.conflict.diskHash; $('conflictMessage').textContent = s.conflict.message; if (key !== conflictKey) { conflictKey = key; $<HTMLDialogElement>('conflictDialog').showModal(); } }
  else { conflictKey = ''; $<HTMLDialogElement>('conflictDialog').close(); }
  update();
  window.dispatchEvent(new Event('notebase:editor-change'));
}
async function run(fn: () => Promise<void>) {
  if (busy) return; busy = true; update();
  try { await fn(); } catch (error) { status(message(error), true); }
  finally { switching = false; busy = false; update(); }
}
async function vaultCall<T>(command: Command, payload: Record<string, unknown> = {}): Promise<T> {
  if (!opened) throw staleVault;
  const generation = vaultGeneration, result = await call<T>(command, payload);
  if (generation !== vaultGeneration) throw staleVault;
  return result;
}
async function flush() { const ok = await session.flush(); if (!ok) status(session.state.paused ? '请先处理冲突；本地内容仍保留。' : '保存失败，当前笔记仍保留。', true); return ok; }
function markSelected() { $('notes').querySelectorAll<HTMLButtonElement>('button').forEach(b => b.classList.toggle('selected', b.dataset.path === active?.path)); }
function renderNotes() {
  const nav = $('notes'); nav.replaceChildren(); let directory: string | null = null;
  const grouped = $<HTMLInputElement>('groupTree').checked;
  for (const n of entries) {
    const dir = n.path.includes('/') ? n.path.slice(0, n.path.lastIndexOf('/')) : '根目录';
    if (grouped && dir !== directory) { directory = dir; const label = document.createElement('div'); label.className = 'tree-directory'; label.textContent = dir; nav.append(label); }
    const b = button(n.path, () => void run(() => read(n.path))); b.className = 'note'; b.title = (n.pinned ? '置顶 · ' : '') + (n.favorite ? '收藏 · ' : '') + n.path; b.dataset.path = n.path; nav.append(b);
  }
  if (!entries.length) nav.textContent = $<HTMLInputElement>('favoriteFilter').checked ? '没有收藏的笔记。' : '还没有笔记，点击新建开始。';
  markSelected();
}
async function list(reset = true) {
  const generation = vaultGeneration;
  const result = await vaultCall<{ items: NoteEntry[]; nextCursor?: string | null }>('note_list', { dirPrefix: '', favorite: $<HTMLInputElement>('favoriteFilter').checked || undefined, limit: 500, cursor: reset ? null : nextCursor });
  if (generation !== vaultGeneration) return;
  entries = reset ? result.items : [...entries, ...result.items]; nextCursor = result.nextCursor ?? null; $('more').hidden = nextCursor === null; renderNotes();
}
function renderTabs() {
  const nav = $('tabs'); nav.replaceChildren();
  persistWorkspace();
  for (const path of tabs) {
    const group = document.createElement('span'); group.className = path === active?.path ? 'tab selected' : 'tab';
    const open = button(path.split('/').at(-1)!, () => void run(() => read(path))); open.setAttribute('aria-label', '打开 ' + path);
    const close = button('×', () => void run(async () => { if (active?.path === path && !await session.close()) return; tabs.delete(path); renderTabs(); if (!active && tabs.size) await read([...tabs].at(-1)!); })); close.setAttribute('aria-label', '关闭 ' + path);
    group.append(open, close); nav.append(group);
  }
}
function showReadNotice(note: NoteContent) {
  const messages: string[] = [];
  if (note.sizeBytes > 10 * 1024 * 1024) messages.push('文件超过 10 MB，仅预览前 256 KiB，不能编辑或保存；完整内容请使用外部编辑器查看。');
  if (note.encoding !== 'utf-8') messages.push('文件包含非法 UTF-8 字节，以替换字符只读展示。原文件未修改，请用外部工具转换为 UTF-8 后重新打开。');
  if (note.readOnly && !messages.length) messages.push('此文件为只读，不能编辑或保存。');
  if (note.frontmatterError) messages.push('属性解析失败：' + note.frontmatterError + '；原文保持不变。');
  $('readNotice').textContent = messages.join(' '); $('readNotice').hidden = !messages.length;
}
async function read(path: string) {
  if (!await flush()) return;
  switching = true; update();
  try { const next = await vaultCall<NoteContent>('note_read', { path, includeRaw: true }); if (await session.switchTo(next)) { status((next.readOnly ? '已只读打开 ' : '已打开 ') + path); void loadLinks(); } }
  finally { switching = false; update(); }
}
function resetVaultView() {
  vaultGeneration++; opened = false; externalTreeDirty = false;
  nextCursor = searchCursor = null; searchText = ''; entries = [];
  tabs.clear(); renderTabs(); $('notes').replaceChildren(); $('more').hidden = true;
  $('searchResults').replaceChildren(); $('searchInfo').textContent = ''; $('searchMore').hidden = true;
  $<HTMLInputElement>('searchQuery').value = '';
  pendingRename = null; graphData = null; graphZoom = 1; lastSave = null; lastError = null;
  for (const id of ['renameDialog', 'targetDialog']) $<HTMLDialogElement>(id).close();
  $('renameDetails').replaceChildren(); $('targetCandidates').replaceChildren();
  $<HTMLFormElement>('properties').reset(); $('propertiesStatus').textContent = '';
  for (const id of ['outgoing', 'backlinks', 'mentions', 'graph', 'drafts', 'trash', 'renames']) $(id).textContent = '打开笔记库后查看';
  $<HTMLSelectElement>('template').replaceChildren(new Option('空白笔记', '')); $('templateStatus').textContent = '';
  $('indexStatus').textContent = '打开笔记库后建立索引'; $('indexWarning').hidden = true; $('indexWarning').textContent = '';
}
async function releaseVault() {
  switching = true; update();
  // Lock input before flushing, and close while all queued writes still target the old vault.
  if (!await flush() || !await session.close()) return false;
  persistWorkspace(); vaultIdentity = ''; resetVaultView(); return true;
}
async function open(path = $<HTMLInputElement>('vaultPath').value) {
  if (!await releaseVault()) return;
  $<HTMLInputElement>('vaultPath').value = path;
  await call('vault_open', { path });
  opened = true;
  if (desktopMode) { const info = await vaultCall<{ vaultPath: string }>('vault_info'); $<HTMLInputElement>('vaultPath').value = info.vaultPath; }
  vaultIdentity = desktopMode ? $<HTMLInputElement>('vaultPath').value : 'demo';
  if (desktopMode) localStorage.setItem('notebase-last-vault', vaultIdentity);
  await list();
  restoringWorkspace = true;
  try {
    const restored = restoreWorkspace(localStorage.getItem(workspaceKey()));
    for (const path of restored.tabs) tabs.add(path);
    if (restored.active) { try { await read(restored.active); } catch { tabs.delete(restored.active); } }
  } finally { restoringWorkspace = false; renderTabs(); }
  status('笔记库已打开'); window.dispatchEvent(new Event('notebase:vault-opened')); void loadRecovery(); void loadTemplates();
}
async function chooseAndOpenVault() {
  if (!desktopMode) { await open(); return; }
  const path = await chooseVaultFolder($<HTMLInputElement>('vaultPath').value);
  if (path !== null) await open(path);
}
async function migrate() {
  const target = prompt('迁移到新的绝对目录（目标必须不存在）。校验后切换，原目录会完整保留。');
  if (target === null || !await releaseVault()) return;
  await call('settings_set', { values: { vaultPath: target } });
  // The backend has switched. Even if the subsequent settings read fails, no old handles remain.
  $<HTMLInputElement>('vaultPath').value = target; opened = true;
  const settings = await vaultCall<{ values: VaultSettings }>('settings_get', { keys: ['vaultPath'] });
  $<HTMLInputElement>('vaultPath').value = settings.values.vaultPath ?? target;
  vaultIdentity = $<HTMLInputElement>('vaultPath').value;
  await list(); status('迁移完成，已切换到新目录；原目录完整保留。'); window.dispatchEvent(new Event('notebase:vault-opened')); void loadRecovery(); void loadTemplates();
}
async function prepareRestore() {
  switching = true; update();
  if (!await session.close()) { switching = false; update(); return false; }
  return true;
}
async function reloadRestoredVault() {
  // Restore has already changed disk. Do not flush stale editor text over it.
  await open();
}
async function save() { if (!active || active.readOnly || switching) return; if (await session.save()) void loadLinks(); }
function fillProperties(note: NoteContent) {
  const f = note.frontmatter;
  for (const [id, key] of [['propertyTitle', 'title'], ['propertyStatus', 'status'], ['created', 'created'], ['updated', 'updated']]) $<HTMLInputElement>(id).value = typeof f[key] === 'string' ? f[key] : '';
  $<HTMLInputElement>('tags').value = Array.isArray(f.tags) ? f.tags.join(', ') : typeof f.tags === 'string' ? f.tags : '';
  $<HTMLInputElement>('aliases').value = Array.isArray(f.aliases) ? f.aliases.join(', ') : typeof f.aliases === 'string' ? f.aliases : '';
  $<HTMLInputElement>('favorite').checked = f.favorite === true; $<HTMLInputElement>('pinned').checked = f.pinned === true;
  $('propertiesStatus').textContent = note.frontmatterError ? '属性不可写：请在源文修复 YAML。' : '';
}
async function saveProperties() {
  const split = (id: string) => $<HTMLInputElement>(id).value.split(/[,，]/).map(v => v.trim()).filter(Boolean);
  const text = (id: string) => $<HTMLInputElement>(id).value.trim() || null;
  const timestamp = (id: string) => {
    const value = text(id);
    if (value !== null && (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-](?:[01]\d|2[0-3]):[0-5]\d)$/.test(value) || !Number.isFinite(Date.parse(value)))) throw new Error('时间必须是带时区的 ISO 8601，例如 2026-01-01T09:00:00+08:00；留空移除。');
    return value;
  };
  const updates = { title: text('propertyTitle'), status: text('propertyStatus'), created: timestamp('created'), updated: timestamp('updated'), tags: split('tags'), aliases: split('aliases'), favorite: $<HTMLInputElement>('favorite').checked, pinned: $<HTMLInputElement>('pinned').checked };
  if (await session.mutate(async s => { await vaultCall('note_save', { action: 'metadata', path: s.path, baseHash: s.baseHash, updates }); return vaultCall<NoteContent>('note_read', { path: s.path, includeRaw: true }); })) { status('属性已保存'); await list(); void loadLinks(); }
}
async function panel(id: string, label: string, fn: () => Promise<void>) { const generation = vaultGeneration; try { await fn(); } catch (error) { if (generation === vaultGeneration && error !== staleVault) $(id).textContent = label + '不可用：' + message(error); } }
async function loadTemplates() {
  const generation = vaultGeneration;
  await panel('templateStatus', '模板列表', async () => {
    const result = await vaultCall<{ items: NoteEntry[] }>('note_list', { scope: 'templates' }); if (generation !== vaultGeneration) return; const select = $<HTMLSelectElement>('template'), selected = select.value; select.replaceChildren(new Option('空白笔记', ''));
    for (const n of result.items.filter(n => n.path.startsWith('templates/'))) select.add(new Option(n.path, n.path)); if (Array.from(select.options).some(o => o.value === selected)) select.value = selected;
    $('templateStatus').textContent = desktopMode ? '模板文件位于 templates/*.md' : '内存演示模板；刷新后重置';
  });
}
async function loadDrafts() {
  const generation = vaultGeneration;
  await panel('drafts', '草稿恢复', async () => {
    const { items } = await vaultCall<{ items: DraftEntry[] }>('note_list', { scope: 'drafts' }); if (generation !== vaultGeneration) return; if (items.some(d => !d.id || typeof d.body !== 'string')) throw new Error('后端未返回草稿记录');
    const target = $('drafts'); target.replaceChildren();
    for (const d of items) {
      const el = row(d.path + ' · ' + new Date(d.savedAtMs).toLocaleString()); const preview = document.createElement('pre'); preview.textContent = d.body.slice(0, 240); el.append(preview);
      for (const [choice, label] of [['restore', '恢复原位'], ['copy', '保留为副本'], ['discard', '丢弃草稿']] as const) el.append(button(label, () => void run(async () => {
        if (choice === 'discard' && !confirm('确定丢弃这份恢复草稿？')) return; if (!await flush()) return;
        const result = await vaultCall<{ path?: string; copied?: boolean }>('note_restore', { kind: 'draft', draftId: d.id, choice }); await list(); if (result.path) await read(result.path); await loadDrafts(); status(choice === 'discard' ? '草稿已丢弃' : result.copied ? '草稿已保留为恢复副本' : '草稿已恢复');
      })));
      target.append(el);
    }
    if (!items.length) target.textContent = '没有待恢复草稿。';
  });
}
async function loadTrash() {
  const generation = vaultGeneration;
  await panel('trash', '回收站', async () => {
    const { items } = await vaultCall<{ items: TrashEntry[] }>('note_list', { scope: 'trash' }); if (generation !== vaultGeneration) return; const target = $('trash'); target.replaceChildren();
    for (const t of items.filter(t => t.trashPath)) {
      const el = row(t.originalPath + ' · ' + new Date(t.deletedAtMs).toLocaleString());
      el.append(button('恢复', () => void run(async () => { if (!await flush()) return; const r = await vaultCall<{ path: string }>('note_restore', { trashPath: t.trashPath }); await list(); await read(r.path); await loadTrash(); status('已恢复 ' + r.path); })));
      el.append(button('彻底删除', () => void run(async () => { if (!confirm('彻底删除 ' + t.originalPath + '？') || !confirm('此操作无法从回收站恢复，确认永久删除？')) return; await vaultCall('note_delete', { path: t.trashPath, permanent: true, confirmed: true }); await loadTrash(); status('已永久删除'); })));
      target.append(el);
    }
    if (!target.childElementCount) target.textContent = '回收站为空。';
  });
}
async function loadRenames() {
  const generation = vaultGeneration;
  await panel('renames', '重命名恢复', async () => {
    const { items } = await vaultCall<{ items: RenamePlan[] }>('note_list', { scope: 'renames' }); if (generation !== vaultGeneration) return; const target = $('renames'); target.replaceChildren();
    for (const p of items.filter(p => p.id && p.state !== 'completed')) { const el = row(p.from + ' → ' + p.to + ' · ' + (p.state ?? '待继续')); el.append(button('继续重命名', () => void run(async () => { if (!await flush()) return; switching = true; update(); const r = await vaultCall<{ path?: string }>('note_rename', { resumeId: p.id }); tabs.delete(p.from); await list(); await read(r.path ?? p.to); await loadRenames(); status('重命名已继续完成'); }))); target.append(el); }
    if (!target.childElementCount) target.textContent = '没有未完成的重命名。';
  });
}
async function loadRecovery() { await Promise.allSettled([loadDrafts(), loadTrash(), loadRenames()]); }
async function rename() {
  if (!active || !await flush()) return; const path = active.path, newName = prompt('新的笔记名称', path.split('/').at(-1)!.replace(/\.md$/i, '')); if (newName === null) return;
  const { plan } = await vaultCall<{ plan: RenamePlan }>('note_rename', { path, newName, planOnly: true }); pendingRename = { path, newName, plan };
  const details = $('renameDetails'); details.replaceChildren(); const summary = document.createElement('p'); summary.textContent = plan.from + ' → ' + plan.to + '；' + plan.affectedFiles.length + ' 个文件，' + plan.totalReplacements + ' 处链接替换。'; details.append(summary);
  for (const file of plan.affectedFiles) { const el = row(file.path), preview = document.createElement('pre'); preview.textContent = typeof file.preview === 'string' ? file.preview : JSON.stringify(file.preview, null, 2); el.append(preview); details.append(el); }
  $<HTMLDialogElement>('renameDialog').showModal();
}
async function executeRename() {
  const pending = pendingRename; if (!pending || !await flush()) return; switching = true; update();
  try { const r = await vaultCall<{ path?: string }>('note_rename', { path: pending.path, newName: pending.newName, planOnly: false, planId: pending.plan.id }); tabs.delete(pending.path); $<HTMLDialogElement>('renameDialog').close(); pendingRename = null; await list(); await read(r.path ?? pending.plan.to); status('重命名与链接更新完成'); }
  finally { void loadRenames(); }
}
async function deleteNote() {
  if (!active || !confirm('将 ' + active.path + ' 移到回收站？') || !await flush()) return;
  const path = active.path; switching = true; update(); await vaultCall('note_delete', { path }); await session.close(); tabs.delete(path); renderTabs(); await list(); await loadTrash(); status('已移到回收站，可恢复');
}
async function addFiles(files: File[]) {
  if (!active || active.readOnly || switching) return;
  const generation = vaultGeneration;
  for (const file of files) {
    if (generation !== vaultGeneration || !active || switching) return;
    if (file.size > 32 * 1024 * 1024) { status('附件超过 32 MiB：' + file.name, true); continue; }
    const path = active.path, bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
    try {
      if (generation !== vaultGeneration || switching) return;
      await session.write(async s => {
        if (generation !== vaultGeneration || s.path !== path) throw new Error('笔记已切换，请重新插入附件。');
        const result = await vaultCall<AttachmentResult>('note_save', { action: 'attachment', path, name: file.name, bytes, image: file.type.startsWith('image/') });
        const selection = view.state.selection.main; view.dispatch({ changes: { from: selection.from, to: selection.to, insert: result.markdown }, selection: { anchor: selection.from + result.markdown.length } });
      });
      await session.save();
    } catch (error) { status('附件插入失败：' + message(error), true); }
  }
}
async function loadLinks() {
  const path = active?.path, generation = vaultGeneration; if (!path) return;
  await Promise.allSettled([
    panel('outgoing', '链接列表', async () => { const note = await vaultCall<NoteContent>('note_read', { path, probe: true }); if (generation !== vaultGeneration || active?.path !== path) return; const target = $('outgoing'); target.replaceChildren(); for (const link of note.links ?? []) target.append(button((link.label ?? link.target) + ' · ' + link.line + ':' + link.col, () => void run(() => followLink(path, link.target)))); if (!target.childElementCount) target.textContent = '没有双链。'; }),
    panel('backlinks', '反向链接', async () => { const { items } = await vaultCall<{ items: Backlink[] }>('link_backlinks', { path }); if (generation !== vaultGeneration || active?.path !== path) return; const target = $('backlinks'); target.replaceChildren(); for (const item of items) { const source = item.sourcePath ?? item.source; if (!source) continue; const el = row((item.circular ? '循环引用 · ' : '') + (item.ambiguous ? '目标有歧义 · ' : '') + (item.snippet ?? item.context ?? '') + (item.candidates?.length ? ' 候选：' + item.candidates.join('、') : '')); el.prepend(button(source + ' · ' + item.line + ':' + item.col, () => void run(() => read(source)))); target.append(el); } if (!target.childElementCount) target.textContent = '没有反向链接。'; }),
    panel('mentions', '未链接提及', async () => { const { items } = await vaultCall<{ items: Mention[] }>('link_mentions', { path }); if (generation !== vaultGeneration || active?.path !== path) return; const target = $('mentions'); target.replaceChildren(); for (const item of items) { const el = row(item.text + ' · ' + item.line + ':' + item.col); el.prepend(button(item.sourcePath, () => void run(() => read(item.sourcePath)))); el.append(button('转为双链', () => void run(() => linkMention(path, item)))); target.append(el); } if (!target.childElementCount) target.textContent = '没有未链接提及。'; }),
  ]);
}
async function followLink(sourcePath: string, target: string) {
  const r = await vaultCall<LinkResolution>('link_backlinks', { path: sourcePath, resolveTarget: target });
  if (r.targetPath && !r.ambiguous) { await read(r.targetPath); return; }
  if (r.ambiguous || r.candidates?.length) { const list = $('targetCandidates'); list.replaceChildren(); for (const path of r.candidates) list.append(button(path, () => { $<HTMLDialogElement>('targetDialog').close(); void run(() => read(path)); })); $<HTMLDialogElement>('targetDialog').showModal(); return; }
  if (!confirm('目标“' + target + '”不存在，是否创建？') || !await flush()) return;
  const name = target.split('#')[0].replace(/\.md$/i, ''), parts = name.split('/'), title = parts.pop()!; const r2 = await vaultCall<{ path: string }>('note_create', { dir: parts.join('/'), title }); await list(); await read(r2.path);
}
async function linkMention(targetPath: string, item: Mention) {
  if (!await flush()) return;
  const source = await vaultCall<NoteContent>('note_read', { path: item.sourcePath, includeRaw: true });
  if (source.readOnly || (active?.path === source.path && session.state.dirty)) throw new Error('源笔记已修改或为只读，请刷新提及后重试。');
  const bytes = new TextEncoder().encode(source.body), text = new TextDecoder('utf-8', { fatal: true }).decode(bytes.slice(item.start, item.end));
  if (text !== item.text) throw new Error('提及位置已变化，请刷新后重试。');
  const body = replaceUtf8Range(source.body, item.start, item.end, '[[' + targetPath.replace(/\.md$/i, '') + '|' + item.text + ']]');
  if (source.path === active?.path) { session.edit(body); if (!await session.save()) return; }
  else await vaultCall('note_save', { path: source.path, body, baseHash: source.contentHash, expectedMtimeMs: source.mtimeMs });
  await loadLinks(); status('已转为双链：' + item.sourcePath);
}
let graphData: GraphData | null = null, graphZoom = 1;
async function loadGraph() {
  const generation = vaultGeneration;
  await panel('graph', '图谱', async () => { const data = await vaultCall<GraphData>('link_graph', { path: $<HTMLInputElement>('graphLocal').checked ? active?.path : undefined, pathPrefix: $<HTMLInputElement>('graphPrefix').value || undefined, maxNodes: 500 }); if (generation !== vaultGeneration) return; graphData = data; graphZoom = 1; renderGraph(); });
}
function renderGraph() {
  const target = $('graph'); target.replaceChildren(); if (!graphData) return;
  const nodes = graphData.nodes.slice(0, 500).map(n => typeof n === 'string' ? { path: n, title: n } : { ...n, path: n.path ?? n.id! });
  if (!nodes.length) { target.textContent = '此范围没有节点。'; return; }
  const ns = 'http://www.w3.org/2000/svg', svg = document.createElementNS(ns, 'svg'); svg.setAttribute('aria-label', '笔记关系图'); svg.setAttribute('viewBox', (150 - 150 / graphZoom) + ' ' + (150 - 150 / graphZoom) + ' ' + 300 / graphZoom + ' ' + 300 / graphZoom);
  const positions = new Map(nodes.map((n, i) => [n.path, { x: 150 + 110 * Math.cos(i * 2 * Math.PI / nodes.length), y: 150 + 110 * Math.sin(i * 2 * Math.PI / nodes.length) }]));
  for (const edge of graphData.edges) { const from = positions.get(edge.from), to = positions.get(edge.to); if (!from || !to) continue; const line = document.createElementNS(ns, 'line'); for (const [key, value] of Object.entries({ x1: from.x, y1: from.y, x2: to.x, y2: to.y })) line.setAttribute(key, String(value)); line.setAttribute('stroke', '#a5b8ac'); svg.append(line); }
  for (const n of nodes) { const p = positions.get(n.path)!, circle = document.createElementNS(ns, 'circle'), label = document.createElementNS(ns, 'title'); circle.setAttribute('cx', String(p.x)); circle.setAttribute('cy', String(p.y)); circle.setAttribute('r', '7'); circle.setAttribute('fill', n.path === active?.path ? '#b5773c' : '#34594a'); circle.setAttribute('role', 'button'); circle.setAttribute('tabindex', '0'); circle.setAttribute('aria-label', '打开 ' + n.path); label.textContent = n.path; circle.append(label); circle.addEventListener('click', () => void run(() => read(n.path))); circle.addEventListener('keydown', e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); void run(() => read(n.path)); } }); svg.append(circle); }
  target.append(svg); for (const n of nodes) target.append(button('图谱 ' + n.path, () => void run(() => read(n.path))));
}
$('mode').textContent = desktopMode ? '桌面模式：写入所选文件夹中的真实 .md 文件。已接入自动保存与恢复草稿。' : '浏览器演示：仅使用内存数据，刷新即重置，不读写本地磁盘。';
$('open').addEventListener('click', () => void run(chooseAndOpenVault)); $('openPath').addEventListener('click', () => void run(open)); $('save').addEventListener('click', () => void save());
if (!desktopMode) $('open').textContent = '打开演示库';
$('more').addEventListener('click', () => void run(() => list(false)));
$('refresh').addEventListener('click', () => void run(async () => { await list(); void loadRecovery(); status('文件列表已刷新，笔记内容由外部检查独立核对'); }));
$('favoriteFilter').addEventListener('change', () => void run(() => list())); $('groupTree').addEventListener('change', renderNotes);
$('new').addEventListener('click', () => { if (busy) return; const title = prompt('笔记标题'); if (title === null) return; void run(async () => { if (!await flush()) return; const dir = $<HTMLInputElement>('newDir').value, templatePath = $<HTMLSelectElement>('template').value; const r = await vaultCall<{ path: string }>('note_create', { dir, title, ...(templatePath ? { templatePath } : {}) }); await list(); await read(r.path); if (r.path !== (dir ? dir + '/' : '') + title + '.md') status('文件名已修正或避开同名：' + r.path); }); });
$('properties').addEventListener('submit', e => { e.preventDefault(); void run(saveProperties); });
$('rename').addEventListener('click', () => void run(rename)); $('renameConfirm').addEventListener('click', () => void run(executeRename));
$('renameCancel').addEventListener('click', () => { pendingRename = null; $<HTMLDialogElement>('renameDialog').close(); });
$('renameDialog').addEventListener('cancel', () => { pendingRename = null; }); $('delete').addEventListener('click', () => void run(deleteNote));
$('showConflict').addEventListener('click', () => $<HTMLDialogElement>('conflictDialog').showModal()); $('conflictLater').addEventListener('click', () => $<HTMLDialogElement>('conflictDialog').close());
$('conflictDialog').querySelectorAll<HTMLButtonElement>('[data-choice]').forEach(b => b.addEventListener('click', () => { void (async () => { if (await session.resolveConflict(b.dataset.choice as 'local' | 'disk' | 'copy')) { status('冲突已按所选方式处理'); await list(); void loadLinks(); void loadDrafts(); } })().catch(e => status(message(e), true)); }));
$('targetClose').addEventListener('click', () => $<HTMLDialogElement>('targetDialog').close());
$('attachment').addEventListener('change', e => { const input = e.target as HTMLInputElement; void addFiles(Array.from(input.files ?? [])); input.value = ''; });
view.dom.addEventListener('dragover', e => { if (e.dataTransfer?.types.includes('Files') && active && !active.readOnly) e.preventDefault(); });
view.dom.addEventListener('drop', e => { const files = Array.from(e.dataTransfer?.files ?? []); if (files.length && active && !active.readOnly) { e.preventDefault(); const pos = view.posAtCoords({ x: e.clientX, y: e.clientY }); if (pos !== null) view.dispatch({ selection: { anchor: pos } }); void addFiles(files); } });
view.dom.addEventListener('paste', e => { const files = Array.from(e.clipboardData?.files ?? []); if (files.length && active && !active.readOnly) { e.preventDefault(); void addFiles(files); } });
$('draftsRefresh').addEventListener('click', () => void loadDrafts()); $('trashRefresh').addEventListener('click', () => void loadTrash()); $('renamesRefresh').addEventListener('click', () => void loadRenames()); $('linksRefresh').addEventListener('click', () => void loadLinks());
$('graphRefresh').addEventListener('click', () => void loadGraph()); $('graphZoomIn').addEventListener('click', () => { graphZoom = Math.min(4, graphZoom * 1.25); renderGraph(); }); $('graphZoomOut').addEventListener('click', () => { graphZoom = Math.max(0.5, graphZoom / 1.25); renderGraph(); });

// Browsers cannot await unload. Start flush and retain the native unsaved-changes guard.
window.addEventListener('beforeunload', e => { if (session.state.dirty || session.state.paused || session.state.saving) { void session.flush(); e.preventDefault(); e.returnValue = ''; } });
if (desktopMode) { void import('@tauri-apps/api/window').then(({ getCurrentWindow }) => { const win = getCurrentWindow(); let closing = false; return win.onCloseRequested(async e => { if (closing) return; e.preventDefault(); if (busy || !await flush()) return; try { if (opened) await vaultCall('vault_info'); } catch (error) { status(message(error), true); return; } persistWorkspace(); closing = true; await win.close(); }); }).catch(error => status('窗口关闭保护不可用：' + message(error), true)); }
window.setInterval(() => { if (opened && !switching) session.tick(); }, 100);
window.setInterval(() => { if (opened && active && !switching) void session.checkExternal(); }, 2000);
// Search snippets contain escaped text and exact <b> markers. Only create text and mark nodes.
function snippetParts(snippet: string) {
  const decode = (value: string) => value.replace(/&(?:amp|lt|gt|quot|#39);/g, entity => ({ '&amp;': '&', '&lt;': '<', '&gt;': '>', '&quot;': '"', '&#39;': "'" })[entity]!);
  let matched = false;
  const parts: { text: string; matched: boolean }[] = [];
  for (const token of snippet.split(/(<b>|<\/b>)/g)) {
    if (token === '<b>') matched = true;
    else if (token === '</b>') matched = false;
    else if (token) parts.push({ text: decode(token), matched });
  }
  return parts;
}
function locateSearchResult(item: SearchHit) {
  const parts = snippetParts(item.snippet), plain = parts.map(part => part.text).join('');
  const leading = plain.startsWith('…') ? 1 : 0, context = plain.slice(leading, plain.endsWith('…') ? -1 : undefined);
  const body = view.state.doc.toString(), contextAt = context ? body.indexOf(context) : -1;
  let offset = 0, firstMatch: { text: string; offset: number } | null = null;
  for (const part of parts) { if (part.matched && part.text) { firstMatch = { text: part.text, offset }; break; } offset += part.text.length; }
  let from = contextAt < 0 ? -1 : contextAt + Math.max(0, (firstMatch?.offset ?? leading) - leading);
  if (from < 0 && firstMatch) from = body.toLocaleLowerCase().indexOf(firstMatch.text.toLocaleLowerCase());
  if (from < 0) return;
  const to = Math.min(body.length, from + (firstMatch?.text.length ?? 0));
  view.dispatch({ selection: { anchor: from, head: to }, effects: EditorView.scrollIntoView(from, { y: 'center' }) }); view.focus();
}
async function searchNotes(more = false) {
  await run(async () => {
    if (!desktopMode || !opened) throw new Error('全文检索需要打开桌面文件库');
    if (more && !searchCursor) return;
    const generation = vaultGeneration, query = more ? searchText : $<HTMLInputElement>('searchQuery').value;
    const result = await vaultCall<SearchPage>('search', { query, limit: 50, sort: $<HTMLSelectElement>('searchSort').value, cursor: more ? searchCursor : null, filters: { pathPrefix: $<HTMLInputElement>('searchPath').value || undefined, tags: $<HTMLInputElement>('searchTag').value.split(/[,，]/).map(s => s.trim()).filter(Boolean) } });
    if (!more) $('searchResults').replaceChildren();
    for (const item of result.items) {
      const b = button('', () => void run(async () => {
        if (generation !== vaultGeneration) return;
        await read(item.path);
        if (generation === vaultGeneration && active?.path === item.path && !session.state.paused) locateSearchResult(item);
      }));
      b.className = 'note'; b.dataset.path = item.path;
      const label = document.createElement('span'); label.textContent = item.title + ' · ' + item.path;
      const snippet = document.createElement('small'); snippet.className = 'muted'; snippet.style.display = 'block';
      for (const part of snippetParts(item.snippet)) {
        if (part.matched) { const mark = document.createElement('mark'); mark.textContent = part.text; snippet.append(mark); }
        else snippet.append(document.createTextNode(part.text));
      }
      b.title = snippet.textContent ?? ''; b.append(label, snippet); $('searchResults').append(b);
    }
    searchText = query; searchCursor = result.nextCursor; $('searchMore').hidden = !searchCursor;
    $('searchInfo').textContent = result.totalApprox + ' 条结果' + (result.degraded ? result.degradeReason?.startsWith('index_rebuilding') ? ' · 索引重建中，结果可能不完整' : ' · 短查询扫描' : '');
  });
}
$('searchButton').addEventListener('click', () => void searchNotes()); $('searchMore').addEventListener('click', () => void searchNotes(true));
$('searchQuery').addEventListener('keydown', event => { if (event.key === 'Enter') void searchNotes(); });
$('rebuild').addEventListener('click', () => void run(async () => { const result = await vaultCall<IndexRebuildResult>('index_rebuild', { mode: 'full' }); status('已启动后台重建：' + result.jobId); }));
$('migrate').addEventListener('click', () => void run(migrate));
if (desktopMode) {
  void import('@tauri-apps/api/event').then(async ({ listen }) => {
    await listen<{ path?: string; source?: string; contentHash?: string | null }>(transportEventName('note.changed'), event => {
      externalTreeDirty = true;
      // App saves also emit note.changed. Hash probing, with EditorSession's saving guard, decides conflicts.
      if (opened && !switching && event.payload.source === 'external' && !!active && (!event.payload.path || active.path === event.payload.path) && event.payload.contentHash !== session.state.baseHash) { status('磁盘文件已变化，正在核对当前编辑版本。'); void session.checkExternal(); }
    });
    await listen<{ code: string; message: string; detail?: { recoveryDir?: string } }>(transportEventName('error.raised'), event => {
      if (event.payload.code === 'E_INDEX_UNAVAILABLE') { $('indexWarning').hidden = false; $('indexWarning').textContent = event.payload.message + (event.payload.detail?.recoveryDir ? ' · 原件归档：' + event.payload.detail.recoveryDir : ''); }
      else status(event.payload.message, true);
    });
  }).catch(error => status('实时事件监听不可用：' + message(error), true));
  void call<{ values: VaultSettings }>('settings_get', { keys: ['vaultPath'] }).then(result => { if (!opened && !switching && !$<HTMLInputElement>('vaultPath').value && result.values.vaultPath) $<HTMLInputElement>('vaultPath').value = result.values.vaultPath; }).catch(() => {});
}
async function pollIndex() {
  if (!desktopMode) { $('indexStatus').textContent = '浏览器演示不建立磁盘索引'; return; }
  const generation = vaultGeneration;
  if (opened && !switching) {
    try {
      const value = await vaultCall<IndexStatus>('index_status', {}), labels = { ready: '索引已就绪', rebuilding: '正在构建索引', offline: '索引需刷新或存在不可索引文件', unavailable: '索引不可用' };
      if (generation === vaultGeneration) $('indexStatus').textContent = labels[value.state] + ' · ' + value.noteCount + ' 篇' + (value.noteCount >= 100000 ? ' · 大库模式：短词检索可能需要更长时间' : '') + (value.progress ? ' · ' + value.progress.done + '/' + value.progress.total : '');
    } catch (error) { if (generation === vaultGeneration && error !== staleVault) $('indexStatus').textContent = '索引不可用：' + message(error); }
    if (generation === vaultGeneration && externalTreeDirty && !busy) {
      externalTreeDirty = false;
      await run(async () => { try { await list(); } catch (error) { externalTreeDirty = true; throw error; } });
    }
  }
  window.setTimeout(() => void pollIndex(), 1000);
}
mountDataTools({desktop:desktopMode,note:()=>active,body:()=>view.state.doc.toString(),call:vaultCall,flush,read,prepareRestore,reload:reloadRestoredVault,run,status});

function workspaceKey() { return 'notebase-workspace:' + windowLabel + ':' + vaultIdentity; }
function persistWorkspace() {
  if (!vaultIdentity || restoringWorkspace) return;
  try { localStorage.setItem(workspaceKey(), JSON.stringify({ tabs: [...tabs].slice(-100), active: active?.path ?? null })); }
  catch { status('无法记录标签页布局；笔记内容不受影响', true); }
}
async function newWindow() {
  if (!desktopMode || !opened) { status('请先打开桌面笔记库'); return; }
  if (!await flush()) return;
  const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow');
  const { getAllWindows } = await import('@tauri-apps/api/window');
  const windows = await getAllWindows(); let slot = 1; while (windows.some(w => w.label === 'note-' + slot)) slot++;
  const win = new WebviewWindow('note-' + slot, { url: 'index.html?vault=' + encodeURIComponent(vaultIdentity), title: 'NoteBase · 笔记窗口', width: 1280, height: 820, minWidth: 800, minHeight: 600 });
  await win.once('tauri://error', event => status('新窗口创建失败：' + String(event.payload), true));
}
function applyEditorPreferences(value: Preferences) {
  view.dispatch({ effects: editorPreferences.reconfigure([
    EditorState.tabSize.of(value.editor.tabSize), ...(value.editor.wordWrap ? [EditorView.lineWrapping] : []),
    EditorView.theme({ '&': { fontSize: value.editor.fontSize + 'px' }, '.cm-content': { maxWidth: value.editor.lineWidth + 'ch' }, '.cm-scroller': { color: 'var(--text)' } }),
  ]) });
}
async function boot() {
  if (desktopMode) { const { getCurrentWindow } = await import('@tauri-apps/api/window'); windowLabel = getCurrentWindow().label; }
  const clickAction = (id: string, label: string) => ({ id, label, run: () => $(id).click(), available: () => !($<HTMLButtonElement>(id).disabled) });
  await mountShell([
    clickAction('new', '新建笔记'), { id: 'search', label: '搜索笔记', run: () => $('searchQuery').focus() },
    { id: 'save', label: '保存当前笔记', run: save, available: () => !!active && !active.readOnly },
    { id: 'nextTab', label: '切换到下一标签页', run: () => run(async () => { const paths = [...tabs]; if (paths.length) await read(paths[(paths.indexOf(active?.path ?? '') + 1) % paths.length]); }) },
    { id: 'newWindow', label: '新建笔记窗口', run: newWindow, available: () => desktopMode && opened },
    ...[['open', '打开笔记库'], ['refresh', '刷新文件列表'], ['rename', '重命名笔记'], ['delete', '移到回收站'], ['rebuild', '后台重建索引'], ['migrate', '迁移笔记库目录'], ['linksRefresh', '刷新反向链接'], ['draftsRefresh', '查看恢复草稿'], ['trashRefresh', '查看回收站'], ['graphRefresh', '加载关系图谱']].map(([id, label]) => clickAction(id, label)),
  ], applyEditorPreferences, status);
  update();
  if (desktopMode) {
    const requested = new URLSearchParams(location.search).get('vault');
    const stored = await call<{ values: { vaultPath: string | null } }>('settings_get', { keys: ['vaultPath'] }).catch(() => null);
    $<HTMLInputElement>('vaultPath').value = requested ?? stored?.values.vaultPath ?? localStorage.getItem('notebase-last-vault') ?? '';
    bootReady = true; update();
    if ($<HTMLInputElement>('vaultPath').value) await run(open);
  } else { bootReady = true; update(); await run(open); }
  void pollIndex();
}
update();
void boot().catch(error => { bootReady = true; update(); status('初始化失败：' + message(error), true); });

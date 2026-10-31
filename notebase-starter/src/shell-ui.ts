import { call, desktopMode } from './bridge';
import { defaultPreferences, shortcutMatches, validatePreferences, PreferenceValidationError, type Preferences } from './preferences';
export interface ShellAction { id: string; label: string; run(): void | Promise<void>; available?(): boolean }
export async function mountShell(actions: ShellAction[], applyEditor: (value: Preferences) => void, report: (message: string, error?: boolean) => void) {
  let preferences = structuredClone(defaultPreferences);
  const toolbar = document.createElement('div'); toolbar.className = 'shell-toolbar'; toolbar.setAttribute('aria-label', '工作区工具');
  toolbar.innerHTML = '<button id="commands">命令面板</button><button id="settings">设置</button><button id="focus">专注模式</button><button id="newWindow">新建窗口</button>';
  document.querySelector('main')!.prepend(toolbar);
  document.body.insertAdjacentHTML('beforeend', `<dialog id="settingsDialog" aria-labelledby="settingsTitle"><h2 id="settingsTitle">工作区设置</h2><form id="settingsForm" novalidate>
    <div class="settings-grid"><label>主题<select id="theme"><option value="system">跟随系统</option><option value="light">浅色</option><option value="dark">深色</option></select></label>
    <label>布局<select id="layoutPreset"><option value="student">学生 · 文件与编辑</option><option value="researcher">研究者 · 引用与属性</option><option value="developer">开发者 · 标签页</option><option value="writer">写作者 · 专注</option></select></label>
    <label>字号<input id="fontSize" type="number" min="10" max="32"></label><label>行宽（字符）<input id="lineWidth" type="number" min="40" max="200"></label><label>缩进宽度<input id="tabSize" type="number" min="1" max="8"></label><label class="check"><input id="wordWrap" type="checkbox">自动换行</label></div>
    <h3>快捷键</h3><div id="keymapFields" class="settings-grid"></div><p id="settingsError" role="alert"></p><div class="dialog-actions"><button type="button" id="settingsClose">取消</button><button class="primary" id="settingsSave">确定</button></div></form></dialog>
    <dialog id="commandDialog" aria-labelledby="commandTitle"><h2 id="commandTitle">命令面板</h2><input id="commandQuery" aria-label="查找命令" placeholder="输入操作名称" aria-describedby="commandError"><p id="commandError" class="field-error" role="alert" hidden></p><div id="commandResults"></div><div class="dialog-actions"><button id="commandClose">取消</button><button id="commandConfirm" class="primary">确定</button></div></dialog>`);
  const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
  const labels: Record<string, string> = { new: '新建笔记', search: '搜索笔记', save: '保存', nextTab: '下一标签页', commandPalette: '命令面板', globalQuickNote: '全局唤出（Windows）' };
  for (const [key, label] of Object.entries(labels)) { const field = document.createElement('label'); field.textContent = label + '快捷键'; const input = document.createElement('input'); input.id = 'key-' + key; field.append(input); $('keymapFields').append(field); }
  $('settingsForm').querySelectorAll<HTMLInputElement | HTMLSelectElement>('input,select').forEach(input => {
    const warning = document.createElement('p'); warning.id = input.id + '-error'; warning.className = 'field-error'; warning.hidden = true; warning.setAttribute('role', 'alert'); input.after(warning); input.setAttribute('aria-describedby', warning.id);
    input.addEventListener('input', () => { warning.hidden = true; input.removeAttribute('aria-invalid'); });
  });
  function clearWarnings() { $('settingsError').textContent = ''; $('settingsForm').querySelectorAll<HTMLElement>('.field-error').forEach(e => { e.hidden = true; }); $('settingsForm').querySelectorAll('[aria-invalid]').forEach(e => e.removeAttribute('aria-invalid')); }
  function fieldWarning(field: string, text: string) { const input = document.getElementById(field), warning = document.getElementById(field + '-error'); if (input && warning) { warning.textContent = text; warning.hidden = false; input.setAttribute('aria-invalid', 'true'); } }
  function apply() {
    const dark = preferences.theme === 'dark' || (preferences.theme === 'system' && matchMedia('(prefers-color-scheme: dark)').matches);
    document.documentElement.dataset.theme = dark ? 'dark' : 'light';
    document.documentElement.dataset.layout = preferences.layoutPreset;
    document.documentElement.classList.toggle('focus-mode', preferences.layoutPreset === 'writer');
    applyEditor(preferences);
  }
  function fill() {
    for (const key of ['theme', 'layoutPreset'] as const) $<HTMLSelectElement>(key).value = preferences[key];
    for (const key of ['fontSize', 'lineWidth', 'tabSize'] as const) $<HTMLInputElement>(key).value = String(preferences.editor[key]);
    $<HTMLInputElement>('wordWrap').checked = preferences.editor.wordWrap;
    for (const [key, value] of Object.entries(preferences.keymap)) $<HTMLInputElement>('key-' + key).value = value;
    clearWarnings();
  }
  async function execute(action: ShellAction) { if (action.available?.() === false) return; try { await action.run(); } catch (error) { report(String(error), true); } }
  const openSettings = () => { fill(); $<HTMLDialogElement>('settingsDialog').showModal(); };
  const allActions: ShellAction[] = [...actions, { id: 'settings', label: '打开设置', run: openSettings }, { id: 'focus', label: '切换专注模式', run: () => { document.documentElement.classList.toggle('focus-mode'); } }];
  function renderCommands() {
    $('commandError').hidden = true; $('commandQuery').removeAttribute('aria-invalid');
    const query = $<HTMLInputElement>('commandQuery').value.trim().toLowerCase(); $('commandResults').replaceChildren();
    for (const action of allActions.filter(a => a.label.toLowerCase().includes(query))) { const button = document.createElement('button'); button.textContent = action.label; button.disabled = action.available?.() === false; button.onclick = () => { $<HTMLDialogElement>('commandDialog').close(); void execute(action); }; $('commandResults').append(button); }
    if (!$('commandResults').childElementCount) $('commandResults').textContent = '没有匹配的命令';
  }
  const openCommands = () => { $<HTMLInputElement>('commandQuery').value = ''; renderCommands(); $<HTMLDialogElement>('commandDialog').showModal(); $('commandQuery').focus(); };
  $('commands').onclick = openCommands; $('settings').onclick = openSettings;
  $('focus').onclick = () => { document.documentElement.classList.toggle('focus-mode'); };
  $('newWindow').onclick = () => { const action = actions.find(a => a.id === 'newWindow'); if (action) void execute(action); };
  $<HTMLButtonElement>('newWindow').disabled = !desktopMode;
  $('commandQuery').oninput = renderCommands;
  $('commandConfirm').onclick = () => { const action = $('commandResults').querySelector<HTMLButtonElement>('button:not(:disabled)'); if (action) action.click(); else { $('commandError').textContent = '没有可执行的匹配命令，请修改输入'; $('commandError').hidden = false; $('commandQuery').setAttribute('aria-invalid', 'true'); $('commandQuery').focus(); } };
  $('commandQuery').onkeydown = event => { if (event.key === 'ArrowDown' || event.key === 'Enter') { event.preventDefault(); $('commandResults').querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus(); } };
  $('settingsClose').onclick = () => $<HTMLDialogElement>('settingsDialog').close(); $('commandClose').onclick = () => $<HTMLDialogElement>('commandDialog').close();
  let settingsSaving = false;
  $<HTMLDialogElement>('settingsDialog').addEventListener('cancel', event => { if (settingsSaving) event.preventDefault(); });
  $('settingsForm').onsubmit = event => { event.preventDefault(); if (settingsSaving) return; clearWarnings(); settingsSaving = true; $<HTMLButtonElement>('settingsSave').disabled = $<HTMLButtonElement>('settingsClose').disabled = true; void (async () => {
    const next = structuredClone(preferences);
    next.theme = $<HTMLSelectElement>('theme').value as Preferences['theme']; next.layoutPreset = $<HTMLSelectElement>('layoutPreset').value as Preferences['layoutPreset'];
    for (const key of ['fontSize', 'lineWidth', 'tabSize'] as const) next.editor[key] = Number($<HTMLInputElement>(key).value);
    next.editor.wordWrap = $<HTMLInputElement>('wordWrap').checked;
    for (const key of Object.keys(next.keymap) as (keyof Preferences['keymap'])[]) next.keymap[key] = $<HTMLInputElement>('key-' + key).value.trim();
    validatePreferences(next);
    if (desktopMode) await call('settings_set', { values: next }); else localStorage.setItem('notebase-demo-preferences', JSON.stringify(next));
    preferences = next; apply(); $<HTMLDialogElement>('settingsDialog').close(); report('设置已保存');
  })().catch(error => { const text = String(error); $('settingsError').textContent = text; if (error instanceof PreferenceValidationError) { fieldWarning(error.field, error.message); if (error.relatedField) fieldWarning(error.relatedField, error.message); document.getElementById(error.field)?.focus(); } else if (error?.code === 'E_KEYMAP_CONFLICT') { fieldWarning('key-globalQuickNote', text); } }).finally(() => { settingsSaving = false; $<HTMLButtonElement>('settingsSave').disabled = $<HTMLButtonElement>('settingsClose').disabled = false; }); };
  document.addEventListener('keydown', event => {
    if (event.isComposing || document.querySelector('dialog[open]')) return;
    for (const [id, shortcut] of Object.entries(preferences.keymap)) {
      if (id === 'globalQuickNote' || !shortcutMatches(event, shortcut)) continue;
      event.preventDefault(); event.stopImmediatePropagation();
      if (id === 'commandPalette') openCommands(); else { const action = actions.find(a => a.id === id); if (action) void execute(action); } break;
    }
  }, true);
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', apply);
  try {
    const stored = desktopMode ? (await call<{ values: Preferences }>('settings_get', {})).values : JSON.parse(localStorage.getItem('notebase-demo-preferences') ?? 'null');
    if (stored) preferences = validatePreferences({ theme: stored.theme ?? defaultPreferences.theme, layoutPreset: stored.layoutPreset ?? defaultPreferences.layoutPreset, editor: { ...defaultPreferences.editor, ...stored.editor }, keymap: { ...defaultPreferences.keymap, ...stored.keymap } });
  } catch (error) { report('读取设置失败，使用默认值：' + String(error), true); }
  apply();
  if (desktopMode) {
    const { listen } = await import('@tauri-apps/api/event');
    await listen<string>('notebase:shell_action', e => { if (e.payload === 'commands') openCommands(); else { const action = allActions.find(a => a.id === e.payload); if (action) void execute(action); } });
  }
  return { preferences: () => preferences };
}

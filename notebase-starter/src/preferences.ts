export interface Preferences {
  theme: 'system' | 'light' | 'dark'; layoutPreset: 'student' | 'researcher' | 'developer' | 'writer';
  editor: { fontSize: number; lineWidth: number; tabSize: number; wordWrap: boolean };
  keymap: Record<'new' | 'search' | 'save' | 'nextTab' | 'commandPalette' | 'globalQuickNote', string>;
}
export const defaultPreferences: Preferences = {
  theme: 'system', layoutPreset: 'student', editor: { fontSize: 14, lineWidth: 90, tabSize: 4, wordWrap: true },
  keymap: { new: 'Ctrl+N', search: 'Ctrl+Shift+F', save: 'Ctrl+S', nextTab: 'Ctrl+Tab', commandPalette: 'Ctrl+Shift+P', globalQuickNote: 'Ctrl+Shift+Space' },
};
export function shortcutMatches(event: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'metaKey' | 'altKey' | 'shiftKey'>, shortcut: string) {
  const parts = shortcut.toLowerCase().split('+'), key = parts.pop();
  return (key === 'space' ? event.key === ' ' : event.key.toLowerCase() === key)
    && event.ctrlKey === parts.includes('ctrl') && event.metaKey === parts.includes('meta')
    && event.altKey === parts.includes('alt') && event.shiftKey === parts.includes('shift');
}
export function validatePreferences(value: Preferences) {
  if (!['system', 'light', 'dark'].includes(value.theme) || !['student', 'researcher', 'developer', 'writer'].includes(value.layoutPreset)) throw new Error('主题或布局不合法');
  const { fontSize, lineWidth, tabSize, wordWrap } = value.editor;
  if (!Number.isInteger(fontSize) || fontSize < 10 || fontSize > 32 || !Number.isInteger(lineWidth) || lineWidth < 40 || lineWidth > 200 || !Number.isInteger(tabSize) || tabSize < 1 || tabSize > 8 || typeof wordWrap !== 'boolean') throw new Error('编辑器设置超出范围');
  const seen = new Set<string>();
  for (const shortcut of Object.values(value.keymap)) {
    if (!/^(?:(?:Ctrl|Alt|Shift|Meta)\+){1,3}(?:[A-Za-z0-9]|Tab|Space|Enter)$/.test(shortcut)) throw new Error('快捷键格式例如 Ctrl+Shift+P');
    const parts = shortcut.toLowerCase().split('+'), key = parts.pop()!;
    if (new Set(parts).size !== parts.length) throw new Error('快捷键包含重复修饰键');
    const normalized = [...parts.sort(), key].join('+');
    if (seen.has(normalized)) throw new Error('E_KEYMAP_CONFLICT：快捷键重复'); seen.add(normalized);
  }
  return value;
}
export function restoreWorkspace(raw: string | null): { tabs: string[]; active: string | null } {
  try {
    const parsed = JSON.parse(raw ?? '{}');
    const valid = (path: unknown): path is string => typeof path === 'string' && path.length <= 1024 && !/^[\\/]|^[a-z]:/i.test(path) && !path.includes('\\') && !path.split('/').some(s => !s || s === '.' || s === '..');
    const tabs: string[] = [...new Set<string>((Array.isArray(parsed.tabs) ? parsed.tabs : []).filter(valid))].slice(0, 100);
    return { tabs, active: valid(parsed.active) && tabs.includes(parsed.active) ? parsed.active : tabs[0] ?? null };
  } catch { return { tabs: [], active: null }; }
}

import { describe, expect, it } from 'vitest';
import { defaultPreferences, restoreWorkspace, shortcutMatches, validatePreferences } from './preferences';
describe('工作区偏好与恢复', () => {
  it('拒绝大小写/顺序不同但实际冲突的快捷键', () => { const p = structuredClone(defaultPreferences); p.keymap.new = 'Shift+Ctrl+P'; expect(() => validatePreferences(p)).toThrow('E_KEYMAP_CONFLICT'); });
  it('拒绝非法范围，不将字符串视为布尔值', () => { const p = structuredClone(defaultPreferences); p.editor.fontSize = 100; expect(() => validatePreferences(p)).toThrow(); });
  it('只恢复有界的库相对路径，去重且损坏数据安全降级', () => { expect(restoreWorkspace('{broken')).toEqual({ tabs: [], active: null }); expect(restoreWorkspace(JSON.stringify({ tabs: ['a.md', '../secret', 'C:/secret', 'a.md', '/root'], active: '../secret' }))).toEqual({ tabs: ['a.md'], active: 'a.md' }); });
  it('快捷键必须精确匹配修饰键，不能抢占其它组合', () => { const e = { key: 's', ctrlKey: true, metaKey: false, altKey: false, shiftKey: false }; expect(shortcutMatches(e, 'Ctrl+S')).toBe(true); expect(shortcutMatches({ ...e, shiftKey: true }, 'Ctrl+S')).toBe(false); });
});

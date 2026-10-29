import {describe,it,expect} from 'vitest';
import {COMMANDS,ERROR_CODES,EVENTS,transportEventName} from './contracts';
import {readFileSync} from 'node:fs';
describe('跨语言接口名称一致性',()=>{
 it('所有冻结命令均在 Rust 传输层注册',()=>{const rust=readFileSync('src-tauri/src/main.rs','utf8');expect(COMMANDS).toHaveLength(23);expect(new Set(COMMANDS).size).toBe(23);for(const c of COMMANDS)expect(rust).toContain(`command!(${c})`);});
 it('事件与错误码无重复',()=>{expect(EVENTS).toHaveLength(7);expect(ERROR_CODES).toHaveLength(23);expect(new Set(ERROR_CODES).size).toBe(23);});
 it('逻辑事件映射为合法且互不冲突的 Tauri 传输名称',()=>{const names=EVENTS.map(transportEventName);expect(new Set(names).size).toBe(EVENTS.length);for(const name of names)expect(name).toMatch(/^[A-Za-z0-9_:/-]+$/);expect(transportEventName('note.changed')).toBe('notebase:note_changed');});
});

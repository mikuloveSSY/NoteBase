import { describe, it, expect, vi } from 'vitest';
import { EditorSession, replaceUtf8Range, type SessionSnapshot } from './editor-session';
import type { NoteContent, SaveResult, Request, Response } from './contracts';

const wire = vi.hoisted(() => ({ requests: [] as Request[], failure: false }));
vi.mock('@tauri-apps/api/core', () => ({
  isTauri: () => true,
  invoke: async (_command: string, args: { request: Request }): Promise<Response<unknown>> => {
    wire.requests.push(args.request);
    return wire.failure ? { id: args.request.id, ok: false, error: { code: 'E_EXTERNAL_MODIFIED', message: 'disk changed', detail: { diskHash: 'disk' }, retryable: false } } : { id: args.request.id, ok: true, data: {} };
  },
}));
import { call, AppCallError } from './bridge';

function note(path = 'a.md', body = '', contentHash = 'base'): NoteContent {
  return { path, title: path, body, contentHash, frontmatter: {}, frontmatterError: null, mtimeMs: 0, sizeBytes: body.length, encoding: 'utf-8', lineEnding: 'lf', readOnly: false };
}
function saved(s: SessionSnapshot): SaveResult {
  return { path: s.path, contentHash: 'hash:' + s.body, mtimeMs: 1, bytesWritten: s.body.length, snapshotCreated: false, indexQueued: false, warnings: [] };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(r => { resolve = r; });
  return { promise, resolve };
}
const settle = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };

describe('EditorSession autosave and revision safety', () => {
  it('keeps drafts and retries busy maintenance writes without a rapid retry loop', async () => {
    let now = 0, attempts = 0;
    const drafts: string[] = [];
    const session = new EditorSession({now:()=>now, draft:async s=>{drafts.push(s.body);},save:async s=>{
      attempts++; if(attempts===1)throw {code:'E_SYNC_BUSY',message:'busy',detail:{}};return saved(s);
    }});
    await session.switchTo(note());session.edit('keep local');now=2000;session.tick();await settle();
    expect(attempts).toBe(1);expect(session.state.dirty).toBe(true);
    now=2500;session.tick();await settle();expect(attempts).toBe(1);expect(drafts).toContain('keep local');
    now=3000;session.tick();await settle();expect(attempts).toBe(2);expect(session.state.dirty).toBe(false);
  });
  it('writes drafts on a 500ms cycle even while typing continuously', async () => {
    let now = 0;
    const drafts: SessionSnapshot[] = [];
    const session = new EditorSession({ now: () => now, save: async s => saved(s), draft: async s => { drafts.push(s); } });
    await session.switchTo(note());
    for (now = 100; now <= 1500; now += 100) { session.edit(String(now)); session.tick(); await settle(); }
    expect(drafts.map(d => d.body)).toEqual(['500', '1000', '1500']);
    expect(session.state.dirty).toBe(true);
  });

  it('saves after 2 seconds idle and forces a save at 10 seconds of continuous input', async () => {
    let now = 0;
    const writes: SessionSnapshot[] = [];
    const session = new EditorSession({ now: () => now, save: async s => { writes.push(s); return saved(s); }, draft: async () => {} });
    await session.switchTo(note());
    session.edit('idle'); now = 1999; session.tick(); await settle(); expect(writes).toHaveLength(0);
    now = 2000; session.tick(); await settle(); expect(writes.map(w => w.body)).toEqual(['idle']);
    for (now = 2100; now <= 12100; now += 100) { session.edit(String(now)); session.tick(); await settle(); }
    expect(writes.map(w => w.body)).toEqual(['idle', '12100']);
    expect(session.state.dirty).toBe(false);
  });

  it('keeps typing after an async save dirty and only commits the submitted revision', async () => {
    const pending = deferred<SaveResult>();
    const snapshots: SessionSnapshot[] = [];
    const session = new EditorSession({ save: async s => { snapshots.push(s); return snapshots.length === 1 ? pending.promise : saved(s); }, draft: async () => {} });
    await session.switchTo(note()); session.edit('first'); const saving = session.save(); await settle();
    session.edit('second');
    expect(session.state.body).toBe('second'); expect(session.state.saving).toBe(true);
    pending.resolve(saved(snapshots[0])); await saving;
    expect(session.state.committedRevision).toBe(1); expect(session.state.revision).toBe(2); expect(session.state.dirty).toBe(true);
    await session.flush();
    expect(snapshots[1].baseHash).toBe('hash:first'); expect(session.state.dirty).toBe(false);
  });

  it('serializes draft/save for one path and flushes before switching or closing', async () => {
    const pending = deferred<void>();
    const order: string[] = [];
    let now = 0;
    const session = new EditorSession({ now: () => now, draft: async s => { order.push('draft:' + s.path); await pending.promise; }, save: async s => { order.push('save:' + s.path); return saved(s); } });
    await session.switchTo(note()); session.edit('changed'); now = 500; session.tick(); await settle();
    const switching = session.switchTo(note('b.md')); await settle();
    expect(order).toEqual(['draft:a.md']); expect(session.state.path).toBe('a.md');
    pending.resolve(); expect(await switching).toBe(true); expect(order).toEqual(['draft:a.md', 'save:a.md']);
    session.edit('b'); expect(await session.close()).toBe(true); expect(order.at(-1)).toBe('save:b.md'); expect(session.state.path).toBeNull();
  });

  it('pauses conflicts without discarding local text and blocks switch/close until explicit resolution', async () => {
    let attempts = 0;
    const session = new EditorSession({ save: async () => { attempts++; throw { code: 'E_EXTERNAL_MODIFIED', message: 'changed', detail: { diskHash: 'disk' } }; }, draft: async () => {} });
    await session.switchTo(note()); session.edit('local'); await session.save();
    expect(session.state.paused).toBe(true); expect(session.state.conflict?.diskHash).toBe('disk');
    session.edit('still local'); session.tick(20000); await settle();
    expect(await session.switchTo(note('b.md'))).toBe(false); expect(await session.close()).toBe(false);
    expect(attempts).toBe(1); expect(session.state.body).toBe('still local');
  });

  it('reloads clean external changes but pauses dirty changes and reports deletion', async () => {
    let external = note('a.md', 'disk', 'new');
    let deleted = false;
    const session = new EditorSession({ save: async s => saved(s), draft: async () => {}, probe: async () => { if (deleted) throw { code: 'E_NOT_FOUND', message: 'deleted' }; return external; } });
    await session.switchTo(note()); await session.checkExternal();
    expect(session.state.body).toBe('disk'); expect(session.state.baseHash).toBe('new');
    session.edit('local'); external = note('a.md', 'new disk', 'newer'); await session.checkExternal();
    expect(session.state.body).toBe('local'); expect(session.state.baseHash).toBe('new'); expect(session.state.paused).toBe(true);
    deleted = true; await session.checkExternal(); expect(session.state.externalDeleted).toBe(true); expect(session.state.body).toBe('local');
  });

  it('checks external changes while a periodic draft is in flight instead of starving every 2s probe', async () => {
    let now = 0; const pending = deferred<void>();
    const session = new EditorSession({ now: () => now, save: async s => saved(s), draft: async () => pending.promise, probe: async () => note('a.md', 'disk', 'external') });
    await session.switchTo(note()); session.edit('local'); now = 500; session.tick(); await settle(); await session.checkExternal(); pending.resolve(); await settle();
    expect(session.state.paused).toBe(true); expect(session.state.body).toBe('local');
  });

  it('ignores stale probe responses after a save or a different note opens', async () => {
    const pending = deferred<NoteContent>();
    const session = new EditorSession({ save: async s => saved(s), draft: async () => {}, probe: async () => pending.promise });
    await session.switchTo(note()); const checking = session.checkExternal(); session.edit('local'); await session.save();
    pending.resolve(note('a.md', 'stale', 'stale')); await checking;
    expect(session.state.body).toBe('local'); expect(session.state.paused).toBe(false);
  });

  it('preserves input typed while an explicit conflict resolution is pending', async () => {
    const pending = deferred<NoteContent>();
    const session = new EditorSession({ save: async () => { throw { code: 'E_EXTERNAL_MODIFIED', detail: { diskHash: 'disk' } }; }, draft: async () => {}, resolve: async () => pending.promise });
    await session.switchTo(note()); session.edit('local'); await session.save(); const resolving = session.resolveConflict('copy'); await settle(); session.edit('new input');
    pending.resolve(note('copy.md', 'local', 'copy-hash')); expect(await resolving).toBe(true);
    expect(session.state.path).toBe('copy.md'); expect(session.state.body).toBe('new input'); expect(session.state.dirty).toBe(true);
  });

  it('serializes auxiliary writes and preserves typing during metadata mutation with explicit pause', async () => {
    const pending = deferred<NoteContent>();
    const order: string[] = [];
    const session = new EditorSession({ save: async s => { order.push('save'); return saved(s); }, draft: async () => {} });
    await session.switchTo(note()); session.edit('first');
    const mutation = session.mutate(async () => { order.push('metadata'); return pending.promise; }); await settle();
    session.edit('later'); pending.resolve(note('a.md', 'metadata body', 'metadata-hash')); expect(await mutation).toBe(false);
    expect(order).toEqual(['save', 'metadata']); expect(session.state.body).toBe('later'); expect(session.state.paused).toBe(true); expect(session.state.baseHash).toBe('metadata-hash');
    expect(await session.write(async s => s.path)).toBe('a.md');
  });

  it('does not save read-only notes and retains dirty state after disk errors', async () => {
    let writes = 0;
    const session = new EditorSession({ save: async () => { writes++; throw new Error('disk full'); }, draft: async () => {} });
    await session.switchTo({ ...note(), readOnly: true }); session.edit('not allowed'); session.tick(20000); await session.flush(); expect(writes).toBe(0); expect(session.state.body).toBe('');
    await session.switchTo(note()); session.edit('keep'); expect(await session.flush()).toBe(false); expect(session.state.dirty).toBe(true); expect(session.state.body).toBe('keep');
  });
});

describe('UTF-8 mention replacement', () => {
  it('replaces byte offsets without corrupting Chinese or emoji around a mention', () => {
    const text = '前😀 目标 后'; const encoder = new TextEncoder(); const start = encoder.encode('前😀 ').length;
    expect(replaceUtf8Range(text, start, start + encoder.encode('目标').length, '[[目标]]')).toBe('前😀 [[目标]] 后');
    expect(() => replaceUtf8Range(text, 1, 3, 'x')).toThrow();
  });
});

describe('bridge identity and structured errors', () => {
  it('uses one random client identity per window and preserves request/payload sequence on note operations', async () => {
    wire.requests.length = 0; wire.failure = false;
    for (const command of ['note_read', 'note_save', 'note_create', 'note_rename', 'note_delete', 'note_restore', 'note_list'] as const) await call(command, { path: 'a.md' });
    const ids = new Set(wire.requests.map(r => r.payload.clientId)); expect(ids.size).toBe(1); expect([...ids][0]).toMatch(/^[0-9a-f-]{36}$/);
    for (const r of wire.requests) expect(r.payload.clientSeq).toBe(r.clientSeq);
    expect(new Set(wire.requests.map(r => r.clientSeq)).size).toBe(7);
  });
  it('exports AppCallError with the original code and detail', async () => {
    wire.failure = true;
    try { await call('note_save'); throw new Error('expected failure'); } catch (error) { expect(error).toBeInstanceOf(AppCallError); expect((error as AppCallError).code).toBe('E_EXTERNAL_MODIFIED'); expect((error as AppCallError).detail).toEqual({ diskHash: 'disk' }); }
    wire.failure = false;
  });
});

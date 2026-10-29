import type { NoteContent, SaveResult } from './contracts';

export interface SessionSnapshot { path: string; body: string; baseHash: string; revision: number; expectedMtimeMs: number }
export interface SessionFailure { code?: string; message: string; detail: Record<string, unknown> }
export interface SessionState {
  path: string | null; body: string; baseHash: string; revision: number; committedRevision: number;
  dirty: boolean; saving: boolean; paused: boolean; externalDeleted: boolean;
  conflict: { diskHash: string; message: string } | null; error: SessionFailure | null;
  note: NoteContent | null; lastSave: SaveResult | null;
}
interface Options {
  save(snapshot: SessionSnapshot): Promise<SaveResult>;
  draft(snapshot: SessionSnapshot): Promise<unknown>;
  probe?(path: string): Promise<NoteContent>;
  reload?(path: string): Promise<NoteContent>;
  resolve?(snapshot: SessionSnapshot & { choice: 'local' | 'disk' | 'copy'; diskHash: string }): Promise<NoteContent>;
  now?(): number;
  onChange?(state: SessionState): void;
}

/** No timers or DOM: the host supplies time and drives tick(). All writes share one queue. */
export class EditorSession {
  private note: NoteContent | null = null;
  private body = '';
  private revision = 0;
  private committed = 0;
  private firstDirtyAt: number | null = null;
  private afterSnapshotAt: number | null = null;
  private submittedRevision = -1;
  private lastInputAt = 0;
  private lastDraftAt = 0;
  private draftRevision = -1;
  private paused = false;
  private externalDeleted = false;
  private conflict: SessionState['conflict'] = null;
  private error: SessionFailure | null = null;
  private lastSave: SaveResult | null = null;
  private tail: Promise<void> = Promise.resolve();
  private saveJob: Promise<boolean> | null = null;
  private draftJob: Promise<void> | null = null;
  private probeJob: Promise<void> | null = null;
  private resolving = false;
  private generation = 0;
  constructor(private readonly options: Options) {}
  private now() { return this.options.now?.() ?? Date.now(); }
  get state(): SessionState {
    return { path: this.note?.path ?? null, body: this.body, baseHash: this.note?.contentHash ?? '', revision: this.revision, committedRevision: this.committed, dirty: this.revision !== this.committed, saving: !!this.saveJob || this.resolving, paused: this.paused, externalDeleted: this.externalDeleted, conflict: this.conflict, error: this.error, note: this.note, lastSave: this.lastSave };
  }
  private emit() { this.options.onChange?.(this.state); }
  private snapshot(): SessionSnapshot {
    return { path: this.note!.path, body: this.body, baseHash: this.note!.contentHash, revision: this.revision, expectedMtimeMs: this.note!.mtimeMs };
  }
  private queue<T>(operation: () => Promise<T>): Promise<T> {
    const job = this.tail.then(operation);
    this.tail = job.then(() => {}, () => {});
    return job;
  }
  private load(note: NoteContent | null) {
    this.generation++; this.note = note; this.body = note?.body ?? ''; this.revision = this.committed = 0;
    this.firstDirtyAt = this.afterSnapshotAt = null; this.lastInputAt = this.lastDraftAt = this.now(); this.draftRevision = -1;
    this.paused = !!note?.conflictPaused; this.externalDeleted = false; this.error = null; this.lastSave = null;
    this.conflict = this.paused ? { diskHash: note!.contentHash, message: '此笔记有待处理冲突，请明确选择处理方式。' } : null;
    this.emit();
  }
  async switchTo(note: NoteContent): Promise<boolean> {
    if (!await this.flush()) return false;
    this.load(note); return true;
  }
  async close(): Promise<boolean> {
    if (!await this.flush()) return false;
    this.load(null); return true;
  }
  edit(body: string, now = this.now()) {
    if (!this.note || this.note.readOnly || body === this.body) return;
    if (this.firstDirtyAt === null) this.firstDirtyAt = now;
    if (this.submittedRevision === this.revision && this.afterSnapshotAt === null) this.afterSnapshotAt = now;
    this.body = body; this.revision++; this.lastInputAt = now; this.emit();
  }
  tick(now = this.now()) {
    if (!this.note || this.note.readOnly || !this.state.dirty || this.resolving) return;
    // Drafts keep running while conflict-paused, preserving further local edits.
    if (now - this.lastDraftAt >= 500 && !this.draftJob && this.draftRevision !== this.revision) {
      this.lastDraftAt = now;
      this.draftJob = this.queue(async () => {
        if (!this.note || !this.state.dirty) return;
        const snapshot = this.snapshot();
        try { await this.options.draft(snapshot); this.draftRevision = snapshot.revision; }
        catch (error) { this.reportError(error); }
      }).finally(() => { this.draftJob = null; });
    }
    if (!this.paused && !this.saveJob && (now - this.lastInputAt >= 2000 || (this.firstDirtyAt !== null && now - this.firstDirtyAt >= 10000))) void this.save();
  }
  save(): Promise<boolean> {
    if (this.saveJob) return this.saveJob;
    if (this.paused) return Promise.resolve(false);
    if (!this.note || this.note.readOnly || !this.state.dirty) return Promise.resolve(true);
    this.saveJob = this.queue(async () => {
      if (this.paused) return false;
      if (!this.note || !this.state.dirty) return true;
      const snapshot = this.snapshot(); this.submittedRevision = snapshot.revision; this.afterSnapshotAt = null;
      try {
        const result = await this.options.save(snapshot);
        this.note = { ...this.note, body: snapshot.body, contentHash: result.contentHash, mtimeMs: result.mtimeMs, sizeBytes: result.bytesWritten };
        this.committed = snapshot.revision; this.lastSave = result; this.error = null;
        this.firstDirtyAt = this.state.dirty ? this.afterSnapshotAt ?? this.lastInputAt : null;
        return true;
      } catch (error) { this.reportError(error); return false; }
      finally { this.submittedRevision = -1; }
    }).finally(() => { this.saveJob = null; this.emit(); });
    this.emit(); return this.saveJob;
  }
  async flush(): Promise<boolean> {
    await this.tail;
    if (this.paused) return false;
    while (this.state.dirty && !this.note?.readOnly) { if (!await this.save()) return false; }
    return true;
  }
  write<T>(operation: (snapshot: SessionSnapshot) => Promise<T>): Promise<T> {
    return this.queue(async () => {
      if (!this.note || this.note.readOnly) throw new Error('请先打开可编辑的笔记。');
      return operation(this.snapshot());
    });
  }
  async mutate(operation: (snapshot: SessionSnapshot) => Promise<NoteContent>): Promise<boolean> {
    if (!await this.flush() || !this.note || this.note.readOnly || this.resolving) return false;
    this.resolving = true; this.emit();
    try {
      return await this.write(async snapshot => {
        try {
          const next = await operation(snapshot);
          if (snapshot.revision === this.revision) { this.load(next); return true; }
          this.note = next; this.paused = true;
          this.conflict = { diskHash: next.contentHash, message: '属性已保存，期间输入仍保留；请明确选择如何合并这两份内容。' };
          this.emit(); return false;
        } catch (error) { this.reportError(error); return false; }
      });
    } finally { this.resolving = false; this.emit(); }
  }
  reportError(error: unknown) {
    const e = error as { code?: string; message?: string; detail?: Record<string, unknown> } | null;
    this.error = { code: e?.code, message: e?.message ?? String(error), detail: e?.detail ?? {} };
    if (e?.code === 'E_EXTERNAL_MODIFIED' || e?.code === 'E_STALE_CLIENT') {
      this.paused = true; this.conflict = { diskHash: String(e.detail?.diskHash ?? ''), message: this.error.message };
    }
    if (e?.code === 'E_NOT_FOUND') { this.paused = true; this.externalDeleted = true; }
    this.emit();
  }
  async checkExternal(): Promise<void> {
    if (this.probeJob) return this.probeJob;
    if (!this.options.probe || !this.note || this.state.saving) return;
    const path = this.note.path, baseHash = this.note.contentHash, generation = this.generation;
    this.probeJob = (async () => {
      try {
        const external = await this.options.probe!(path);
        if (generation !== this.generation || this.note?.contentHash !== baseHash || this.state.saving) return;
        if (external.contentHash === baseHash) return;
        if (this.state.dirty || this.paused) {
          this.paused = true; this.externalDeleted = false;
          this.conflict = { diskHash: external.contentHash, message: '磁盘内容已修改，本地内容保留，自动保存已暂停。' }; this.emit();
        } else {
          // probe never changes the backend base hash. A normal read acknowledges reload.
          const next = this.options.reload ? await this.options.reload(path) : external;
          if (generation !== this.generation || this.note?.contentHash !== baseHash) return;
          if (this.state.dirty) { this.paused = true; this.conflict = { diskHash: next.contentHash, message: '重新读取期间有本地输入，请选择冲突处理。' }; this.emit(); }
          else this.load(next);
        }
      } catch (error) { if (generation === this.generation && this.note?.contentHash === baseHash) this.reportError(error); }
    })().finally(() => { this.probeJob = null; });
    return this.probeJob;
  }
  async resolveConflict(choice: 'local' | 'disk' | 'copy'): Promise<boolean> {
    if (!this.options.resolve || !this.note || !this.conflict || this.resolving) return false;
    this.resolving = true; this.emit();
    try {
      return await this.queue(async () => {
        const snapshot = this.snapshot();
        try {
          const next = await this.options.resolve!({ ...snapshot, choice, diskHash: this.conflict!.diskHash });
          const laterBody = this.body, laterRevision = this.revision, laterInput = this.lastInputAt;
          this.load(next);
          if (laterRevision !== snapshot.revision) { this.body = laterBody; this.revision = laterRevision; this.committed = snapshot.revision; this.firstDirtyAt = this.lastInputAt = laterInput; }
          this.paused = false; this.conflict = null; this.externalDeleted = false; this.error = null; this.emit(); return true;
        } catch (error) { this.reportError(error); return false; }
      });
    } finally { this.resolving = false; this.emit(); }
  }
}

/** Link-service offsets address UTF-8 bytes, never JavaScript UTF-16 positions. */
export function replaceUtf8Range(body: string, start: number, end: number, replacement: string): string {
  const bytes = new TextEncoder().encode(body);
  if (!Number.isInteger(start) || !Number.isInteger(end) || start < 0 || end < start || end > bytes.length || (start < bytes.length && (bytes[start] & 0xc0) === 0x80) || (end < bytes.length && (bytes[end] & 0xc0) === 0x80)) throw new Error('引用位置已失效，请刷新未链接提及。');
  const decoder = new TextDecoder('utf-8', { fatal: true });
  return decoder.decode(bytes.slice(0, start)) + replacement + decoder.decode(bytes.slice(end));
}

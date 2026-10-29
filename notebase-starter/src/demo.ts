// Browser-only memory simulation. Refresh resets every item; production behavior lives in Rust.
import type { AppError, Request, Response, DraftEntry, TrashEntry, RenamePlan, NoteLink } from './contracts';
import { sanitizeTitle } from './filename';
const notes = new Map<string, string>([
  ['欢迎.md', '# 欢迎使用 NoteBase\n\n浏览器演示：仅使用内存数据，刷新即重置，不读写本地磁盘。\n\n试试 [[知识组织]]，或粘贴、拖入附件。\n'],
  ['知识组织.md', '---\ntags: ["知识/笔记"]\naliases: ["知识管理"]\nfavorite: true\n---\n# 知识组织\n\n[[欢迎]] · [[尚未创建]]\n'],
  ['示例.md', '# 示例\n\n前😀 知识组织 后\n\n[[欢迎]]\n'],
  ['templates/每日记录.md', '---\ntags: ["日记"]\n---\n# {{title}}\n\n日期：{{date}} {{time}}\n\n今日记录：\n'],
]);
const drafts = new Map<string, DraftEntry>();
const trash = new Map<string, TrashEntry & { body: string }>();
const attachments = new Map<string, number[]>();
const plans = new Map<string, RenamePlan & { bodies: Map<string, string>; hashes: Map<string, string> }>();
const known = new Map<string, string>();
const paused = new Set<string>();
const encoder = new TextEncoder();
async function hash(body: string) { return 'sha256:' + Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', encoder.encode(body)))).map(x => x.toString(16).padStart(2, '0')).join(''); }
function valid(path: string, empty = false) { return (empty && !path) || (!!path && !path.startsWith('/') && !path.includes('\\') && !path.split('/').some(x => !x || x === '.' || x === '..') && !/^[a-z]:/i.test(path)); }
function titleOf(path: string) { return path.split('/').at(-1)!.replace(/\.md$/i, ''); }
function metadata(body: string): Record<string, unknown> {
  const front = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(body); const result: Record<string, unknown> = {};
  for (const line of front?.[1].split(/\r?\n/) ?? []) { const entry = /^(\w+):\s*(.*)$/.exec(line); if (!entry) continue; try { result[entry[1]] = JSON.parse(entry[2]); } catch { result[entry[1]] = entry[2]; } }
  return result;
}
function patch(body: string, updates: Record<string, unknown>) {
  const front = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(body);
  const lines = front ? front[1].split(/\r?\n/).filter(line => !Object.keys(updates).some(key => line.startsWith(key + ':'))) : [];
  lines.push(...Object.entries(updates).filter(([, value]) => value !== null).map(([key, value]) => key + ': ' + JSON.stringify(value)));
  return '---\n' + lines.join('\n') + '\n---\n' + (front ? body.slice(front[0].length) : body);
}
function links(body: string): NoteLink[] {
  const masked = body.replace(/\x60\x60\x60[^]*?\x60\x60\x60|\x60[^\x60\n]*\x60/g, s => s.replace(/[^\n]/g, ' '));
  return [...masked.matchAll(/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g)].map(m => ({ target: m[1], label: m[2] ?? m[1], start: encoder.encode(body.slice(0, m.index)).length, end: encoder.encode(body.slice(0, m.index! + m[0].length)).length, line: body.slice(0, m.index).split('\n').length, col: m.index! - body.lastIndexOf('\n', m.index) }));
}
function paths() { return [...notes.keys()].filter(p => !p.startsWith('templates/')).sort(); }
function resolution(target: string, source: string) {
  const wanted = target.split('#')[0].replace(/\.md$/i, ''); const dir = source.includes('/') ? source.slice(0, source.lastIndexOf('/') + 1) : '';
  const direct = [dir + wanted + '.md', wanted + '.md'].filter(p => notes.has(p));
  const candidates = [...new Set(direct.length ? direct : paths().filter(p => titleOf(p) === wanted || (metadata(notes.get(p)!).aliases as string[] | undefined)?.includes(wanted)))];
  return { targetPath: candidates.length === 1 ? candidates[0] : null, candidates, ambiguous: candidates.length > 1 };
}
function vacant(dir: string, title: string) { const prefix = dir ? dir + '/' : ''; let path = prefix + title + '.md', i = 2; while (notes.has(path)) path = prefix + title + ' (' + i++ + ').md'; return path; }
async function content(path: string, probe = false, identity = '') {
  const body = notes.get(path)!; const contentHash = await hash(body); if (!probe) known.set(identity, contentHash);
  return { path, title: String(metadata(body).title ?? titleOf(path)), body, frontmatter: metadata(body), frontmatterError: null, contentHash, mtimeMs: 0, sizeBytes: encoder.encode(body).length, encoding: 'utf-8', lineEnding: 'lf', readOnly: false, links: links(body), conflictPaused: paused.has(path) };
}
async function result(path: string, body: string) { return { path, contentHash: await hash(body), mtimeMs: Date.now(), bytesWritten: encoder.encode(body).length, snapshotCreated: false, indexQueued: false, warnings: ['浏览器演示未写入磁盘；刷新后重置'] }; }
export async function demoDispatch<T>(r: Request): Promise<Response<T>> {
  const p = r.payload, path = String(p.path ?? ''), identity = String(p.clientId ?? '') + ':' + path;
  const fail = (code: AppError['code'], message: string, detail: Record<string, unknown> = {}): Response<T> => ({ id: r.id, ok: false, error: { code, message, detail, retryable: false } });
  let data: unknown;
  switch (r.command) {
    case 'vault_open': data = { vaultId: 'demo', noteCount: paths().length }; break;
    case 'vault_info': data = { vaultPath: '浏览器演示库', indexState: 'unavailable', syncState: 'disabled' }; break;
    case 'note_list': {
      if (p.scope === 'drafts') { data = { items: [...drafts.values()] }; break; }
      if (p.scope === 'trash') { data = { items: [...trash.values()].map(({ body: _body, ...entry }) => entry) }; break; }
      if (p.scope === 'renames') { data = { items: [...plans.values()].filter(p => p.state === 'partial').map(({ bodies: _b, hashes: _h, ...plan }) => plan) }; break; }
      const prefix = String(p.dirPrefix ?? ''), after = String(p.cursor ?? ''), limit = Math.max(1, Math.min(500, Number(p.limit) || 500));
      const entries = [...notes.keys()].sort().filter(path => path > after && (p.scope === 'templates' ? path.startsWith('templates/') : !path.startsWith('templates/')) && (!prefix || path.startsWith(prefix + '/'))).map(path => ({ path, title: titleOf(path), mtimeMs: 0, hasAttachment: notes.get(path)!.includes('attachments/'), favorite: metadata(notes.get(path)!).favorite === true, ...metadata(notes.get(path)!) })).filter(n => !p.favorite || n.favorite === true);
      data = { items: entries.slice(0, limit), nextCursor: entries.length > limit ? entries[limit - 1].path : null }; break;
    }
    case 'note_create': {
      let title: string; try { title = sanitizeTitle(String(p.title ?? '')); } catch (e) { return fail('E_PATH_INVALID', String(e)); }
      const dir = String(p.dir ?? ''); if (!valid(dir, true)) return fail('E_PATH_INVALID', '目录不合法');
      const target = vacant(dir, title); let body = String(p.body ?? '');
      if (p.templatePath) { const template = notes.get(String(p.templatePath)); if (!template || !String(p.templatePath).startsWith('templates/')) return fail('E_NOT_FOUND', '模板不存在'); const stamp = new Date().toISOString(); body = template.replaceAll('{{title}}', String(p.title)).replaceAll('{{date}}', stamp.slice(0, 10)).replaceAll('{{time}}', stamp.slice(11, 19)); }
      notes.set(target, body); data = { path: target }; break;
    }
    case 'note_read': if (!notes.has(path)) return fail('E_NOT_FOUND', '笔记不存在'); data = await content(path, p.probe === true, identity); break;
    case 'note_save': {
      const old = notes.get(path), action = String(p.action ?? 'save');
      if (action === 'attachment') {
        if (!notes.has(path)) return fail('E_NOT_FOUND', '笔记不存在');
        const bytes = p.bytes; if (!Array.isArray(bytes) || bytes.some(n => !Number.isInteger(n) || n < 0 || n > 255)) return fail('E_INTERNAL', '附件字节无效'); if (bytes.length > 32 * 1024 * 1024) return fail('E_FILE_TOO_LARGE', '附件超过32 MiB');
        const dir = String(p.directory ?? 'attachments'); if (!valid(dir)) return fail('E_PATH_INVALID', '附件目录不合法');
        const name = sanitizeTitle(String(p.name ?? '附件')); let target = dir + '/' + name, i = 2; while (attachments.has(target)) target = dir + '/' + i++ + '-' + name; attachments.set(target, bytes);
        const relativePath = '../'.repeat(path.split('/').length - 1) + target;
        data = { attachmentPath: target, relativePath, markdown: (p.image ? '!' : '') + '[' + String(p.name).replace(/[\[\]]/g, '_') + '](' + relativePath.split('/').map(encodeURIComponent).join('/') + ')' }; break;
      }
      if (action === 'draft') {
        if (known.get(identity) !== p.baseHash) return fail('E_STALE_CLIENT', '草稿基于旧版本');
        const entry: DraftEntry = { id: identity, path, body: String(p.body), baseHash: String(p.baseHash), clientId: String(p.clientId), clientSeq: Number(p.clientSeq), savedAtMs: Date.now() }; drafts.set(identity, entry); data = { draftSaved: true }; break;
      }
      if (old === undefined) return fail('E_NOT_FOUND', '笔记不存在'); const diskHash = await hash(old);
      if (action === 'conflict') {
        if (p.diskHash !== diskHash) return fail('E_EXTERNAL_MODIFIED', '磁盘再次变化', { diskHash });
        if (p.choice === 'disk') { paused.delete(path); drafts.delete(identity); data = await content(path, false, identity); break; }
        if (p.choice === 'copy') { const target = vacant(path.split('/').slice(0, -1).join('/'), titleOf(path) + ' conflict'); notes.set(target, String(p.body)); paused.delete(path); drafts.delete(identity); data = { path: target, copied: true }; break; }
        if (p.choice !== 'local') return fail('E_INTERNAL', '未知冲突选项'); paused.delete(path);
      } else if (paused.has(path) || p.baseHash !== diskHash) { paused.add(path); return fail('E_EXTERNAL_MODIFIED', '内容已变更，请明确处理冲突', { diskHash }); }
      const body = action === 'metadata' ? patch(old, p.updates as Record<string, unknown>) : String(p.body);
      notes.set(path, body); known.set(identity, await hash(body)); drafts.delete(identity); data = await result(path, body); break;
    }
    case 'note_delete': {
      if (p.permanent) { if (p.confirmed !== true || !trash.has(path)) return fail('E_PATH_INVALID', '彻底删除需要确认回收站项目'); trash.delete(path); data = { deleted: true }; break; }
      const body = notes.get(path); if (body === undefined) return fail('E_NOT_FOUND', '笔记不存在'); const trashPath = 'trash/' + crypto.randomUUID() + '/' + path;
      trash.set(trashPath, { trashPath, originalPath: path, deletedAtMs: Date.now(), body }); notes.delete(path); data = { trashPath }; break;
    }
    case 'note_restore': {
      if (p.kind === 'draft') {
        const d = drafts.get(String(p.draftId)); if (!d) return fail('E_NOT_FOUND', '草稿不存在'); if (p.choice === 'discard') { drafts.delete(d.id); data = { discarded: true }; break; }
        const same = notes.has(d.path) && await hash(notes.get(d.path)!) === d.baseHash && p.choice !== 'copy';
        const target = same ? d.path : vacant(d.path.split('/').slice(0, -1).join('/'), titleOf(d.path) + ' recovered'); notes.set(target, d.body); drafts.delete(d.id); data = { path: target, recovered: true, copied: target !== d.path }; break;
      }
      const entry = trash.get(String(p.trashPath)); if (!entry) return fail('E_NOT_FOUND', '回收记录不存在'); const target = vacant(entry.originalPath.split('/').slice(0, -1).join('/'), titleOf(entry.originalPath)); notes.set(target, entry.body); trash.delete(entry.trashPath); data = { path: target }; break;
    }
    case 'note_rename': {
      if (p.planOnly === true) {
        if (!notes.has(path)) return fail('E_NOT_FOUND', '笔记不存在'); const to = (path.includes('/') ? path.slice(0, path.lastIndexOf('/') + 1) : '') + sanitizeTitle(String(p.newName).replace(/\.md$/i, '')) + '.md'; if (notes.has(to) && to !== path) return fail('E_PATH_INVALID', '目标已存在');
        const bodies = new Map<string, string>(), hashes = new Map<string, string>(); let totalReplacements = 0;
        for (const [source, body] of notes) { let next = body; for (const link of links(body).reverse()) { if (resolution(link.target, source).targetPath !== path) continue; const target = link.target.includes('/') ? to.replace(/\.md$/, '') : titleOf(to); const raw = encoder.encode(next), decoder = new TextDecoder(); next = decoder.decode(raw.slice(0, link.start)) + '[[' + target + (link.label !== link.target ? '|' + link.label : '') + ']]' + decoder.decode(raw.slice(link.end)); totalReplacements++; } if (next !== body || source === path) { bodies.set(source, next); hashes.set(source, await hash(body)); } }
        const plan = { id: crypto.randomUUID(), from: path, to, affectedFiles: [...bodies].map(([path, preview]) => ({ path, preview })), totalReplacements, state: 'planned', bodies, hashes }; plans.set(plan.id, plan); const { bodies: _b, hashes: _h, ...publicPlan } = plan; data = { plan: publicPlan }; break;
      }
      const plan = plans.get(String(p.resumeId ?? p.planId)); if (!plan) return fail('E_NOT_FOUND', '重命名计划不存在');
      for (const [source, expected] of plan.hashes) if (!notes.has(source) || await hash(notes.get(source)!) !== expected) return fail('E_EXTERNAL_MODIFIED', '预览后文件已变化');
      for (const [source, body] of plan.bodies) notes.set(source === plan.from ? plan.to : source, body); if (plan.from !== plan.to) notes.delete(plan.from); plan.state = 'completed'; data = { path: plan.to, completed: true }; break;
    }
    case 'link_backlinks': {
      if (p.resolveTarget !== undefined) { data = resolution(String(p.resolveTarget), path); break; }
      data = { items: paths().flatMap(source => links(notes.get(source)!).filter(link => resolution(link.target, source).targetPath === path).map(link => ({ sourcePath: source, line: link.line, col: link.col, snippet: notes.get(source)!.split('\n')[link.line - 1], circular: source === path, ambiguous: false, candidates: [path] }))) }; break;
    }
    case 'link_mentions': {
      const targets = [titleOf(path), ...((metadata(notes.get(path) ?? '').aliases as string[]) ?? [])].filter(Boolean);
      const items: { sourcePath: string; text: string; line: number; col: number; start: number; end: number }[] = [];
      for (const sourcePath of paths().filter(source => source !== path)) { const body = notes.get(sourcePath)!, linked = links(body); for (const text of targets) { let index = body.indexOf(text); while (index >= 0) { const start = encoder.encode(body.slice(0, index)).length, end = start + encoder.encode(text).length; if (!linked.some(l => start >= l.start && end <= l.end)) items.push({ sourcePath, text, start, end, line: body.slice(0, index).split('\n').length, col: index - body.lastIndexOf('\n', index) }); index = body.indexOf(text, index + text.length); } } }
      data = { items }; break;
    }
    case 'link_graph': {
      const all = paths().filter(path => !p.pathPrefix || path.startsWith(String(p.pathPrefix))), edges = all.flatMap(from => links(notes.get(from)!).flatMap(l => { const to = resolution(l.target, from).targetPath; return to ? [{ from, to }] : []; }));
      const selected = p.path ? new Set([path, ...edges.filter(e => e.from === path || e.to === path).flatMap(e => [e.from, e.to])]) : new Set(all); const nodes = all.filter(p => selected.has(p)).slice(0, Math.min(500, Number(p.maxNodes) || 500)); data = { nodes: nodes.map(path => ({ path, title: titleOf(path) })), edges: edges.filter(e => nodes.includes(e.from) && nodes.includes(e.to)) }; break;
    }
    default: return fail('E_INTERNAL', '此能力尚未接入浏览器内存演示');
  }
  return { id: r.id, ok: true, data: data as T };
}

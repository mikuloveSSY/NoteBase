import type { Command, NoteContent, RenderedNote } from './contracts';

type ViewMode = 'edit' | 'split' | 'read';
const preferenceKey = 'notebase-markdown-view';
export function mountMarkdownPreview(c: {
  desktop: boolean;
  note: () => NoteContent | null;
  body: () => string;
  call: <T>(command: Command, payload?: Record<string, unknown>) => Promise<T>;
}) {
  let mode: ViewMode = c.desktop ? 'split' : 'edit';
  try {
    const saved = localStorage.getItem(preferenceKey);
    if (c.desktop && (saved === 'edit' || saved === 'split' || saved === 'read')) mode = saved;
  } catch { /* An unavailable preference store must not block editing. */ }
  const editor = document.getElementById('editor')!;
  const layout = document.createElement('div');
  layout.className = 'editor-layout';
  editor.replaceWith(layout);
  layout.append(editor);
  const pane = document.createElement('section');
  pane.className = 'preview-pane';
  pane.setAttribute('aria-label', 'Markdown 预览面板');
  const status = document.createElement('p');
  status.id = 'previewStatus'; status.className = 'preview-status'; status.setAttribute('role', 'status');
  const preview = document.createElement('iframe');
  preview.id = 'markdownPreview'; preview.title = 'Markdown 预览';
  preview.setAttribute('sandbox', ''); preview.referrerPolicy = 'no-referrer'; preview.hidden = true;
  pane.append(status, preview); layout.append(pane);
  const bar = document.querySelector('.editor-bar')!;
  bar.innerHTML = '<div class="preview-modes" role="group" aria-label="Markdown 视图"><button id="previewEdit" type="button">编辑</button><button id="previewToggle" type="button">分栏预览</button><button id="previewRead" type="button">阅读</button></div><span>500ms 草稿 · 2s 停输入 · 10s 强制保存</span>';
  let revision = 0, path: string | undefined, renderedBody: string | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const buttons = { edit: 'previewEdit', split: 'previewToggle', read: 'previewRead' } as const;
  function applyMode() {
    editor.hidden = mode === 'read'; pane.hidden = mode === 'edit';
    layout.dataset.view = mode;
    for (const [value, id] of Object.entries(buttons)) {
      const button = document.getElementById(id) as HTMLButtonElement;
      button.setAttribute('aria-pressed', String(value === mode));
      button.disabled = !c.desktop;
    }
  }
  function clear() { preview.srcdoc = ''; preview.hidden = true; renderedBody = undefined; }
  const previewVisible = () => mode !== 'edit';
  async function render() {
    const note = c.note(), rev = ++revision;
    if (mode === 'edit' || !c.desktop || !note) return;
    const body = c.body();
    if (renderedBody === body && path === note.path) return;
    status.textContent = '正在更新预览…'; status.classList.remove('error');
    try {
      const result = await c.call<RenderedNote>('export_note', { path: note.path, format: 'html', preview: true, body });
      if (rev !== revision || !previewVisible() || c.note()?.path !== note.path || c.body() !== body) return;
      preview.srcdoc = result.html; preview.hidden = false; renderedBody = body;
      status.textContent = result.missingAssets.length ? '缺失图片：' + result.missingAssets.join('、') : 'Markdown 预览 · ' + note.path;
      if (result.warnings?.length) status.textContent += '；' + result.warnings.length + ' 项链接提示';
    } catch (error) {
      if (rev !== revision || !previewVisible()) return;
      clear(); status.classList.add('error');
      status.textContent = '预览失败：' + String((error as { message?: string })?.message ?? error) + '。可切换到编辑继续写作。';
    }
  }
  function refresh() {
    clearTimeout(timer); revision++;
    const nextPath = c.note()?.path;
    if (!nextPath || nextPath !== path) { clear(); status.classList.remove('error'); }
    path = nextPath;
    if (!nextPath) status.textContent = '打开一篇 Markdown 笔记即可预览。';
    if (mode !== 'edit' && c.desktop && nextPath) timer = setTimeout(() => void render(), 150);
  }
  for (const [value, id] of Object.entries(buttons)) {
    document.getElementById(id)!.addEventListener('click', () => {
      mode = value as ViewMode; applyMode();
      renderedBody = undefined;
      try { localStorage.setItem(preferenceKey, mode); } catch { /* Optional preference. */ }
      refresh();
    });
  }
  window.addEventListener('notebase:editor-change', refresh);
  applyMode(); refresh();
  return { refresh };
}

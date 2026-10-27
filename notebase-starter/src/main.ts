import { EditorView, basicSetup } from 'codemirror';
import { markdown } from '@codemirror/lang-markdown';
import { Compartment } from '@codemirror/state';
import { call, desktopMode } from './bridge';
import type { NoteContent, NoteEntry, SaveResult } from './contracts';
import './style.css';
document.querySelector<HTMLDivElement>('#app')!.innerHTML=`
<aside><div class="brand"><b>N</b><div>NoteBase<small>本地优先 · Markdown</small></div></div>
<label class="field">笔记库目录<input id="vaultPath" placeholder="例如 D:\\Notes" aria-label="笔记库绝对路径"></label>
<button id="open">打开已有文件夹</button><div class="section-title">笔记 <button id="new" aria-label="新建笔记">＋ 新建</button></div>
<nav id="notes" aria-label="笔记列表"></nav><div class="sidebar-foot">文件是唯一数据源<br>主干版本 0.1.0</div></aside>
<main><header><div><span class="eyebrow">工作空间</span><h1 id="title">选择或新建一篇笔记</h1><div id="path" class="muted">从左侧打开已有的 Markdown 文件夹</div></div><div class="actions"><span id="dirty">未打开</span><button id="save" class="primary" disabled>保存 Ctrl+S</button></div></header>
<div id="mode" class="notice"></div><div class="editor-bar"><span>Markdown 源文</span><span>CodeMirror 6 · 手动保存</span></div><section id="editor" aria-label="Markdown 编辑器"></section>
<footer><span id="status" role="status" aria-live="polite">准备就绪</span><span id="count">0 字符</span></footer></main>
<aside class="right"><h2>工程进度</h2><p>先打通最小闭环，再并行扩展模块。</p><div class="feature done">笔记库与文件列表</div><div class="feature done">新建 · 读取 · 手动保存</div><div class="feature">自动保存与崩溃恢复 待接入</div><div class="feature">全文检索与链接 待接入</div><div class="feature">渲染与导出 待接入</div><div class="feature">备份与同步 待接入</div><h2>四人接入位置</h2><p>① 存储与检索<br>② 编辑与知识组织<br>③ 导出与数据保障<br>④ 界面与集成</p><p class="muted">详细入口和交接要求见工程 docs 目录。</p></aside>`;
const $=<T extends HTMLElement>(id:string)=>document.getElementById(id) as T;
let active:NoteContent|null=null,dirty=false,busy=false,opened=false,loading=false;
const editable=new Compartment();
const view=new EditorView({parent:$('editor'),extensions:[basicSetup,markdown(),editable.of(EditorView.editable.of(false)),EditorView.updateListener.of(u=>{if(u.docChanged&&!loading){dirty=true;update();}})]});
function update(){
 $('dirty').textContent=active?(dirty?'有未保存修改':'已保存'):'未打开';
 $<HTMLButtonElement>('save').disabled=!active||!dirty||busy||!!active?.readOnly;
 $<HTMLButtonElement>('new').disabled=!opened||busy;
 $<HTMLButtonElement>('open').disabled=busy;
 $<HTMLInputElement>('vaultPath').disabled=busy||!desktopMode;
 $('count').textContent=`${view.state.doc.length} 字符`;
 view.dispatch({effects:editable.reconfigure(EditorView.editable.of(!!active&&!active.readOnly&&!busy))});
}
function status(text:string,error=false){$('status').textContent=text;$('status').classList.toggle('error',error);}
async function run(fn:()=>Promise<void>){if(busy)return;busy=true;update();try{await fn();}catch(e){status(e instanceof Error?e.message:String(e),true);}finally{busy=false;update();}}
function discard(){return !dirty||confirm('当前笔记有未保存修改，是否放弃修改？');}
async function list(){const result=await call<{items:NoteEntry[]}>('note_list',{dirPrefix:'',limit:500,cursor:null});const nav=$('notes');nav.replaceChildren();for(const n of result.items){const b=document.createElement('button');b.className='note'+(active?.path===n.path?' selected':'');b.textContent=n.path;b.title=n.path;b.addEventListener('click',()=>{if(busy||!discard())return;void run(()=>read(n.path));});nav.append(b);}if(!result.items.length)nav.textContent='还没有笔记，点击新建开始。';}
async function read(path:string){const next=await call<NoteContent>('note_read',{path,includeRaw:true});active=next;loading=true;view.dispatch({changes:{from:0,to:view.state.doc.length,insert:next.body}});loading=false;dirty=false;$('title').textContent=next.title;$('path').textContent=next.path;await list();status(next.readOnly?'文件较大，以只读方式打开':'已打开 '+path);}
async function open(){if(!discard())return;await run(async()=>{await call('vault_open',{path:$<HTMLInputElement>('vaultPath').value});opened=true;active=null;dirty=false;loading=true;view.dispatch({changes:{from:0,to:view.state.doc.length,insert:''}});loading=false;$('title').textContent='选择或新建一篇笔记';$('path').textContent='Markdown 文件库';await list();status('笔记库已打开');});}
async function save(){if(busy||!active||!dirty)return;await run(async()=>{const n=active!,result=await call<SaveResult>('note_save',{path:n.path,body:view.state.doc.toString(),baseHash:n.contentHash,expectedMtimeMs:n.mtimeMs,clientSeq:Date.now()});active={...n,contentHash:result.contentHash,mtimeMs:result.mtimeMs};dirty=false;status(result.warnings.join('；')||'已写入磁盘');});}
const mode=$('mode');mode.textContent=desktopMode?'桌面模式：写入所选文件夹中的真实 .md 文件。基础版尚无自动保存，请主动保存。':'浏览器演示：仅使用内存数据，刷新即重置，不读写本地磁盘。';
$('open').addEventListener('click',()=>void open());$('save').addEventListener('click',()=>void save());
$('new').addEventListener('click',()=>{if(busy||!discard())return;const title=prompt('笔记标题');if(title===null)return;void run(async()=>{const r=await call<{path:string}>('note_create',{dir:'',title});await read(r.path);});});
document.addEventListener('keydown',e=>{if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='s'){e.preventDefault();void save();}});
window.addEventListener('beforeunload',e=>{if(dirty){e.preventDefault();}});
// Tauri 原生窗口关闭不会依赖浏览器 beforeunload。
if(desktopMode){void import('@tauri-apps/api/window').then(({getCurrentWindow})=>getCurrentWindow().onCloseRequested(e=>{if(busy||(!discard()))e.preventDefault();}));}
update();if(!desktopMode)void open();

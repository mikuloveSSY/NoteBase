import { EditorView, basicSetup } from 'codemirror';
import { markdown } from '@codemirror/lang-markdown';
import { Compartment, EditorState } from '@codemirror/state';
import { call, desktopMode } from './bridge';
import type { NoteContent, NoteEntry, SaveResult, IndexStatus } from './contracts';
import { transportEventName } from './contracts';
import { version } from '../package.json';
import './style.css';
document.querySelector<HTMLDivElement>('#app')!.innerHTML=`
<aside><div class="brand"><b>N</b><div>NoteBase<small>本地优先 · Markdown</small></div></div>
<label class="field">笔记库目录<input id="vaultPath" placeholder="例如 D:\\Notes" aria-label="笔记库绝对路径"></label>
<button id="open">打开已有文件夹</button><div class="section-title">笔记 <button id="new" aria-label="新建笔记">＋ 新建</button></div>
<label class="field">搜索笔记<input id="searchQuery" aria-label="搜索笔记" placeholder='关键词、title:标题、tag:标签'></label><button id="searchButton">搜索</button><div id="searchInfo" role="status"></div><nav id="searchResults" aria-label="搜索结果"></nav><button id="searchMore" hidden>更多搜索结果</button>
<nav id="notes" aria-label="笔记列表"></nav><button id="more" hidden>加载更多笔记</button><button id="refresh">刷新文件列表</button><div class="sidebar-foot">文件是唯一数据源<br>主干版本 ${version}</div></aside>
<main><header><div><span class="eyebrow">工作空间</span><h1 id="title">选择或新建一篇笔记</h1><div id="path" class="muted">从左侧打开已有的 Markdown 文件夹</div></div><div class="actions"><span id="dirty">未打开</span><button id="save" class="primary" disabled>保存 Ctrl+S</button></div></header>
<div id="mode" class="notice"></div><div id="readNotice" class="notice" role="status" hidden></div><div class="editor-bar"><span>Markdown 源文</span><span>CodeMirror 6 · 手动保存</span></div><section id="editor" aria-label="Markdown 编辑器"></section>
<footer><span id="status" role="status" aria-live="polite">准备就绪</span><span id="count">0 字符</span></footer></main>
<aside class="right"><h2>索引状态</h2><p id="indexStatus" role="status">打开笔记库后建立索引</p><p class="muted">索引在笔记库外保存。保存和外部文件变化会自动更新索引；监听异常时每 300 秒对账。可通过左侧输入框检索笔记。</p><h2>工程进度</h2><div class="feature done">笔记库与文件列表</div><div class="feature done">新建 · 读取 · 手动保存</div><div class="feature">自动保存与崩溃恢复 待接入</div><div class="feature">全文检索已接入 · 链接待接入</div><div class="feature">渲染与导出 待接入</div><div class="feature">备份与同步 待接入</div><h2>四人接入位置</h2><p>① 存储与检索<br>② 编辑与知识组织<br>③ 导出与数据保障<br>④ 界面与集成</p><p class="muted">详细入口和交接要求见工程 docs 目录。</p></aside>`;
const $=<T extends HTMLElement>(id:string)=>document.getElementById(id) as T;
let active:NoteContent|null=null,dirty=false,busy=false,opened=false,loading=false;
let nextCursor:string|null=null;
const editable=new Compartment();
const view=new EditorView({parent:$('editor'),extensions:[basicSetup,markdown(),editable.of([EditorView.editable.of(false),EditorState.readOnly.of(true)]),EditorView.updateListener.of(u=>{if(u.docChanged&&!loading){dirty=true;update();}})]});
function update(){
 $('dirty').textContent=active?(active.readOnly?'只读':dirty?'有未保存修改':'已保存'):'未打开';
 $<HTMLButtonElement>('save').disabled=!active||!dirty||busy||!!active?.readOnly;
 $<HTMLButtonElement>('new').disabled=!opened||busy;
 $<HTMLButtonElement>('more').disabled=!opened||busy;
 $<HTMLButtonElement>('refresh').disabled=!opened||busy;
 $<HTMLButtonElement>('open').disabled=busy;
 $<HTMLInputElement>('vaultPath').disabled=busy||!desktopMode;
 $('count').textContent=`${view.state.doc.length} 字符`;
 const canEdit=!!active&&!active.readOnly&&!busy;
 view.dispatch({effects:editable.reconfigure([EditorView.editable.of(canEdit),EditorState.readOnly.of(!canEdit)])});
}
function status(text:string,error=false){$('status').textContent=text;$('status').classList.toggle('error',error);}
async function run(fn:()=>Promise<void>){if(busy)return;busy=true;update();try{await fn();}catch(e){status(e instanceof Error?e.message:String(e),true);}finally{busy=false;update();}}
function discard(){return !dirty||confirm('当前笔记有未保存修改，是否放弃修改？');}
function markSelected(){$('notes').querySelectorAll<HTMLButtonElement>('button').forEach(button=>button.classList.toggle('selected',button.dataset.path===active?.path));}
async function list(reset=true){
 const result=await call<{items:NoteEntry[];nextCursor?:string|null}>('note_list',{dirPrefix:'',limit:500,cursor:reset?null:nextCursor});
 const nav=$('notes');if(reset)nav.replaceChildren();
 for(const n of result.items){const b=document.createElement('button');b.className='note';b.textContent=n.path;b.title=n.path;b.dataset.path=n.path;b.addEventListener('click',()=>{if(busy||!discard())return;void run(()=>read(n.path));});nav.append(b);}
 nextCursor=result.nextCursor??null;$('more').hidden=nextCursor===null;
 if(!nav.childElementCount)nav.textContent='还没有笔记，点击新建开始。';markSelected();
}
function showReadNotice(note:NoteContent){
 const messages:string[]=[];
 if(note.sizeBytes>10*1024*1024)messages.push('文件超过 10 MB，仅预览前 256 KiB，不能编辑或保存；完整内容请使用外部编辑器查看。');
 if(note.encoding!=='utf-8')messages.push('文件包含非法 UTF-8 字节，以替换字符只读展示。原文件未修改，请用外部工具转换为 UTF-8 后重新打开。');
 if(note.readOnly&&!messages.length)messages.push('此文件为只读，不能编辑或保存。');
 if(note.frontmatterError)messages.push('属性解析失败：'+note.frontmatterError+'；原文保持不变。');
 $('readNotice').textContent=messages.join(' ');$('readNotice').hidden=!messages.length;
}
async function read(path:string){const next=await call<NoteContent>('note_read',{path,includeRaw:true});active=next;loading=true;view.dispatch({changes:{from:0,to:view.state.doc.length,insert:next.body}});loading=false;dirty=false;$('title').textContent=next.title;$('path').textContent=next.path;showReadNotice(next);markSelected();status((next.readOnly?'已只读打开 ':'已打开 ')+path);}
async function open(){if(busy||!discard())return;await run(async()=>{await call('vault_open',{path:$<HTMLInputElement>('vaultPath').value});opened=true;document.getElementById("searchResults")!.replaceChildren();document.getElementById("searchInfo")!.textContent='';document.getElementById("searchMore")!.hidden=true;active=null;dirty=false;loading=true;view.dispatch({changes:{from:0,to:view.state.doc.length,insert:''}});loading=false;$('title').textContent='选择或新建一篇笔记';$('path').textContent='Markdown 文件库';$('readNotice').hidden=true;await list();status('笔记库已打开');});}
async function save(){if(busy||!active||!dirty||active.readOnly)return;await run(async()=>{const n=active!,result=await call<SaveResult>('note_save',{path:n.path,body:view.state.doc.toString(),baseHash:n.contentHash,expectedMtimeMs:n.mtimeMs});active={...n,contentHash:result.contentHash,mtimeMs:result.mtimeMs,sizeBytes:result.bytesWritten};dirty=false;status(result.warnings.join('；')||'已写入磁盘');});}
const mode=$('mode');mode.textContent=desktopMode?'桌面模式：写入所选文件夹中的真实 .md 文件。基础版尚无自动保存，请主动保存。':'浏览器演示：仅使用内存数据，刷新即重置，不读写本地磁盘。';
$('open').addEventListener('click',()=>void open());$('save').addEventListener('click',()=>void save());
$('more').addEventListener('click',()=>void run(()=>list(false)));
$('refresh').addEventListener('click',()=>void run(async()=>{await list();status('文件列表已刷新，笔记内容未重新加载');}));
$('new').addEventListener('click',()=>{if(busy||!discard())return;const title=prompt('笔记标题');if(title===null)return;void run(async()=>{const r=await call<{path:string}>('note_create',{dir:'',title});await list();await read(r.path);if(r.path!==title+'.md')status('文件名已修正或避开同名：'+r.path);});});
document.addEventListener('keydown',e=>{if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='s'){e.preventDefault();void save();}});
window.addEventListener('beforeunload',e=>{if(dirty){e.preventDefault();}});
// Tauri 原生窗口关闭不会依赖浏览器 beforeunload。
if(desktopMode){void import('@tauri-apps/api/window').then(({getCurrentWindow})=>getCurrentWindow().onCloseRequested(e=>{if(busy||(!discard()))e.preventDefault();}));}
update();if(!desktopMode)void open();
async function pollIndex(){
 if(!desktopMode){$('indexStatus').textContent='浏览器演示不建立磁盘索引';return;}
 if(opened){try{
  const value=await call<IndexStatus>('index_status',{});
  const labels={ready:'索引已就绪',rebuilding:'正在构建索引',offline:'索引更新中或存在不可索引文件',unavailable:'索引不可用'};
  $('indexStatus').textContent=`${labels[value.state]} · ${value.noteCount} 篇`+(value.progress?` · ${value.progress.done}/${value.progress.total}`:'');
  if(externalTreeDirty&&!busy){externalTreeDirty=false;await run(()=>list());}
 }catch(error){$('indexStatus').textContent='索引不可用：'+(error instanceof Error?error.message:String(error));}}
 window.setTimeout(()=>void pollIndex(),1000);
}
void pollIndex();
let externalTreeDirty=false;
if(desktopMode){void import('@tauri-apps/api/event').then(async({listen})=>{
 await listen<{path:string;source:string;contentHash:string}>(transportEventName('note.changed'),event=>{externalTreeDirty=true;if(active?.path===event.payload.path)status('磁盘文件已变化；当前编辑内容保留，请重新打开并确认。');});
});}
let searchCursor:string|null=null,searchText='';
async function searchNotes(more=false){await run(async()=>{
 if(!desktopMode)throw new Error('全文检索需要打开桌面文件库');
 const query=more?searchText:$<HTMLInputElement>('searchQuery').value;
 const result=await call<{items:{path:string;title:string;snippet:string}[];nextCursor:string|null;totalApprox:number;degraded:boolean}>('search',{query,limit:50,sort:'relevance',cursor:more?searchCursor:null});
 if(!more)$('searchResults').replaceChildren();
 for(const item of result.items){const button=document.createElement('button');button.className='note';button.textContent=item.title+' · '+item.path;button.title=item.snippet.replace(/<\/?b>/g,'');button.addEventListener('click',()=>{if(busy||!discard())return;void run(()=>read(item.path));});$('searchResults').append(button);}
 searchText=query;searchCursor=result.nextCursor;$('searchMore').hidden=!searchCursor;$('searchInfo').textContent=`${result.totalApprox} 条结果`+(result.degraded?' · 短查询扫描':'');
});}
$('searchButton').addEventListener('click',()=>void searchNotes());$('searchMore').addEventListener('click',()=>void searchNotes(true));
$('searchQuery').addEventListener('keydown',event=>{if(event.key==='Enter')void searchNotes();});

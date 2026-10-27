// 仅供浏览器界面联调：内存模拟，不读取用户磁盘，也不伪装真实持久化。
import type { Request, Response } from './contracts';
const notes=new Map<string,string>([['欢迎.md','# 欢迎使用 NoteBase\n\n这是基础工程的界面演示。\n\n- 桌面模式：读写真实 Markdown 文件\n- 浏览器模式：数据仅保存在内存中，刷新即重置\n\n请从左侧新建一篇笔记，体验编辑与手动保存。\n']]);
async function hash(body:string){return 'sha256:'+Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(body)))).map(x=>x.toString(16).padStart(2,'0')).join('');}
export async function demoDispatch<T>(r:Request):Promise<Response<T>>{
 const p=r.payload; let data:unknown;
 const fail=(code:'E_NOT_FOUND'|'E_PATH_INVALID'|'E_EXTERNAL_MODIFIED'|'E_INTERNAL',message:string):Response<T>=>({id:r.id,ok:false,error:{code,message,detail:{},retryable:false}});
 switch(r.command){
 case 'vault_open': data={vaultId:'demo',noteCount:notes.size};break;
 case 'vault_info':data={vaultPath:'浏览器演示库',indexState:'unavailable',syncState:'disabled'};break;
 case 'note_list':data={items:[...notes.keys()].sort().map(path=>({path,title:path.slice(0,-3),mtimeMs:0,hasAttachment:false}))};break;
 case 'note_create':{
 const title=String(p.title??'').trim();if(!title||/[\\/:*?"<>|]/.test(title)||title==='.'||title==='..')return fail('E_PATH_INVALID','请输入合法的笔记标题');
 let path=title+'.md',i=1;while(notes.has(path))path=`${title} (${i++}).md`;notes.set(path,'');data={path};break;}
 case 'note_read':{const path=String(p.path),body=notes.get(path);if(body===undefined)return fail('E_NOT_FOUND','笔记不存在');data={path,title:path.slice(0,-3),body,frontmatter:{},frontmatterError:null,contentHash:await hash(body),mtimeMs:0,sizeBytes:new TextEncoder().encode(body).length,encoding:'utf-8',lineEnding:'lf',readOnly:false};break;}
 case 'note_save':{const path=String(p.path),old=notes.get(path);if(old===undefined)return fail('E_NOT_FOUND','笔记不存在');if(p.baseHash!==await hash(old))return fail('E_EXTERNAL_MODIFIED','内容已变更，请重新打开');const body=String(p.body);notes.set(path,body);data={path,contentHash:await hash(body),mtimeMs:Date.now(),bytesWritten:new TextEncoder().encode(body).length,snapshotCreated:false,indexQueued:false,warnings:['浏览器演示未写入磁盘；刷新后重置']};break;}
 default:return fail('E_INTERNAL','此能力尚未接入基础工程');
 }
 return {id:r.id,ok:true,data:data as T};
}

import { test, expect, type Page } from '@playwright/test';
async function dataBridge(page:Page){
 await page.route('**/src/vault-dialog.ts*',route=>route.fulfill({contentType:'application/javascript',body:`
 window.__folderChoice=null;window.__folderError=false;window.__folderCalls=[];
 export async function chooseVaultFolder(path){window.__folderCalls.push(path);if(window.__folderError)throw new Error('文件夹选择窗口不可用');return window.__folderChoice;}` }));
 await page.route('**/src/bridge.ts*',route=>route.fulfill({contentType:'application/javascript',body:`
 export const desktopMode=true;
 let body='# 当前内容',hash='current',vault='/fixture/vault';window.__dataCalls=[];window.__previewDelay=false;window.__releasePreview=null;
 const content=()=>({path:'A.md',title:'A',body,contentHash:hash,mtimeMs:1,sizeBytes:body.length,encoding:'utf-8',lineEnding:'lf',readOnly:false,frontmatter:{},frontmatterError:null,links:[]});
 export async function call(command,p={}){window.__dataCalls.push({command,p});
 if(command==='settings_get')return {values:{vaultPath:'/fixture/vault'}};
 if(command==='vault_open'){vault=p.path;return {vaultId:'fixture',noteCount:1};}if(command==='vault_info')return {vaultPath:vault};
 if(command==='note_list')return {items:p.scope==='versions'?(window.__versionDays===0?[]:[{path:'A.md',versionId:'old',createdMs:1,description:'历史节点',sizeBytes:8}]):p.scope?[]:[{path:'A.md',title:'A',mtimeMs:1,hasAttachment:false}]};
 if(command==='note_read')return p.versionId?{body:'# 历史内容'}:content();
 if(command==='note_save'){if(p.action==='draft')return {};body=p.body;hash='saved';return {path:'A.md',contentHash:hash,mtimeMs:1,bytesWritten:body.length,snapshotCreated:true,indexQueued:true,warnings:[]};}
 if(command==='note_restore'){body='# 历史内容';hash='old';return {path:'A.md',contentHash:hash,mtimeMs:1,warnings:[]};}
 if(command==='index_status')return {state:'ready',noteCount:1};
 if(command==='link_backlinks'||command==='link_mentions')return {items:[]};
 if(command==='export_note'){if(p.preview){if(window.__previewDelay)await new Promise(r=>window.__releasePreview=r);return {html:'<!doctype html><h1>'+p.body.replaceAll('<','&lt;')+'</h1>',missingAssets:[],warnings:[]};}return {filePath:p.targetPath,missingAssets:['missing.png'],warnings:[{kind:'unresolved_link',target:'Missing'}]};}
 if(command==='backup_create')return {filePath:'/fixture/backup.zip',bytes:100,checksum:'fixture'};
 if(command==='backup_restore'){if(window.__restoreFailure&&!p.verifyOnly)throw {code:'E_IO_PERMISSION',message:'恢复发布失败',detail:{retainedOriginalDir:'/fixture/retained-vault',snapshotPath:'/fixture/before.zip'}};return p.verifyOnly?{verified:true,noteCount:1}:{restoredTo:p.targetPath,snapshotPath:'/fixture/before.zip'};}
 if(command==='sync_configure')return {ok:true};if(command==='sync_now')return {completed:true,conflicts:['A.md']};if(command==='sync_status')return {state:'conflict',conflicts:['A.md']};
 if(command==='settings_set'){if(p.values.backup)window.__versionDays=p.values.backup.versionRetentionDays;return {ok:true};}throw new Error('unsupported '+command);}` }));
 await page.goto('/');await page.locator('#openPath').click();await page.getByRole('button',{name:'A.md',exact:true}).click();
}

test('选择文件夹后自动填入并打开所选中文路径',async({page})=>{
 await dataBridge(page);await page.evaluate(()=>{(window as any).__folderChoice='/fixture/手动选择的笔记库';});await page.locator('#open').click();
 await expect(page.locator('#status')).toHaveText('笔记库已打开');await expect(page.locator('#vaultPath')).toHaveValue('/fixture/手动选择的笔记库');
 const calls=await page.evaluate(()=>(window as any).__dataCalls);expect(calls.findLast((c:any)=>c.command==='vault_open').p.path).toBe('/fixture/手动选择的笔记库');
});

test('取消选择不切库不丢失当前编辑',async({page})=>{
 await dataBridge(page);const before=await page.evaluate(()=>(window as any).__dataCalls.filter((c:any)=>c.command==='vault_open').length);
 await page.locator('.cm-content').click();await page.keyboard.press('Control+End');await page.keyboard.insertText(' pending edit');await page.locator('#open').click();
 await expect.poll(()=>page.evaluate(()=>(window as any).__folderCalls.length)).toBe(1);await expect(page.locator('#open')).toBeEnabled();
 await expect(page.locator('#vaultPath')).toHaveValue('/fixture/vault');await expect(page.locator('.cm-content')).toContainText('pending edit');
 expect(await page.evaluate(()=>(window as any).__dataCalls.filter((c:any)=>c.command==='vault_open').length)).toBe(before);
});

test('选择器报错时保留当前库并允许按路径打开',async({page})=>{
 await dataBridge(page);await page.evaluate(()=>{(window as any).__folderError=true;});await page.locator('#open').click();
 await expect(page.locator('#status')).toContainText('文件夹选择窗口不可用');await expect(page.locator('.cm-content')).toContainText('当前内容');
 await expect(page.locator('#openPath')).toBeEnabled();await page.locator('#openPath').click();await expect(page.locator('#status')).toHaveText('笔记库已打开');
});
test('隔离的预览消费当前未保存文本',async({page})=>{await dataBridge(page);await page.locator('#previewToggle').click();await expect(page.frameLocator('#markdownPreview').locator('h1')).toContainText('当前内容');await expect(page.locator('#markdownPreview')).toHaveAttribute('sandbox','');const editor=page.locator('.cm-content');await editor.click();await page.keyboard.press('Control+End');await page.keyboard.type(' unsaved');await expect(page.frameLocator('#markdownPreview').locator('h1')).toContainText('unsaved');});
test('导出展示缺失资源和链接警告',async({page})=>{await dataBridge(page);await page.locator('#exportTarget').fill('/fixture/out.html');await page.locator('#exportRun').click();await expect(page.locator('#exportResult')).toContainText('missing.png');await expect(page.locator('#exportResult')).toContainText('unresolved_link');});
test('版本可对比并在明确确认后回滚',async({page})=>{await dataBridge(page);await page.locator('#versionsRefresh').click();await page.locator('#versions').getByRole('button',{name:'对比'}).click();await expect(page.locator('#versionOld')).toHaveValue('# 历史内容');await expect(page.locator('#versionCurrent')).toHaveValue('# 当前内容');await page.locator('#compareClose').click();page.once('dialog',d=>d.accept());await page.locator('#versions').getByRole('button',{name:'回滚'}).click();await expect(page.locator('.cm-content')).toContainText('历史内容');});
test('整库备份和仅校验均有可见结果',async({page})=>{await dataBridge(page);page.once('dialog',d=>d.accept('保存节点'));await page.locator('#backupRun').click();await expect(page.locator('#restoreFile')).toHaveValue('/fixture/backup.zip');await page.locator('#verifyBackup').click();await expect(page.locator('#backupResult')).toContainText('校验通过');});
test('同步配置清空密码 并展示冲突路径',async({page})=>{await dataBridge(page);await page.getByText('WebDAV 同步',{exact:true}).click();await page.locator('#syncEndpoint').fill('https://dav.example.invalid/notes/');await page.locator('#syncUser').fill('fixture');await page.locator('#syncPassword').fill('fixture-only');await page.locator('#syncConfigure').click();await expect(page.locator('#syncPassword')).toHaveValue('');await page.locator('#syncRun').click();await expect(page.locator('#syncResult')).toContainText('冲突已保留双份');});

test('PDF 导出可选择并提示浏览器要求',async({page})=>{await dataBridge(page);await expect(page.locator('#exportFormat option[value="pdf"]')).toHaveJSProperty('disabled',false);});

test('版本保留可关闭并通过既有设置命令保存',async({page})=>{
 await dataBridge(page);await page.locator('#versionRetentionDays').fill('0');await page.locator('#backupSettings').click();
 await expect(page.locator('#backupResult')).toContainText('版本历史已关闭');
 await expect(page.locator('#versions')).toContainText('版本历史已关闭');
 const calls=await page.evaluate(()=>(window as any).__dataCalls);expect(calls.findLast((c:any)=>c.command==='settings_set').p.values.backup.versionRetentionDays).toBe(0);
});

test('恢复回退失败时显示原库保留路径与快照路径',async({page})=>{
 await dataBridge(page);await page.evaluate(()=>{(window as any).__restoreFailure=true;});
 await page.locator('#restoreMode').selectOption('original');await page.locator('#restoreFile').fill('/fixture/backup.zip');page.once('dialog',d=>d.accept());await page.locator('#restoreRun').click();
 await expect(page.locator('#backupResult')).toContainText('/fixture/retained-vault');await expect(page.locator('#backupResult')).toContainText('/fixture/before.zip');
});

test('迟到预览不会覆盖较新的编辑文本',async({page})=>{
 await dataBridge(page);await page.locator('#previewToggle').click();await expect(page.frameLocator('#markdownPreview').locator('h1')).toContainText('当前内容');
 await page.evaluate(()=>{(window as any).__previewDelay=true;});
 await page.locator('.cm-content').click();await page.keyboard.press('Control+End');await page.keyboard.type(' delayed');
 await expect.poll(()=>page.evaluate(()=>typeof (window as any).__releasePreview)).toBe('function');
 await page.evaluate(()=>{(window as any).__oldRelease=(window as any).__releasePreview;(window as any).__previewDelay=false;});
 await page.keyboard.type(' newest');await expect(page.frameLocator('#markdownPreview').locator('h1')).toContainText('newest');
 await page.evaluate(()=>(window as any).__oldRelease());await expect(page.frameLocator('#markdownPreview').locator('h1')).toContainText('newest');
});

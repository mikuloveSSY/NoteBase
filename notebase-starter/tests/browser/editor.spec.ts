import { submitInput } from '../input-dialog';
import {test,expect,type Page} from '@playwright/test';
test('演示模式编辑 保存并重新打开',async({page})=>{
 await page.goto('/');await expect(page.locator('#mode')).toContainText('刷新即重置');
 await page.getByRole('button',{name:'欢迎.md',exact:true}).click();
 await expect(page.locator('h1')).toHaveText('欢迎');
 const editor=page.locator('.cm-content');await editor.click();await page.keyboard.press('Control+End');await page.keyboard.type('\nsmoke-test');
 await expect(page.locator('#dirty')).toHaveText('有未保存修改');await page.locator('#save').click();
 await expect(page.locator('#dirty')).toHaveText('已保存');await expect(page.locator('#status')).toContainText('未写入磁盘');
 await page.getByRole('button',{name:'欢迎.md',exact:true}).click();await expect(editor).toContainText('smoke-test');
 await page.screenshot({path:'test-results/editor.png',fullPage:true});
});
test('新建同名笔记生成独立路径 刷新重置演示',async({page})=>{
 await page.goto('/');await expect(page.locator('#new')).toBeEnabled();
 await page.locator('#sidebarTools>summary').click();
 await page.locator('#new').click();await submitInput(page,'欢迎');
 await expect(page.locator('#path')).toHaveText('欢迎 (2).md');
 await page.reload();await expect(page.getByRole('button',{name:'欢迎 (2).md',exact:true})).toHaveCount(0);
});

test('修正系统保留名并向用户说明实际路径',async({page})=>{
 await page.goto('/');await expect(page.locator('#new')).toBeEnabled();
 await page.locator('#sidebarTools>summary').click();
 await page.locator('#new').click();await submitInput(page,'COM¹');
 await expect(page.locator('#path')).toHaveText('_COM1.md');
 await expect(page.locator('#status')).toContainText('文件名已修正或避开同名：_COM1.md');
});

// 只验证 UI 对真实后端响应形态的消费；磁盘行为由 Rust 集成测试验证。
async function mockStorage(page:Page, options:{large?:boolean;invalid?:boolean;count?:number}={}){
 await page.route('**/src/bridge.ts*',route=>route.fulfill({
  contentType:'application/javascript',
  body:`export const desktopMode=false;
   const options=${JSON.stringify(options)};
   window.__saveCalls=0;
   const paths=Array.from({length:options.count??1},(_,i)=>'笔记-'+String(i).padStart(4,'0')+'.md');
   export async function call(command,payload={}){
    if(command==='vault_open')return {vaultId:'test',noteCount:paths.length};
    if(command==='note_list'){
     const left=paths.filter(p=>p>(payload.cursor??''));
     return {items:left.slice(0,500).map(path=>({path,title:path,mtimeMs:0,hasAttachment:false})),nextCursor:left.length>500?left[499]:null};
    }
    if(command==='note_read')return {path:payload.path,title:payload.path,body:'只读内容',frontmatter:{},frontmatterError:null,contentHash:options.large?'':'sha256:test',mtimeMs:0,sizeBytes:options.large?10485761:12,encoding:options.invalid?'unknown':'utf-8',lineEnding:'lf',readOnly:!!(options.large||options.invalid)};
    if(command==='note_save'){window.__saveCalls++;throw new Error('不可保存只读预览');}
    throw new Error('unexpected command '+command);
   }`,
 }));
}

test('非法编码只读预览 禁止编辑和快捷键保存',async({page})=>{
 await mockStorage(page,{invalid:true});await page.goto('/');
 await page.getByRole('button',{name:'笔记-0000.md',exact:true}).click();
 await expect(page.locator('#readNotice')).toContainText('非法 UTF-8');
 await expect(page.locator('#dirty')).toHaveText('只读');
 await expect(page.locator('.cm-content')).toHaveAttribute('contenteditable','false');
 await expect(page.locator('#save')).toBeDisabled();
 await page.keyboard.press('Control+s');
 expect(await page.evaluate('window.__saveCalls')).toBe(0);
});

test('超大文件显示预览截断说明',async({page})=>{
 await mockStorage(page,{large:true});await page.goto('/');
 await page.getByRole('button',{name:'笔记-0000.md',exact:true}).click();
 await expect(page.locator('#readNotice')).toContainText('仅预览前 256 KiB');
 await expect(page.locator('.cm-content')).toHaveAttribute('contenteditable','false');
 await expect(page.locator('#save')).toBeDisabled();
});

test('可以加载第501篇笔记并保持列表选择',async({page})=>{
 await mockStorage(page,{count:501});await page.goto('/');
 await expect(page.locator('#notes button')).toHaveCount(500);
 await page.getByRole('button',{name:'加载更多笔记'}).click();
 await expect(page.locator('#notes button')).toHaveCount(501);
 await page.getByRole('button',{name:'笔记-0500.md',exact:true}).click();
 await expect(page.locator('#path')).toHaveText('笔记-0500.md');
 await expect(page.locator('#notes button')).toHaveCount(501);
 await expect(page.getByRole('button',{name:'笔记-0500.md',exact:true})).toHaveClass(/selected/);
 await expect(page.locator('#more')).toBeHidden();
});

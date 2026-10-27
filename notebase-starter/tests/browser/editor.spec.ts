import {test,expect} from '@playwright/test';
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
 page.once('dialog',dialog=>dialog.accept('欢迎'));await page.locator('#new').click();
 await expect(page.locator('#path')).toHaveText('欢迎 (1).md');
 await page.reload();await expect(page.getByRole('button',{name:'欢迎 (1).md',exact:true})).toHaveCount(0);
});

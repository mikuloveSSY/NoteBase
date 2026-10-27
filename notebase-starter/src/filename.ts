// 浏览器内存演示的名称规则；真实文件路径只由 Rust 校验。
export function sanitizeTitle(input: string): string {
  if (!input.trim()) throw new Error('标题不能为空');
  let name = input.trim().normalize('NFKC')
    .replace(/[\u0000-\u001f\u007f-\u009f<>:"/\\|?*]/g, '_')
    .replace(/[. ]+$/, '');
  if (!name) name = '未命名';
  if (/^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$/i.test(name.split('.')[0].trimEnd())) {
    name = '_' + name;
  }
  if (name.length > 255) throw new Error('文件名过长');
  return name;
}

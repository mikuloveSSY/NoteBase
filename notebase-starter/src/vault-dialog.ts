import { open } from '@tauri-apps/plugin-dialog';

/** System folder selection is shell UI; vault_open remains the business boundary. */
export async function chooseVaultFolder(currentPath: string): Promise<string | null> {
  return open({
    title: '选择 NoteBase 笔记库文件夹',
    directory: true,
    multiple: false,
    defaultPath: currentPath.trim() || undefined,
  });
}

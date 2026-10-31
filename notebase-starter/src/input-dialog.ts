export const requiredText = (label: string) => (value: string) => value.trim() ? null : label + '不能为空';
export function absoluteDirectory(value: string) {
  if (!value.trim()) return '请输入目标文件夹的绝对路径';
  return /^(?:[a-z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+|\/)/i.test(value.trim()) ? null : '请输入绝对路径，例如 E:\\我的笔记';
}

/** An app-owned modal. Only successful submission resolves with a result. */
export function inputDialog<T>(options: {
  title: string; label: string; hint?: string; value?: string;
  validate?: (value: string) => string | null;
  submit: (value: string) => Promise<T>;
}): Promise<{ value: string; result: T } | null> {
  const previousFocus = document.activeElement as HTMLElement | null;
  const dialog = document.createElement('dialog');
  dialog.id = 'inputDialog'; dialog.className = 'input-dialog';
  dialog.setAttribute('aria-labelledby', 'inputDialogTitle');
  dialog.innerHTML = '<form novalidate><h2 id="inputDialogTitle"></h2><p id="inputDialogHint" class="muted"></p><label for="inputDialogValue" id="inputDialogLabel"></label><input id="inputDialogValue" autocomplete="off" aria-describedby="inputDialogHint inputDialogError"><p id="inputDialogError" class="field-error" role="alert" hidden></p><div class="dialog-actions"><button type="button" id="inputDialogCancel">取消</button><button type="submit" id="inputDialogConfirm" class="primary">确定</button></div></form>';
  const get = <E extends HTMLElement>(id: string) => dialog.querySelector<E>('#' + id)!;
  const input = get<HTMLInputElement>('inputDialogValue'), error = get('inputDialogError');
  const confirm = get<HTMLButtonElement>('inputDialogConfirm'), cancel = get<HTMLButtonElement>('inputDialogCancel');
  get('inputDialogTitle').textContent = options.title; get('inputDialogLabel').textContent = options.label;
  get('inputDialogHint').textContent = options.hint ?? ''; input.value = options.value ?? '';
  let submitting = false, attempted = false, outcome: { value: string; result: T } | null = null;
  function warn(message: string | null) {
    error.textContent = message; error.hidden = !message; input.setAttribute('aria-invalid', String(!!message));
  }
  input.addEventListener('input', () => { if (attempted) warn(options.validate?.(input.value) ?? null); });
  document.body.append(dialog);
  return new Promise(resolve => {
    dialog.addEventListener('close', () => {
      dialog.remove();
      if (previousFocus?.isConnected) previousFocus.focus({ preventScroll: true });
      resolve(outcome);
    }, { once: true });
    dialog.addEventListener('cancel', event => { if (submitting) event.preventDefault(); });
    cancel.onclick = () => { if (!submitting) dialog.close(); };
    dialog.querySelector('form')!.onsubmit = event => {
      event.preventDefault(); if (submitting) return; attempted = true;
      const warning = options.validate?.(input.value) ?? null;
      warn(warning); if (warning) { input.focus(); return; }
      const value = input.value;
      submitting = true; confirm.disabled = cancel.disabled = input.disabled = true;
      confirm.textContent = '处理中…'; dialog.setAttribute('aria-busy', 'true');
      void options.submit(value).then(result => { outcome = { value, result }; dialog.close(); }).catch(cause => {
        warn(String((cause as { message?: string })?.message ?? cause));
      }).finally(() => {
        submitting = false; confirm.disabled = cancel.disabled = input.disabled = false;
        confirm.textContent = '确定'; dialog.removeAttribute('aria-busy');
        if (dialog.open) input.focus();
      });
    };
    dialog.showModal(); input.focus(); input.select();
  });
}

import { invoke, isTauri } from '@tauri-apps/api/core';
import type { AppError, Command, Request, Response } from './contracts';
import { demoDispatch } from './demo';
let sequence = 0;
// Module lifetime is the window lifetime; never reuse another window's identity.
export const clientId = crypto.randomUUID();
export const desktopMode = isTauri();
export class AppCallError extends Error {
  readonly code: AppError['code'];
  readonly detail: AppError['detail'];
  readonly retryable: boolean;
  constructor(error: AppError) {
    super(error.message + '（' + error.code + '）'); this.name = 'AppCallError';
    this.code = error.code; this.detail = error.detail; this.retryable = error.retryable;
  }
}
export async function call<T>(command: Command, payload: Record<string, unknown> = {}): Promise<T> {
  const clientSeq = ++sequence;
  const request: Request = { id: crypto.randomUUID(), command, payload: command.startsWith('note_') ? { ...payload, clientId, clientSeq } : payload, clientSeq };
  const response = desktopMode ? await invoke<Response<T>>(command, { request }) : await demoDispatch<T>(request);
  if (!response.ok) throw new AppCallError(response.error);
  return response.data;
}

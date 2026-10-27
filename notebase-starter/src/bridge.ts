import { invoke, isTauri } from '@tauri-apps/api/core';
import type { Command, Request, Response } from './contracts';
import { demoDispatch } from './demo';
let sequence=0;
export const desktopMode=isTauri();
export async function call<T>(command:Command,payload:Record<string,unknown>={}):Promise<T>{
 const clientSeq=++sequence;
 const request:Request={id:crypto.randomUUID(),command,payload,clientSeq};
 const response=desktopMode?await invoke<Response<T>>(command,{request}):await demoDispatch<T>(request);
 if(!response.ok) throw new Error(`${response.error.message}（${response.error.code}）`);
 return response.data;
}

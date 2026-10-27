// 与软件设计文档 v1.2 §5 对齐。CMD- 只用于文档，不在线上传输。
export const COMMANDS = ['vault_open','vault_info','note_list','note_read','note_save','note_create','note_rename','note_delete','note_restore','search','link_backlinks','link_mentions','link_graph','index_status','index_rebuild','export_note','backup_create','backup_restore','sync_configure','sync_now','sync_status','settings_get','settings_set'] as const;
export type Command = typeof COMMANDS[number];
export const EVENTS = ['note.changed','note.conflict','index.progress','index.state','sync.state','conflict.detected','error.raised'] as const;
export const ERROR_CODES = ['E_IO_DISK_FULL','E_IO_PERMISSION','E_IO_LOCKED','E_NOT_FOUND','E_EXTERNAL_MODIFIED','E_STALE_CLIENT','E_FILE_TOO_LARGE','E_ENCODING_INVALID','E_PATH_INVALID','E_INDEX_UNAVAILABLE','E_QUERY_SYNTAX','E_QUERY_UNSUPPORTED','E_SYNC_AUTH','E_SYNC_NETWORK','E_SYNC_BUSY','E_PARTIAL_RENAME','E_BACKUP_CHECKSUM','E_FORMAT_UNSUPPORTED','E_VAULT_NOT_FOUND','E_PDF_UNSUPPORTED','E_KEYMAP_CONFLICT','E_CANCELLED','E_INTERNAL'] as const;
export interface Request {id:string;command:Command;payload:Record<string,unknown>;clientSeq:number}
export interface AppError {code:typeof ERROR_CODES[number];message:string;detail:Record<string,unknown>;retryable:boolean}
export type Response<T> = {id:string;ok:true;data:T}|{id:string;ok:false;error:AppError};
export interface NoteEntry {path:string;title:string;mtimeMs:number;hasAttachment:boolean}
export interface NoteContent {path:string;title:string;body:string;frontmatter:Record<string,unknown>;frontmatterError:string|null;contentHash:string;mtimeMs:number;sizeBytes:number;encoding:string;lineEnding:string;readOnly:boolean}
export interface SaveResult {path:string;contentHash:string;mtimeMs:number;bytesWritten:number;snapshotCreated:boolean;indexQueued:boolean;warnings:string[]}
export interface IndexStatus {state:'ready'|'rebuilding'|'offline'|'unavailable';noteCount:number;indexBytes:number;lastFullBuildMs:number|null;progress:{phase:'scan'|'write'|'optimize';done:number;total:number;elapsedMs:number}|null}

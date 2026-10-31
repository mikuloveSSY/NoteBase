// 与软件设计文档 v1.2 §5 对齐。CMD- 只用于文档，不在线上传输。
export const COMMANDS = ['vault_open','vault_info','note_list','note_read','note_save','note_create','note_rename','note_delete','note_restore','search','link_backlinks','link_mentions','link_graph','index_status','index_rebuild','export_note','backup_create','backup_restore','sync_configure','sync_now','sync_status','settings_get','settings_set'] as const;
export type Command = typeof COMMANDS[number];
export const EVENTS = ['note.changed','note.conflict','index.progress','index.state','sync.state','conflict.detected','error.raised'] as const;
export function transportEventName(event:typeof EVENTS[number]):string{return 'notebase:'+event.replaceAll('.','_');}
export const ERROR_CODES = ['E_IO_DISK_FULL','E_IO_PERMISSION','E_IO_LOCKED','E_NOT_FOUND','E_EXTERNAL_MODIFIED','E_STALE_CLIENT','E_FILE_TOO_LARGE','E_ENCODING_INVALID','E_PATH_INVALID','E_INDEX_UNAVAILABLE','E_QUERY_SYNTAX','E_QUERY_UNSUPPORTED','E_SYNC_AUTH','E_SYNC_NETWORK','E_SYNC_BUSY','E_PARTIAL_RENAME','E_BACKUP_CHECKSUM','E_FORMAT_UNSUPPORTED','E_VAULT_NOT_FOUND','E_PDF_UNSUPPORTED','E_KEYMAP_CONFLICT','E_CANCELLED','E_INTERNAL'] as const;
export interface Request {id:string;command:Command;payload:Record<string,unknown>;clientSeq:number}
export interface AppError {code:typeof ERROR_CODES[number];message:string;detail:Record<string,unknown>;retryable:boolean}
export type Response<T> = {id:string;ok:true;data:T}|{id:string;ok:false;error:AppError};
export interface NoteEntry {path:string;title:string;mtimeMs:number;hasAttachment:boolean;tags?:string[];favorite?:boolean;pinned?:boolean}
export interface NoteContent {path:string;title:string;body:string;frontmatter:Record<string,unknown>;frontmatterError:string|null;contentHash:string;mtimeMs:number;sizeBytes:number;encoding:string;lineEnding:string;readOnly:boolean;conflictPaused?:boolean;links?:NoteLink[]}
export interface NoteLink {target:string;label?:string;line:number;col:number;start:number;end:number}
export interface DraftEntry {id:string;path:string;body:string;baseHash:string;clientId:string;clientSeq:number;savedAtMs:number}
export interface TrashEntry {trashPath:string;originalPath:string;deletedAtMs:number}
export interface RenamePlan {id:string;from:string;to:string;affectedFiles:{path:string;preview:unknown}[];totalReplacements:number;state?:string}
export interface Backlink {source?:string;sourcePath?:string;line:number;col:number;snippet?:string;context?:string;circular?:boolean;ambiguous?:boolean;candidates?:string[]}
export interface Mention {sourcePath:string;text:string;line:number;col:number;start:number;end:number}
export interface LinkResolution {targetPath?:string|null;candidates:string[];ambiguous:boolean}
export interface GraphData {nodes:({path:string;id?:string;title?:string}|string)[];edges:{from:string;to:string}[]}
export interface AttachmentResult {attachmentPath:string;relativePath:string;markdown:string}
export interface SaveResult {path:string;contentHash:string;mtimeMs:number;bytesWritten:number;snapshotCreated:boolean;indexQueued:boolean;warnings:string[]}
export interface RenderedNote {html:string;missingAssets:string[];warnings:Record<string,unknown>[]}
export interface ExportResult {filePath:string;missingAssets?:string[];warnings?:Record<string,unknown>[]}
export interface VersionEntry {path:string;versionId:string;contentHash:string;createdMs:number;sizeBytes:number;description:string}
export interface BackupResult {filePath:string;bytes:number;checksum:string;noteCount:number;attachmentCount:number}
export interface BackupSettings {enabled:boolean;intervalHours:number;keepCount:number;dir:string;versionRetentionDays?:number}
export interface SearchQuery {query:string;filters?:{tags?:string[];pathPrefix?:string;mtimeFromMs?:number|null;mtimeToMs?:number|null;createdFromMs?:number|null;createdToMs?:number|null;hasAttachment?:boolean|null};sort?:'relevance'|'mtime';limit?:number;cursor?:string|null}
export interface SearchHit {path:string;title:string;snippet:string;score:number;matchedIn:string;mtimeMs:number}
export interface SearchPage {items:SearchHit[];nextCursor:string|null;totalApprox:number;tookMs:number;degraded:boolean;degradeReason:string|null}
export interface IndexRebuildResult {jobId:string}
export interface VaultSettings {vaultPath:string|null}
export interface IndexStatus {state:'ready'|'rebuilding'|'offline'|'unavailable';noteCount:number;indexBytes:number;lastFullBuildMs:number|null;progress:{phase:'scan'|'write'|'optimize';done:number;total:number;elapsedMs:number}|null}

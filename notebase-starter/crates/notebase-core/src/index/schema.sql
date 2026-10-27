PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

-- 元信息
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
-- schema_version / tokenizer_version / vault_id / last_reconcile_ms / rebuild_cursor

-- 笔记主表（路径即身份，见 D-02）
CREATE TABLE notes (
  path           TEXT PRIMARY KEY,                       -- 库相对路径，'/' 分隔，NFC
  title          TEXT NOT NULL,
  size_bytes     INTEGER NOT NULL CHECK (size_bytes >= 0),
  mtime_ms       INTEGER NOT NULL CHECK (mtime_ms >= 0),
  content_hash   TEXT NOT NULL,                          -- sha256 hex，64 字符
  created_ms     INTEGER,
  updated_ms     INTEGER,
  has_attachment INTEGER NOT NULL DEFAULT 0 CHECK (has_attachment IN (0, 1)),
  indexed_at_ms  INTEGER NOT NULL
);
CREATE INDEX idx_notes_mtime ON notes(mtime_ms);

-- 别名（供 [[别名]] 解析与检索）
CREATE TABLE aliases (
  note_path TEXT NOT NULL REFERENCES notes(path) ON DELETE CASCADE,
  alias     TEXT NOT NULL,
  PRIMARY KEY (note_path, alias)
);
CREATE INDEX idx_aliases_alias ON aliases(alias);

-- 标签（层级标签以 '/' 原样存储，聚合查询在应用层展开）
CREATE TABLE tags (
  note_path TEXT NOT NULL REFERENCES notes(path) ON DELETE CASCADE,
  tag       TEXT NOT NULL,
  PRIMARY KEY (note_path, tag)
);
CREATE INDEX idx_tags_tag ON tags(tag);

-- 链接（派生自正文，随索引重建）
CREATE TABLE links (
  id          INTEGER PRIMARY KEY,
  source_path TEXT NOT NULL REFERENCES notes(path) ON DELETE CASCADE,
  target_text TEXT NOT NULL,
  target_path TEXT REFERENCES notes(path) ON DELETE SET NULL,
  line        INTEGER NOT NULL CHECK (line >= 1),
  col         INTEGER NOT NULL CHECK (col >= 1),
  resolved    INTEGER NOT NULL CHECK (resolved IN (0, 1)),
  ambiguous   INTEGER NOT NULL DEFAULT 0 CHECK (ambiguous IN (0, 1))
);
CREATE INDEX idx_links_source ON links(source_path);
CREATE INDEX idx_links_target ON links(target_path);

-- 全文索引（trigram：只服务 ≥3 字符查询，见 D-03）
-- 表形态：普通 FTS5 表（非 external content、非 contentless）。
-- 它自带一份 title/body 副本，影子表名为 note_fts_content；
-- 短查询（<3 字符）的正文 LIKE 扫描直接作用于该影子表，不读 notes 表。
CREATE VIRTUAL TABLE note_fts USING fts5(
  path UNINDEXED,
  title,
  body,
  tokenize = 'trigram'
);

-- 版本快照索引（blob 在文件系统）
CREATE TABLE snapshot_index (
  note_path  TEXT NOT NULL,
  version_id TEXT NOT NULL,          -- 内容 sha256 前 16 位
  blob_rel   TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  created_ms INTEGER NOT NULL,
  PRIMARY KEY (note_path, version_id)
);
CREATE INDEX idx_snapshot_age ON snapshot_index(created_ms);

-- 同步状态（base_hash 是三方比对的基准）
CREATE TABLE sync_state (
  path           TEXT PRIMARY KEY,
  base_hash      TEXT,
  local_hash     TEXT,
  remote_etag    TEXT,
  device_id      TEXT NOT NULL,
  last_synced_ms INTEGER,
  state          TEXT NOT NULL CHECK (state IN ('clean','dirty','pending','conflict'))
);

-- 删除墓碑（区分「本地删除」与「从未同步」）
CREATE TABLE sync_tombstone (
  path       TEXT PRIMARY KEY,
  deleted_ms INTEGER NOT NULL,
  base_hash  TEXT
);

-- 重命名日志（幂等续跑）
CREATE TABLE rename_journal (
  id           INTEGER PRIMARY KEY,
  from_path    TEXT NOT NULL,
  to_path      TEXT NOT NULL,
  affected     TEXT NOT NULL,        -- JSON 数组
  done         TEXT NOT NULL,        -- JSON 数组
  created_ms   INTEGER NOT NULL,
  state        TEXT NOT NULL CHECK (state IN ('running','done','failed'))
);

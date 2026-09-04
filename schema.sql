PRAGMA journal_mode=WAL;
CREATE TABLE IF NOT EXISTS cookies(
  host TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
  value TEXT NOT NULL, expires INTEGER NOT NULL, created INTEGER NOT NULL,
  secure INTEGER NOT NULL, httponly INTEGER NOT NULL, samesite TEXT NOT NULL,
  host_only INTEGER NOT NULL,
  PRIMARY KEY(host, path, name));
CREATE INDEX IF NOT EXISTS idx_cookies_host ON cookies(host);
CREATE TABLE IF NOT EXISTS cache(
  url TEXT PRIMARY KEY, status INTEGER NOT NULL, headers TEXT NOT NULL,
  etag TEXT NOT NULL, lastmod TEXT NOT NULL, fetched_at INTEGER NOT NULL,
  max_age INTEGER NOT NULL, vary TEXT NOT NULL, req_vary TEXT NOT NULL,
  body_path TEXT NOT NULL, size INTEGER NOT NULL, accessed INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS idx_cache_accessed ON cache(accessed);
CREATE TABLE IF NOT EXISTS localstorage(
  origin TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
  PRIMARY KEY(origin, key));
CREATE TABLE IF NOT EXISTS history(
  url TEXT NOT NULL, title TEXT NOT NULL, visited_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS idx_history_time ON history(visited_at);

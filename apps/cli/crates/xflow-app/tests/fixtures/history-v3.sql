-- Synthetic released v3 schema, including the original column order and defaults.
PRAGMA journal_mode=WAL;
CREATE TABLE history (
    id INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    text TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT,
    raw_text TEXT,
    app_id TEXT,
    language TEXT,
    duration_ms INTEGER,
    latency_ms INTEGER,
    mode TEXT NOT NULL DEFAULT 'dictation',
    words INTEGER NOT NULL DEFAULT 0,
    cleanup TEXT
);
CREATE INDEX history_created_at ON history (created_at);
CREATE INDEX history_latency ON history (latency_ms);
PRAGMA user_version=3;
INSERT INTO history VALUES
    (7, 1700000000, 'Hello 世界', 'groq', 'test-model', 'hello world', 'test.app', 'en', 1200, 250, 'dictation', 2, 'applied'),
    (42, 1700000001, 'command text', 'openai', NULL, NULL, NULL, NULL, NULL, NULL, 'command', 2, 'rejected');
INSERT INTO history (id, created_at, text, provider) VALUES (99, 1700000002, '', 'custom');

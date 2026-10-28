-- Credentials live in the OS credential store, never in SQLite.
CREATE TABLE todoist_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    account_id TEXT,
    connected_at INTEGER,
    tag TEXT NOT NULL DEFAULT 'dayplanner' CHECK (length(trim(tag)) > 0),
    last_synced_at INTEGER,
    pending_batch TEXT CHECK (pending_batch IS NULL OR json_valid(pending_batch))
) STRICT;
INSERT INTO todoist_settings (id) VALUES (1);

-- Retain the remote ID after a local deletion so it cannot be imported again.
-- Deletion and removing the selection tag disconnect; they never delete remotely.
CREATE TABLE todoist_links (
    remote_id TEXT PRIMARY KEY,
    task_id INTEGER UNIQUE REFERENCES tasks(id) ON DELETE SET NULL,
    baseline TEXT NOT NULL CHECK (json_valid(baseline)),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))
) STRICT;

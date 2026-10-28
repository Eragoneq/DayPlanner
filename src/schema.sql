CREATE TABLE tasks (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    notes TEXT NOT NULL DEFAULT '',
    completed INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1)),
    color TEXT NOT NULL DEFAULT 'lavender'
        CHECK (color IN ('lavender', 'blue', 'green', 'amber', 'rose'))
) STRICT;

CREATE TABLE time_blocks (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    starts_at INTEGER NOT NULL,
    ends_at INTEGER NOT NULL CHECK (ends_at > starts_at)
) STRICT;

CREATE TABLE time_sessions (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL,
    ended_at INTEGER CHECK (ended_at IS NULL OR ended_at >= last_saved_at),
    elapsed_seconds INTEGER NOT NULL DEFAULT 0 CHECK (elapsed_seconds >= 0),
    last_saved_at INTEGER NOT NULL CHECK (last_saved_at >= started_at)
) STRICT;

CREATE INDEX time_blocks_task ON time_blocks(task_id);
CREATE INDEX time_blocks_start ON time_blocks(starts_at);
CREATE INDEX time_sessions_task ON time_sessions(task_id);
CREATE UNIQUE INDEX one_active_session ON time_sessions((1)) WHERE ended_at IS NULL;

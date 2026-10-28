CREATE TABLE tags (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL COLLATE NOCASE UNIQUE CHECK (length(trim(name)) > 0)
) STRICT;

CREATE TABLE task_tags (
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (task_id, tag_id)
) STRICT;

CREATE INDEX task_tags_tag ON task_tags(tag_id);

CREATE TABLE subtasks (
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    completed INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1)),
    PRIMARY KEY (task_id, position)
) STRICT;

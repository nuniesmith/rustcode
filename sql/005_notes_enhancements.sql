-- Migration: 005_notes_enhancements.sql
-- Rewritten for PostgreSQL
-- Enhance notes system with proper tag relationships and repo linking

-- ============================================================================
-- Create notes table if it doesn't exist
-- ============================================================================

CREATE TABLE IF NOT EXISTS notes (
    id         TEXT PRIMARY KEY NOT NULL,
    title      TEXT NOT NULL,
    content    TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'active'
                   CHECK(status IN ('active', 'archived', 'deleted')),
    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    updated_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT
);

-- ============================================================================
-- Enhance notes table with repo linking
-- ============================================================================

ALTER TABLE notes ADD COLUMN IF NOT EXISTS repo_id TEXT;

CREATE INDEX IF NOT EXISTS idx_notes_repo_id ON notes(repo_id);
CREATE INDEX IF NOT EXISTS idx_notes_status  ON notes(status);
CREATE INDEX IF NOT EXISTS idx_notes_created ON notes(created_at DESC);

-- ============================================================================
-- Tags table
-- ============================================================================

CREATE TABLE IF NOT EXISTS tags (
    name        TEXT PRIMARY KEY,
    color       TEXT    DEFAULT '#3b82f6',
    description TEXT,
    usage_count INTEGER DEFAULT 0,
    created_at  BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    updated_at  BIGINT  NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT
);

-- ============================================================================
-- note_tags junction table (many-to-many)
-- ============================================================================

CREATE TABLE IF NOT EXISTS note_tags (
    note_id    TEXT   NOT NULL,
    tag        TEXT   NOT NULL,
    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT,
    PRIMARY KEY (note_id, tag),
    FOREIGN KEY (note_id) REFERENCES notes(id) ON DELETE CASCADE,
    FOREIGN KEY (tag)     REFERENCES tags(name) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_note_tags_tag  ON note_tags(tag);
CREATE INDEX IF NOT EXISTS idx_note_tags_note ON note_tags(note_id);

-- ============================================================================
-- Views
-- ============================================================================

CREATE OR REPLACE VIEW notes_with_tags AS
SELECT
    n.id,
    n.title,
    n.content,
    n.status,
    n.repo_id,
    n.created_at,
    n.updated_at,
    STRING_AGG(nt.tag, ',' ORDER BY nt.tag) AS tags,
    COUNT(nt.tag)                            AS tag_count
FROM notes n
LEFT JOIN note_tags nt ON n.id = nt.note_id
GROUP BY n.id, n.title, n.content, n.status, n.repo_id, n.created_at, n.updated_at;

CREATE OR REPLACE VIEW tag_stats AS
SELECT
    t.name,
    t.color,
    t.description,
    t.usage_count,
    COUNT(DISTINCT nt.note_id) AS current_note_count,
    t.created_at,
    t.updated_at
FROM tags t
LEFT JOIN note_tags nt ON t.name = nt.tag
GROUP BY t.name, t.color, t.description, t.usage_count, t.created_at, t.updated_at
ORDER BY t.usage_count DESC;

CREATE OR REPLACE VIEW repo_notes_summary AS
SELECT
    r.id                                                  AS repo_id,
    r.name                                                AS repo_name,
    COUNT(n.id)                                           AS note_count,
    COUNT(n.id) FILTER (WHERE n.status = 'inbox')         AS inbox_count,
    COUNT(n.id) FILTER (WHERE n.status = 'active')        AS active_count,
    COUNT(n.id) FILTER (WHERE n.status = 'done')          AS done_count,
    MAX(n.created_at)                                     AS last_note_at
FROM repositories r
LEFT JOIN notes n ON r.id = n.repo_id
GROUP BY r.id, r.name;

CREATE OR REPLACE VIEW recent_notes_activity AS
SELECT
    n.id,
    n.content,
    n.status,
    n.repo_id,
    r.name                                                     AS repo_name,
    STRING_AGG(nt.tag, ',' ORDER BY nt.tag)                   AS tags,
    n.created_at,
    TO_CHAR(TO_TIMESTAMP(n.created_at), 'YYYY-MM-DD HH24:MI:SS') AS created_at_formatted
FROM notes n
LEFT JOIN repositories r  ON n.repo_id = r.id
LEFT JOIN note_tags nt    ON n.id = nt.note_id
GROUP BY n.id, n.content, n.status, n.repo_id, r.name, n.created_at
ORDER BY n.created_at DESC
LIMIT 50;

-- ============================================================================
-- Trigger functions for tag usage counts and note updated_at
-- ============================================================================

CREATE OR REPLACE FUNCTION increment_tag_usage_fn()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO tags (name, created_at, updated_at)
    VALUES (NEW.tag, EXTRACT(EPOCH FROM NOW())::BIGINT, EXTRACT(EPOCH FROM NOW())::BIGINT)
    ON CONFLICT (name) DO UPDATE
        SET usage_count = tags.usage_count + 1,
            updated_at  = EXTRACT(EPOCH FROM NOW())::BIGINT;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION decrement_tag_usage_fn()
RETURNS TRIGGER AS $$
BEGIN
    UPDATE tags
    SET usage_count = GREATEST(0, usage_count - 1),
        updated_at  = EXTRACT(EPOCH FROM NOW())::BIGINT
    WHERE name = OLD.tag;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION update_note_updated_at_fn()
RETURNS TRIGGER AS $$
BEGIN
    UPDATE notes
    SET updated_at = EXTRACT(EPOCH FROM NOW())::BIGINT
    WHERE id = NEW.note_id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION update_note_updated_at_del_fn()
RETURNS TRIGGER AS $$
BEGIN
    UPDATE notes
    SET updated_at = EXTRACT(EPOCH FROM NOW())::BIGINT
    WHERE id = OLD.note_id;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

-- Attach triggers

DROP TRIGGER IF EXISTS increment_tag_usage           ON note_tags;
CREATE TRIGGER increment_tag_usage
AFTER INSERT ON note_tags
FOR EACH ROW EXECUTE FUNCTION increment_tag_usage_fn();

DROP TRIGGER IF EXISTS decrement_tag_usage           ON note_tags;
CREATE TRIGGER decrement_tag_usage
AFTER DELETE ON note_tags
FOR EACH ROW EXECUTE FUNCTION decrement_tag_usage_fn();

DROP TRIGGER IF EXISTS update_note_timestamp_on_tag_add    ON note_tags;
CREATE TRIGGER update_note_timestamp_on_tag_add
AFTER INSERT ON note_tags
FOR EACH ROW EXECUTE FUNCTION update_note_updated_at_fn();

DROP TRIGGER IF EXISTS update_note_timestamp_on_tag_remove ON note_tags;
CREATE TRIGGER update_note_timestamp_on_tag_remove
AFTER DELETE ON note_tags
FOR EACH ROW EXECUTE FUNCTION update_note_updated_at_del_fn();

-- notes updated_at trigger
DROP TRIGGER IF EXISTS notes_updated_at ON notes;
CREATE TRIGGER notes_updated_at
BEFORE UPDATE ON notes
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ============================================================================
-- Default tags
-- ============================================================================

INSERT INTO tags (name, color, description) VALUES
    ('idea',          '#10b981', 'New ideas and brainstorming'),
    ('todo',          '#f59e0b', 'Things to do'),
    ('bug',           '#ef4444', 'Bug reports and issues'),
    ('question',      '#8b5cf6', 'Questions and uncertainties'),
    ('research',      '#3b82f6', 'Research notes'),
    ('refactor',      '#ec4899', 'Code refactoring ideas'),
    ('performance',   '#f97316', 'Performance improvements'),
    ('documentation', '#06b6d4', 'Documentation related'),
    ('security',      '#dc2626', 'Security concerns'),
    ('feature',       '#22c55e', 'Feature requests')
ON CONFLICT (name) DO NOTHING;

-- ============================================================================
-- Migration complete
-- ============================================================================

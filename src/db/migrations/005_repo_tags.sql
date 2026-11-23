-- Manual tags on repos, so a repo can be tagged before it's cloned. A clone's manual tags
-- are its own (clone_tags) plus its repo's.
CREATE TABLE repo_tags (
    repo_id  INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    tag_id   INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (repo_id, tag_id)
);

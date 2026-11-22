-- Repositories the app knows about on GitHub (or another host).
-- Identity is host + owner + name, compared case-insensitively like GitHub does.
CREATE TABLE repos (
    id              INTEGER PRIMARY KEY,
    host            TEXT    NOT NULL COLLATE NOCASE,
    owner           TEXT    NOT NULL COLLATE NOCASE,
    name            TEXT    NOT NULL COLLATE NOCASE,
    -- GitHub's node ID, so a rename or transfer updates this row instead of adding a new one.
    github_node_id  TEXT    UNIQUE,
    description     TEXT,
    language        TEXT,
    topics          TEXT    NOT NULL DEFAULT '[]',   -- JSON array of strings
    is_fork         INTEGER NOT NULL DEFAULT 0,
    parent_repo_id  INTEGER REFERENCES repos(id) ON DELETE SET NULL,
    is_archived     INTEGER NOT NULL DEFAULT 0,
    is_private      INTEGER NOT NULL DEFAULT 0,
    ssh_url         TEXT,
    pushed_at       TEXT,
    ignored         INTEGER NOT NULL DEFAULT 0,
    last_listed_at  TEXT,                            -- last sync that listed this repo in any source
    UNIQUE (host, owner, name)
);

-- Why a repo is in the list. A repo can have several sources.
CREATE TABLE repo_sources (
    repo_id  INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    source   TEXT    NOT NULL CHECK (source IN
                 ('owned', 'org_member', 'collaborator', 'contributed', 'manual_repo', 'manual_owner')),
    PRIMARY KEY (repo_id, source)
);

-- Orgs or users whose every repo was added by hand.
CREATE TABLE manual_owners (
    login  TEXT PRIMARY KEY COLLATE NOCASE
);

-- One decision per fork/parent pair.
CREATE TABLE fork_links (
    fork_repo_id    INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    parent_repo_id  INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    decision        TEXT    NOT NULL DEFAULT 'undecided'
                        CHECK (decision IN ('undecided', 'linked', 'separate')),
    PRIMARY KEY (fork_repo_id, parent_repo_id)
);

-- Folders under the clone root that hold a git repository.
CREATE TABLE clones (
    id       INTEGER PRIMARY KEY,
    path     TEXT    NOT NULL UNIQUE,                -- relative to the clone root, e.g. 'r/react~2'
    repo_id  INTEGER REFERENCES repos(id) ON DELETE SET NULL  -- the main remote's repo
);

CREATE TABLE clone_remotes (
    clone_id     INTEGER NOT NULL REFERENCES clones(id) ON DELETE CASCADE,
    remote_name  TEXT    NOT NULL,
    url          TEXT    NOT NULL,
    repo_id      INTEGER REFERENCES repos(id) ON DELETE SET NULL,
    PRIMARY KEY (clone_id, remote_name)
);

CREATE TABLE tags (
    id    INTEGER PRIMARY KEY,
    name  TEXT    NOT NULL UNIQUE COLLATE NOCASE
);

-- Manual tags only. Rule tags are computed when queried.
CREATE TABLE clone_tags (
    clone_id  INTEGER NOT NULL REFERENCES clones(id) ON DELETE CASCADE,
    tag_id    INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (clone_id, tag_id)
);

CREATE TABLE rules (
    id                INTEGER PRIMARY KEY,
    position          INTEGER NOT NULL,
    field             TEXT    NOT NULL,
    pattern           TEXT    NOT NULL,
    tag_template      TEXT    NOT NULL,
    case_insensitive  INTEGER NOT NULL DEFAULT 1,
    enabled           INTEGER NOT NULL DEFAULT 1
);

-- "This rule doesn't apply to this clone."
CREATE TABLE rule_exceptions (
    rule_id   INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
    clone_id  INTEGER NOT NULL REFERENCES clones(id) ON DELETE CASCADE,
    PRIMARY KEY (rule_id, clone_id)
);

CREATE TABLE run_history (
    id            INTEGER PRIMARY KEY,
    command       TEXT    NOT NULL,
    started_at    TEXT    NOT NULL,
    repo_count    INTEGER NOT NULL,
    ok_count      INTEGER NOT NULL DEFAULT 0,
    failed_count  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE snippets (
    id       INTEGER PRIMARY KEY,
    name     TEXT    NOT NULL,
    command  TEXT    NOT NULL
);

INSERT INTO snippets (name, command) VALUES
    ('Fetch all remotes',  'git fetch --all --prune'),
    ('Pull (fast-forward only)', 'git pull --ff-only'),
    ('Short status',       'git status --short'),
    ('Last commit',        'git log -1 --format=''%cr %s''');

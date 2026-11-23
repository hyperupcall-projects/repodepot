-- Every fork is linked or separate; nothing is left undecided. `chosen` marks a decision
-- made by hand. The rest follow the default, which the app keeps current after every sync
-- and scan: linked, unless the fork has a clone and none of its clones has a remote for
-- the parent (it's set up on its own, as a separate repo).
CREATE TABLE fork_links_new (
    fork_repo_id    INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    parent_repo_id  INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    decision        TEXT    NOT NULL DEFAULT 'linked'
                        CHECK (decision IN ('linked', 'separate')),
    chosen          INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (fork_repo_id, parent_repo_id)
);

INSERT INTO fork_links_new (fork_repo_id, parent_repo_id, decision, chosen)
SELECT fork_repo_id, parent_repo_id,
       CASE decision WHEN 'separate' THEN 'separate' ELSE 'linked' END,
       decision <> 'undecided'
FROM fork_links;

DROP TABLE fork_links;
ALTER TABLE fork_links_new RENAME TO fork_links;

UPDATE fork_links SET decision = CASE
    WHEN EXISTS (SELECT 1 FROM clones c
                 WHERE c.repo_id = fork_links.fork_repo_id AND c.missing_since IS NULL)
     AND NOT EXISTS (SELECT 1 FROM clones c JOIN clone_remotes r ON r.clone_id = c.id
                     WHERE c.repo_id = fork_links.fork_repo_id AND c.missing_since IS NULL
                       AND r.repo_id = fork_links.parent_repo_id)
    THEN 'separate' ELSE 'linked' END
WHERE NOT chosen;

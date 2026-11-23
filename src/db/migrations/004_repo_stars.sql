-- GitHub's star count, as of the last sync that saw the repo. NULL until then.
ALTER TABLE repos ADD COLUMN stars INTEGER;

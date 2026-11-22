-- The remote picked by hand as a clone's main remote, when the usual rule
-- (default remote name, then 'origin', then the only remote) can't decide.
ALTER TABLE clones ADD COLUMN main_remote TEXT;

-- Set when a scan didn't find the folder. Rows aren't deleted automatically, so a folder
-- that's moved or briefly unavailable keeps its tags.
ALTER TABLE clones ADD COLUMN missing_since TEXT;

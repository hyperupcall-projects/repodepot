# depository — plan

A Rust + egui desktop app (Linux first) for managing the GitHub repositories I care about: finding them, cloning them into a predictable folder layout, tagging them, and opening them or running commands across groups of them. State is kept in SQLite.

## Decisions at a glance

| Topic | Decision |
|---|---|
| Folder layout | `<root>/<first letter of repo name>/<repo name>` |
| Name clashes | Add `~2`, `~3`, … to the folder (`r/react~2`). The UI always shows `owner/name`. |
| Identity in the database | Host + owner + name (`github.com` + `facebook` + `react`) |
| Repo sources | Owned, org member, collaborator, contributed, added by hand (one repo or a whole org/user) |
| Cloning | SSH only. The default remote is named `me` (configurable). |
| Forks | One table of every fork and its upstream. Each fork is either linked to its parent or kept separate: linked by default, and you can switch any of them. |
| Updating | No automatic fetch or pull. You update through the command runner. |
| Import | Moves repos into the layout |
| Tags | On clones, and on repos that aren't cloned yet (a clone keeps its repo's tags). Manual and rule-made tags are tracked separately; rules can have exceptions. |
| Opening in one window | Only editors that support workspaces are offered |
| Command runner | Run on a group, then pick a subset and run again in its own tab, with its own sorting and filtering |

## Terms

- **Repo**: a repository on GitHub (or another host) that the app knows about.
- **Clone**: a folder under the clone root that holds a git repository.
- **Source**: why a repo is in the list (owned, contributed, …). A repo can have several.
- **Fork family**: a parent repo plus the forks of it that the app knows about.
- **Selection**: the rows selected in a Repositories view. Opening and running act on the clones it covers.
- **Run**: one command run over a set of clones, shown in its own tab.

## Settings and storage

- **Settings** live in `~/.config/depository/config.toml`. You can edit them in the app's Settings tab or by hand:
  - clone root
  - default remote name (`me`)
  - upstream remote name (`upstream`)
  - SSH host aliases that mean github.com (for example `github-work`)
  - which sources are switched on
  - clone defaults: partial clone (`--filter=blob:none`) on/off, `--recurse-submodules` on/off
  - runner: shell (default `$SHELL`), how many commands run at once (default 8), timeout, line limit per repo (default 10,000)
  - launchers (editor command templates)
  - how many windows can open before the app asks first (default 10)
  - theme: system (default), light or dark. System follows the desktop's `color-scheme` from the XDG desktop portal, live; with no answer it's dark
- **State** lives in `~/.local/share/depository/depository.db` (SQLite in WAL mode, with versioned migrations).
- **Workspace files** live in `~/.local/share/depository/workspaces/`.
- **GitHub token**: the app looks in the OS keyring first (KWallet, through the Secret Service API), then `GITHUB_TOKEN`, then `gh auth token`. The token is never written to the database, the config file, or any clone URL.
  - Use a classic token with the `repo` and `read:org` scopes, or gh's own token. Fine-grained tokens only cover one owner, so they miss contributions to other orgs.
  - Orgs that use SAML single sign-on need the token authorized for that org. Show this as a hint when an org returns an SSO error.

## Part 1 — Repositories

### 1.1 Finding repos (GitHub sync)

Sync runs when you click Sync (and optionally on startup). Results are cached in SQLite.

| Source | How (GraphQL) |
|---|---|
| Owned / org member / collaborator | `viewer.repositories(ownerAffiliations: [OWNER, ORGANIZATION_MEMBER, COLLABORATOR])` |
| Contributed | For each year in `contributionsCollection.contributionYears`: `contributionsCollection(from, to)` → commit, PR, issue and review contributions by repository |
| Added by hand: one repo | `repository(owner, name)`, from `owner/name` or a URL |
| Added by hand: org or user | `repositoryOwner(login).repositories`, which gives every repo of that owner |

For each repo, store: GitHub node ID, owner, name, description, main language, topics, whether it's a fork (and its parent), archived, private, SSH URL, last push date, star count, and which sources listed it. Sources show as badges.

- The node ID is stored so that a renamed or transferred repo updates its existing row instead of becoming a new one.
- A repo that no longer appears in any source is flagged "no longer listed". It is never deleted.
- After each sync, show the remaining API rate limit.

### 1.2 Folder layout

- **Letter folder**: the first character of the repo name, lowercased. Names that start with anything other than a letter or digit (`.github`, `_foo`, `-foo`) go in `_/`.
- **Folder name**: the repo name, keeping its case. If that folder already exists (compared ignoring case), add `~2`, `~3`, … so you get `r/react`, `r/react~2`.
  - GitHub doesn't allow `~` in repo names, so a suffix can never clash with a real repo.
  - Shells only expand `~` at the start of a word, so `cd r/react~2` works.
- The path is chosen once, when the repo is cloned, and saved. It is never recomputed.
- The UI always shows the repo as `owner/name`. The folder path only appears in its own column and in tooltips.

### 1.3 Remote List and Ignored

Every relevant repo that isn't cloned and isn't ignored waits in the **Remote List** until you act on it. Nothing is cloned automatically. It's the first view of the Repositories tab, left of **Local Clones**.

- **Remote List columns**: `owner/name`, source badges, fork/archived/private badges, language, GitHub stars, last push, description.
- Click a column title to sort by it; click again to reverse. Stars and last push start biggest/newest first. Rows missing the value (no language, no tags, stars not fetched yet) stay last either way.
- **Tags column**, and **Add tag** / **Remove tag** for the selected repos (see Part 2).
- **Tag filter** (here, in Ignored and in Local Clones): a tag query such as `work AND NOT old`, or `untagged` for repos with no tags. The ⏷ menu next to it picks a tag, or its opposite (see 2.4).
- **Actions** (on one repo or many selected, see 1.10): **Clone**, **Ignore**.
- **Ignored list**: same columns, with an **Un-ignore** action.
- **Hide ignored** (on by default): the Remote List leaves out ignored repos. Untick it to show the ones that would otherwise be listed, marked "ignored", with an **Un-ignore** action next to Ignore.
- Every list can be searched and filtered (see 1.9).

### 1.4 Fork families

When a relevant repo is a fork, the app fetches its parent too. The **Forks** view is a table with one row per fork–parent pair: **Fork Repository**, **Upstream Repository**, a **Linked** / **Separate** button (click to switch), and **Notes** (whether the fork is cloned and with an upstream remote, whether the upstream is cloned, whether it's listed or not in your sources, and whether the decision is the default or set by you). Click a column title to sort by it; click again to reverse. Each pair is either:

| Decision | Meaning |
|---|---|
| Linked | A fork for contributing. There is one clone of the fork, with remote `me` → fork and `upstream` → parent. The parent doesn't need its own clone or an Ignore. |
| Separate | A hard fork. The fork and the parent go through the Remote List independently, and the fork's clone only has the `me` remote. |

There is no undecided state. A pair is **Linked by default**, unless the fork has a clone and none of its clones has a remote for the parent: then it's set up on its own, and the default is **Separate**. The default is recomputed after every sync and scan, so it follows the clones on disk. Clicking the button decides it by hand, and from then on syncs and scans leave it alone.

A repo counts as cloned if it is the main remote of a clone, or if it is the parent of a Linked fork that is cloned.

### 1.5 Cloning

- Clone with `git clone -o <default remote name> git@github.com:owner/name.git <path>`. Use the clone defaults (partial clone, submodules) unless they're changed in the clone dialog.
- For a Linked fork, also run `git remote add <upstream name> git@github.com:<parent>.git`.
- Clones go through the job engine (see Architecture), so their progress and errors show in the runner like any other run.
- If a clone fails, its half-made folder is deleted and its path is freed.

### 1.6 Local scan and status

The app scans the clone root (two levels deep) on startup and when you ask. The folders on disk decide what is cloned. The database keeps your choices: ignores, tags, rules, assigned paths and fork decisions.

- **Reading remotes**: one `git remote -v` per clone, which already applies `insteadOf` rewrites.
- **Moved or deleted folders**: a folder that disappears is marked missing instead of being removed from the database, so its tags survive. If a new folder appears whose main remote matches exactly one missing entry, it's treated as that entry moving. Missing entries are listed in the Problems view with a **Forget** button.
- **URL forms** understood: `git@host:o/r(.git)`, `ssh://git@host/o/r`, `https://host/o/r(.git)`, plus the SSH host aliases from settings.
- **Main remote** of a clone: the remote with the default remote name, else `origin`, else its only remote. If none of those apply, you pick one (and you can override the choice for any clone with several remotes).
- **Status column** (no network access): branch, uncommitted changes, ahead/behind (as of the last fetch), detached HEAD, no upstream branch.
  - It comes from one `git status --porcelain=v2 --branch` per clone.
  - It refreshes after every scan and after every run.
- **Hide ignored** (on by default, shared with the other views): Local Clones leaves out clones whose main repo is ignored. Untick it to show them, marked "ignored".

**Problems view**: folders that don't match a relevant repo, grouped by problem.

| Problem | Action offered |
|---|---|
| No remotes | — |
| Remote on another host (GitLab, Codeberg, …) | — (still a clone, and can be tagged) |
| GitHub remote, but the repo isn't in any source | Add by hand |
| GitHub remote returns 404 (deleted, or no access) | — |
| Repo was renamed or moved on GitHub (found through the REST API, which follows redirects) | Update the remote URL; move the folder to the new name |
| Not a git repo, or not in the layout (wrong depth or wrong letter folder) | Import (move) |

Relevant repos that aren't cloned are the Remote List (1.3).

### 1.7 Import (move)

1. Pick a folder. The app searches it for git repos. It doesn't look inside a repo once it finds one, and it skips folders like `node_modules`, `target` and `.venv`.
2. A preview table shows, for each repo found:
   - its `owner/name` (from its main remote, or the folder name if it has no remote)
   - its current path → its new path in the layout
   - any warnings
3. A checkbox, on by default, renames `origin` to the default remote name.
4. On confirm, the moves run in the Runner, two at a time, using `mv`: it renames on the same disk and copies then deletes across disks, and never overwrites anything. Repos that have linked worktrees get `git worktree repair` afterwards. A move that fails or is cancelled is left as it is for you to look at; nothing is deleted automatically.
5. The Problems view links here: "Move into place…" for clones outside the layout, and "Look for repos inside" for folders that aren't repos.

Breaking absolute paths inside a repo (Python venvs and similar) is accepted.

### 1.8 Deleting a clone

- Before deleting, the app warns about uncommitted changes, unpushed commits and stashes, and needs you to confirm past them.
- The folder goes to the trash (`trash` crate), never `rm -rf`.
- The repo goes back to the Remote List. Its manual tags are removed, and the confirmation says so.

### 1.9 Searching and filtering

Every view of the Repositories tab except Tags and Import has two rows of filters between the tabs and the list, set off from the tabs by a line. Tags only has its own row: the shared filters are about repos, not rules.

**The shared row** stays the same as you switch views:

- **Search**: words separated by spaces, all of which must match. It looks in the owner and the name by default; the **Owner**, **Name** and **Description** toggles pick any combination (at least one stays on). With owner and name both on, `facebook/re` matches across the slash. For clones, Name also matches the folder name (`react~2`).
- **Language**: GitHub's main language, exactly, ignoring case. Separate several with commas (`Rust, Go`). The ⏷ menu ticks languages from every synced repo; a name no repo has is flagged.
- **Hide ignored** (on by default; not shown in Ignored, where everything is ignored) and **Hide archived** (off by default).
- Clones, forks and problems are matched through their repo, using what the last sync said about it. Forks go by the fork, but the search also looks at the upstream.

**The view's own row** is a tinted strip that starts with the view's name:

| View | Filters |
|---|---|
| Remote List, Ignored | Source (any, owned, org member, collaborator, contributed, added by hand), tag query |
| Local Clones | Show (all, uncommitted changes, ahead/behind, stashes, problems), branch, tag query |
| Forks | Show (all, linked, separate, cloned, linked but no upstream remote, set by you) |
| Problems | Show (all, or one problem group) |
| Tags | Search: part of a rule's pattern, of the tag it adds, or of a tag it gives right now (`lang:rust` finds `lang:$1`), ignoring case. Yes/no rules have no pattern to search. |

- **Branch**: part of a branch name, ignoring case. Separate several with commas to match any of them; start one with `!` to leave out branches that have it (`!main, !master` lists clones on neither). The ⏷ menu lists the branches clones are on, most used first.
- **Clear filters** shows up once anything narrows the view. It empties the search, language, branch and tag fields and resets the drop-downs, and leaves the toggles alone. In Tags it only empties the rules' search.
- In a narrow window the rows wrap, keeping each label with its field.

### 1.10 Selecting rows and acting on them

Every view of the Repositories tab except Import has rows to select: repos in the Remote List, Ignored and Forks (a fork's row stands for the fork), and clones in Local Clones and Problems. Each view keeps its own selection.

- **Click** selects just that row, even if others were selected. **Ctrl-click** (Cmd-click on macOS) adds a row or takes it out. **Shift-click** selects every row from the last one clicked to this one; with Ctrl (Cmd) held too, it adds that range to the selection.
- Only rows on screen stay selected. A row the filters hide drops out of the selection, so an action never reaches rows you can't see.
- **Ctrl+A** (Cmd+A on macOS) selects every row shown; **Esc** deselects them all. While a text field has the keyboard, both keys are the field's, and Esc closes an open menu first.
- In Problems, a clone with two problems is listed, and selected, twice. Missing folders and folders that aren't git repos can't be selected. **Details** on a row shows the clone in Local Clones, clearing that view's own filters (Show, branch, tags) if they would hide it, and shows the panel if it was hidden.
- Buttons inside a row (Linked / Separate, Use the new URL, …) work without selecting it.

A panel on the right of the list, under the filters, acts on the selection; nothing sits between the filters and the list. **Hide** in the panel, or the **Selection (n)** button at the end of the view's filter row, hides it and shows it again. Hiding it leaves the selection as it is, and it stays hidden in every view until shown again. Its width can be dragged.

| Part | What it does |
|---|---|
| The count, **Hide**, **Select all shown**, **Deselect** | With nothing selected, how to select rows. |
| **Actions**: the view's own | Remote List: **Clone**, **Ignore**, and **Un-ignore** while Hide ignored is off. Ignored: **Un-ignore**. These clear the selection, since the rows leave the list. |
| **Actions**: **Open each**, **Open together**, **Run command…** | On the selected clones, or the clones of the selected repos (2.5). Repos that aren't cloned are left out, and the panel says how many; with none cloned, the buttons are off and say why. |
| **Tags**: **Tag** field, **Add tag**, **Remove tag** | On the selected rows (2.1). Tagging keeps the selection, so several tags can go on the same rows. In Local Clones, with one clone selected, its tags are listed above the field: a manual tag can be removed, a rule's tag excepted. |
| Details | Local Clones, with one clone selected: its folder, remotes (and the main remote picker), status and problems. |

## Part 2 — Tags, groups and actions

Clones can be tagged, and so can repos in the Remote List that aren't cloned yet.

- Manual tags given to a repo are stored on the repo (`repo_tags`); a clone's manual tags are its own plus its repo's, so a repo tagged before cloning keeps those tags once it's cloned. Removing a tag from a clone removes it from the clone's repo too.
- Rules apply to repos that aren't cloned as well, using what the last sync said about them. Exceptions are only for clones.

### 2.1 Manual tags

Add and remove tags for the selected rows of any Repositories view, from the panel on the right (1.10). Tagging a repo tags its clones too. Any name works, and `:` can be used for namespaces (`org:myorg`, `lang:rust`). Names are lowercased and spaces become `-`; `and`, `or`, `not`, `untagged`, `*` and parentheses are kept for queries.

### 2.2 Rules (automatic tags)

A rule says: **if a field matches a pattern, add a tag**.

Rules live in the **Tags** view of the Repositories tab, between Problems and Import. It's a table with one row per rule: **On** (a tickbox), **When** (the field, or the whole condition for yes/no fields), **Pattern**, **Tag** (followed by the tags it gives right now when a capture group makes them differ), how many **Clones** and **Remote List** repos it tags, and its **Exceptions**, with **Edit** and **Delete**. Click a column title to sort by it; click again to reverse. It starts sorted by tag; counts start biggest first, and yes/no rules (no pattern) stay last when sorting by pattern. **New rule** and Edit open the editor above the table, and the row being edited is highlighted.

| Field | Match type |
|---|---|
| `owner/name` (one string, so one regex can target either part) | regex |
| owner | regex |
| name | regex |
| language | regex |
| topics | regex; matches if any topic matches |
| fork / archived / private | yes / no |

- **Regex syntax**: Rust's `regex` crate, which has no lookarounds or backreferences. Case-insensitive by default.
- **Capture groups in tags**: `^([^/]+)/` with the tag `org:$1` creates one tag per org.
- **Live preview**: the rule editor lists matching clones as you type.
- **Evaluated live**: rules are applied when you query, not saved as tags. Editing a rule updates every clone immediately.
- **Non-GitHub clones** only have owner and name (from the remote URL, or the folder name if there's no remote). Rules on the yes/no fields never match them.

### 2.3 Manual and automatic tags

- Manual and automatic tags are tracked separately and look different. Manual tags are filled chips; automatic tags are outlined chips, with a tooltip naming the rule.
- A clone can have the same tag both ways. Both are shown.
- Removing a manual tag only removes the manual one. If a rule still matches, the automatic tag stays.
- **Exceptions**: on an automatic tag, "Add exception" stops that rule from applying to that clone.
  - Each rule lists its exceptions. Removing an exception lets the rule apply again.
  - Exceptions show crossed out in the rule's live preview.

### 2.4 Groups

A group is a tag query in the Repositories tab; there's no separate view for it. The Tags view only holds the rules (2.2).

- The **Tags** filter in the Remote List, Ignored and Local Clones combines tags: `rust AND work`, `rust OR go`, `work AND NOT archived`, with parentheses. Words in a query are tag names, side-by-side words mean AND, `*` means everything, and `untagged` means no tags at all. A query naming a tag no repo has is flagged.
- Its ⏷ menu lists every tag with how many of the view's repos or clones have it, and `untagged` with how many have none.
- **Select all shown** makes the group the selection. Ctrl-click and Shift-click adjust it, and the search narrows it, without touching tags. **Add tag** saves the selection as a tag.

### 2.5 Actions on the selection

From the panel on the right of each list (1.10):

- **Open each**: one window per clone. Lists every launcher that has a per-repo command.
- **Open together**: one window for all of them. Lists only launchers that support workspaces.
- **Run command…**: opens the Runner with the selected clones as its targets.

The app asks before opening more windows than the threshold in settings.

**Launchers** are command templates using `{path}`, `{paths}` and `{workspace}`.

- Presets ship for the editors below, and any other app can be added in settings.
- Only installed launchers are shown, whether found on `PATH` or as a Flatpak.
- Launched apps are detached (their own session, no stdin/stdout), so quitting depository doesn't close them.

| Launcher | Open each | Open together |
|---|---|---|
| VSCode / VSCodium / Cursor | `code -n {path}` (`codium`, `cursor`) | `code {workspace}` (a `.code-workspace` file) |
| Zed | `zed -n {path}` | `zed -n {paths}` |
| Sublime Text | `subl -n {path}` | `subl --project {workspace}` (a `.sublime-project` file) |
| JetBrains IDEs (IntelliJ, RustRover, PyCharm, CLion, GoLand, WebStorm) | `idea {path}` and so on | — |
| Kate | `kate -n {path}` | — |
| Dolphin | `dolphin --new-window {path}` | — |
| Neovim / Helix (in a terminal) | `konsole --workdir {path} -e nvim .` (or `hx .`) | — |
| Emacs | `emacs {path}` | — |
| Terminal | `konsole --workdir {path}` | — |

The exact flags get checked against each tool during the build.

**Workspace files**:

- They're stored at a stable path, not in `/tmp`. VSCode remembers open files and layout per workspace path, so reopening the same group brings them back.
  - If the selection is every row of a tag query, with nothing else narrowing the view: `workspaces/<query>.code-workspace`.
  - Otherwise: `workspaces/set-<hash of the clones>.code-workspace`.
- Each folder entry gets `"name": "owner/name"`, so the editor shows the real name instead of `react~2`.
- Settings has a button to delete workspace files.

## Part 3 — Command runner

### 3.1 Runs

- A run is a command plus a set of clones. Each run opens in its own tab.
- Ways to start a run:
  - from a selection in any Repositories view (1.10). The run is named after the tag query when the selection is all of it, otherwise after the view (`Local Clones selection`).
  - on all clones
- Clone jobs from Part 1 also appear here as runs.
- Commands are saved as history (command, time, number of repos, how many passed and failed) and as named snippets.
- Starter snippets:
  - `git fetch --all --prune`
  - `git pull --ff-only`
  - `git status --short`
  - `git log -1 --format='%cr %s'`

### 3.2 Running

- **Command**: runs as `$SHELL -c '<command>'` (the shell is configurable), with the clone as the working directory. Interactive aliases aren't loaded, and the UI says so.
- **No input**: stdin is `/dev/null` and `GIT_TERMINAL_PROMPT=0` is set, so nothing waits for input.
- **Environment variables** for each clone:
  - `DEPO_REPO` (`owner/name`)
  - `DEPO_PATH`
  - `DEPO_TAGS` (comma-separated)
- **Limits**: a cap on how many commands run at once (from settings, changeable per run) and an optional timeout.
- **Cancel**: each process gets its own session (and so its own process group). Cancelling (one repo or all) sends SIGTERM to the group, then SIGKILL after a few seconds, so child processes die too. Having no terminal also means ssh asks for key passphrases through your desktop's askpass dialog instead of the terminal the app was started from.
- **After a run** finishes, the status column refreshes for those clones.

### 3.3 Viewing results

- **Repo table**: status (queued / running / ✓ / ✗ exit N / timed out / cancelled), `owner/name`, exit code, duration, line count, and search match count. Sort by any column.
- **Output pane** for the selected repo(s):
  - stdout and stderr are interleaved in the order they arrived, with stderr marked
  - only the visible lines are drawn, so huge outputs stay fast
- **Group identical output**: repos with exactly the same output collapse into one row (`×55 · Already up to date.`). Expand the row to see which repos it covers.
- **Search** across every output in the tab: plain text or regex, with a case toggle, highlighted matches, and next/previous match across repos.
- **Filters**:
  - by status
  - hide repos with no output
  - only repos whose output matches (or doesn't match) the search
  - by name
- **Colors**:
  - A toggle sets `CLICOLOR_FORCE=1`, which makes most tools keep their color codes.
  - SGR color codes are rendered; other escape codes are stripped.
  - `\r` overwrites the current line, so simple progress output stays readable.
- **Line limit**: each repo keeps at most the line limit from settings. Older lines are dropped, with a marker where they were cut.

### 3.4 Subsets

- **Selecting repos**: tick them in the table, use "select all visible" (after filters and search), or use "select failed".
- **Run on selection**: opens a new tab with just those repos.
  - It has its own sorting, filters and search.
  - A breadcrumb leads back to where it came from (`rust (24) › failed (5)`).
- **Re-running**: **Re-run** repeats the whole tab, and **Re-run failed** repeats only the failed rows. Both update the tab in place.
- **Clearing**: **Clear all outputs** clears every tab at once. You can also clear or close a single tab.

## Architecture

### Threads

- egui runs on the main thread and never blocks. All slow work (git, network, database, processes) runs on a background tokio runtime.
- **UI → backend**: commands go over a channel (`Sync`, `Scan`, `Clone`, `Run`, `Cancel`, …).
- **Backend → UI**: events come back over a channel, followed by `ctx.request_repaint()`. While output is streaming, repaints are limited to about 30 per second.
- One SQLite connection, owned by the backend.

### Log

- Info, warnings and errors go to one log for the session, kept in memory (the latest 1,000). The backend sends entries as events, and the UI logs what other events mean: database opened, signed in, scan and sync results, failures. Every entry is also written to stderr through `tracing`.
- The status bar shows the latest entry, plus how many warnings and errors came since the log was last open. Clicking it opens the log in a sidebar over the right edge of the window, newest first, with a toggle for each level. Esc or a click outside closes it.
- Finished work is reported in the log, not next to the buttons that started it: the Repositories toolbar only shows a scan or sync while it runs.

### One job engine

Clones and your own commands go through the same engine. It runs a process in N folders with a limit on how many run at once, live output, status, timeout and cancel. That way Part 1 gets the runner's output view at no extra cost. Status scans don't use it: they make quick, parsed git calls whose output nobody needs to read.

### Git

Always run the `git` command rather than a git library. That way your SSH config, SSH agent, `insteadOf` rules, LFS and hooks all work.

### Crates

- **UI**: `eframe`/`egui`, plus `egui_extras` for tables
- **Background work and HTTP**: `tokio`, `reqwest` (rustls)
- **Data**: `serde`, `serde_json`, `toml`, `rusqlite` (bundled) + `rusqlite_migration`
- **Matching**: `regex`, `nucleo-matcher`
- **System**: `keyring`, `directories`, `trash`, `nix` (process groups and signals), `zbus` (desktop portal theme)
- **Errors and logging**: `anyhow`, `tracing`

### Modules

```
src/
  main.rs
  app.rs              top-level tabs, wiring between UI and backend
  config.rs           config.toml
  system_theme.rs     desktop light/dark preference from the XDG portal
  db/                 connection, migrations, queries
  github/             GraphQL client, queries, sync, REST rename check
  git/                remote URL parsing, scan, status, clone, import
  layout.rs           letter folder + ~N path assignment
  log.rs              log entries and levels
  time.rs             UTC dates
  jobs/               engine, process groups, output buffers, ANSI parsing
  tags/               rules, exceptions, query parser
  launchers.rs        presets, detection, workspace files
  ui/                 repos, remote list, ignored, forks, problems, import,
                      selection and its panel, tags (rules), runner,
                      settings, log
```

### Database (sketch)

```
repos            id, host, owner, name, github_node_id, description, language,
                 topics, is_fork, parent_repo_id, is_archived, is_private,
                 ignored, last_listed_at
                 UNIQUE (host, owner, name), case-insensitive
repo_sources     repo_id, source        -- owned | org_member | collaborator | contributed | manual_repo | manual_owner
manual_owners    login                  -- orgs/users added by hand
fork_links       fork_repo_id, parent_repo_id, decision,  -- linked | separate
                 chosen                                   -- set by hand; else the default
clones           id, path UNIQUE, repo_id NULL            -- path is relative to the clone root; repo_id is the main remote's repo
clone_remotes    clone_id, remote_name, url, repo_id NULL
tags             id, name UNIQUE
clone_tags       clone_id, tag_id                         -- manual tags only
rules            id, position, field, pattern, tag_template, case_insensitive, enabled
rule_exceptions  rule_id, clone_id
run_history      id, command, started_at, repo_count, ok_count, failed_count
snippets         id, name, command
```

## Build order

Each step ends with something usable.

1. **Skeleton**: cargo project, window with tabs, config file, database and migrations, token lookup.
   - Done when settings are saved between launches and the app shows "signed in as …" with the rate limit.
2. **Local scan**: walk the clone root, read and parse remotes, status column, the problem categories that need no network.
   - Done when an existing folder of clones is listed with its status.
3. **Job engine and basic runner**: run a command over all clones with live output, a limit on how many run at once, cancel and timeout.
   - Done when `git status --short` runs over every clone and cancels cleanly.
4. **GitHub sync, Remote List and cloning**:
   - sources and the metadata cache
   - Remote List, Ignored list and adding repos by hand
   - fork families
   - path assignment and SSH clones through the job engine
   - the problem categories that need the network
5. **Import**: search, preview, move, remote rename, worktree repair.
6. **Tags and groups**:
   - manual tags, rules, exceptions and the live preview
   - tag sidebar, query bar, working set and Save as tag
   - Run command into the runner
7. **Launchers**: presets, detection, Open each, Open together, workspace files.
8. **Runner features**:
   - identical-output grouping, search, filters and sorting
   - subsets in new tabs, Re-run and Re-run failed, Clear all
   - history and snippets
   - colors, `\r` handling and the line limit

## Not doing (by design)

- No automatic fetch or pull. Updating is done with a runner snippet.
- No tags on repos that aren't cloned.
- SSH only for cloning.

## Maybe later

- A small CLI that uses the same database, such as `cd $(depo path react)` or `depo run -t rust -- cargo update`.
- A Ctrl+K command palette.
- GitLab and Codeberg as sources (the `host` column already allows it).
- Running commands in a real terminal (PTY) for tools with fancy progress bars.

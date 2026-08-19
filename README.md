# paddock

An inbox host. TUI and web.

## nouns

- **item** — one thing that arrived, stripped of its source's shape
- **source** — a plugin that admits items
- **label** — a mark a classifier put on an item
- **inbox** — a named question over the pile (labels + sources + sort), not an account and not a folder

Items may have parts (text, file, image, audio, video) and an optional thread. `body` is the list preview.

Actors and cites are kernel: an item can have `from` / `to` (a person, a group, or a list) and may cite another item. A group is an actor; a reply cites an item (`in_reply_to` + thread).

Compose is a verb: `c` / `:compose` opens a title+body draft, `R` / `:reply` keeps the thread (or starts one from the parent), and `:send` / `:w` (or Ctrl-s) persists. The source writes a file if it can; `:new TITLE` stays the one-shot create.

Inboxes nest. A child is a tighter question over its parent's matched items. Classifiers are a plugin role owned by an inbox; they run when an item enters that inbox, then children re-evaluate.

A label change re-runs classify (classify-on-enter) so a newly matching child can fire. `all/todo` is the todo list — same machinery, no extra feature.

## install

```
cargo install --path .
```

## commands

```
paddock init        # XDG (or existing .paddock / PADDOCK_DIR)
paddock init --here # ./.paddock in cwd
paddock             # TUI; init if needed
paddock pull        # pull sources, classify new items
paddock serve       # http://127.0.0.1:4736
paddock context     # dump this host for an agent
paddock --remote    # ssh (HOST, PADDOCK_REMOTE, or config remote)
paddock --local     # this machine even if remote is set
```

Drop a file in the incoming directory, then `paddock pull` (or just wait — the TUI and `serve` watch that directory).

## paths

| | XDG | `.paddock` / `$PADDOCK_DIR` |
|---|---|---|
| config | `~/.config/paddock/config.toml` | `$root/config.toml` |
| themes | `$config_dir/themes/<name>.toml` | `$root/themes/<name>.toml` |
| store | `~/.local/share/paddock/paddock.db` | `$root/paddock.db` |
| incoming | `~/.local/share/paddock/incoming` | `$root/incoming` |

Resolution: `PADDOCK_DIR` (that directory is the host root), else walk up from cwd for a `.paddock/` directory, else XDG. `paddock init` stays XDG unless a `.paddock` is already in the walk or `PADDOCK_DIR` is set; `init --here` creates `./.paddock`. You can list several `[[source]]` blocks; they share one store.

## config

```toml
[[inbox]]
name = "all"

[[inbox.classifier]]
id = "flag-rfc"
kind = "regex"
pattern = "(?i)rfc"
label = "rfc"

[[inbox.classifier]]
id = "flag-todo"
kind = "regex"
pattern = "(?i)todo"
label = "todo"

[[inbox.inbox]]
name = "later"
labels = ["later"]

[[inbox.inbox]]
name = "todo"
labels = ["todo"]

[[inbox.inbox]]
name = "cal"
view = "calendar"
timed = true

[[source]]
id = "incoming"
kind = "fs"
path = "~/.local/share/paddock/incoming"

# [[source]]
# id = "feed"
# kind = "rss"
# url = "https://example.com/feed.xml"

# [[source]]
# id = "cli"
# kind = "exec"
# cmd = "my-source"
# args = []
# # name = "chat"   # list label; missing uses id
# # dir = "~/.local/share/paddock"

# theme = "phosphor"

# [[inbox.classifier]]
# id = "by-script"
# kind = "script"
# script = '''
# if item.title.contains("invoice") { "money" } else { () }
# '''

# [[inbox.classifier]]
# id = "flag-later"
# kind = "script"
# label = "later"
# script = '''item.title.contains("someday")'''

# [[inbox.classifier]]
# id = "by-llm"
# kind = "llm"
# model = "llama3.2"
# provider = "ollama"
# url = "http://127.0.0.1:11434"
# labels = ["later", "todo"]
# # prompt = "prefer later unless it is actionable"

# [[inbox.inbox]]
# name = "board"
# view = "board"
# columns = ["todo", "doing", "done"]
# labels = ["todo"]
```

Optional `name` on `[[source]]` is the list label; if it is missing or empty, `id` is used.

An item matches an inbox when `(sources empty OR item.source in sources)` and `(labels empty OR item has ALL listed labels)` and (`timed` is unset/false OR the item has `start`), and it matches every ancestor. `:forget` deletes; `keep` labels survive stale cleanup; lists are queried, not loaded whole.

Classifier `kind`: `regex` (title or body match), `script` (Rhai; return a label, `()`, or `true` with `label =`), `llm` (Ollama `/api/chat` or OpenAI-compatible `/chat/completions`). `pull` and the fs watch call the model when an llm classifier is configured — that can be slow. Env: `PADDOCK_LLM_URL`, `PADDOCK_LLM_MODEL`, `PADDOCK_LLM_KEY` or `OPENAI_API_KEY`. Do not put real keys in the config.

Inbox `view` (`list`, `calendar`, `board`) and item `start`/`end` (RFC3339) are kernel fields, not extra nouns. `timed = true` keeps items that have `start`. An exec source can fill those times; fs and rss leave them empty.

`kind = "exec"` runs `{cmd} {args...} pull` (stdout: JSON array or NDJSON items) and `{cmd} {args...} send` (stdin: JSON draft; stdout: `{ "foreign_id", start?, end? }`).

Existing configs are not rewritten by `init`.

## tui

```
j/k ↑↓     move
gg / G     top / bottom
C-d / C-u  half page
C-f / C-b  page
h / l      tree / items
tab        swap pane
gt / gT    next / prev inbox
enter      read
space      toggle read
u          unread
dd         eat (mark read)
/ n N      search (title+body)
L          label (toggle + classify)
:          command
?          help
c          compose
R          reply
esc        back
q          quit (list)
```

Read: `j/k` next item, `esc`/`h` back, `:` still works, `q` quits.

### colon

```
:q :quit     quit
:pull        pull
:help        keys
:theme NAME  load $config_dir/themes/NAME.toml
:themes      list themes
:why         why this item is in this inbox
:again       reclassify
:eat         mark read
:forget      delete the current item
:bury        label later + classify → all/later
:todo        label todo + classify → all/todo
:yank        write title to $data/yank
:open        show href, or the first image/audio/video part; xdg-open if possible
:new TITLE   write $incoming/TITLE.md and admit
:compose     open compose
:reply       open compose as a reply
:send :w     persist the draft
:which       inbox path + counts
:db          store path
:only        unread-only filter
:spill       write current inbox to $data/spill.md
```

Unknown: `not an editor command: foo`.

Web uses the same key table and the same `run_verb` (endpoint `/x/{verb}`).

## theme

TOML. All slots optional; missing = carbon.

```toml
# paddock-theme 1
name = "carbon"

[colors]
bg = "#0a0a0a"
fg = "#d2d2c8"
accent = "#d8c070"
dim = "#666660"
unread = "#f2f2ea"
border = "#222220"
select = "#282828"
```

Load: `theme = "name"` in config.toml (or `:theme NAME`) → `$config_dir/themes/<name>.toml` → built-in carbon.

`paddock init` copies bundled `carbon` and `phosphor` into `$config_dir/themes/` if missing.

TUI maps slots to colors. Web sets `:root { --bg --fg --accent --dim --unread --border --select }`.

## license

MIT

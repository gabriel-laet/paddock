# Handover for agents

Read this before touching the repository. README.md says what paddock is
for a user; PROTOCOL.md is the plugin contract; this file is how the work
is done and where it stands.

## what this is

An inbox kernel with a CLI, no UI. Four nouns: item, source, label, inbox.
Everything else is either a question over the pile (an inbox), a port the
kernel calls (store, source, classifier, embedder, model), or an adapter
behind a port. Meant to be driven by hand, by scripts, and by agents; a
native app is a caller of the same library.

## layout and the lines it keeps

```
crates/kernel     paddock-kernel    pure. deps: anyhow chrono regex serde serde_json. nothing else, ever.
crates/protocol   paddock-protocol  the exec protocol's types, the Message shape, plugin helpers.
crates/paddock    paddock           adapters (sqlite, fs/exec sources, classifiers, model,
                                    embedder, mirror, agent, host) and the CLI. integration tests.
crates/app        paddock-app       the façade for native apps: Session, events, poll/watch, setup.
plugins/*         paddock-<kind>    one binary per format of the world; depend on protocol only.
plugins/mail      paddock-mail      a library: RFC 822 bytes -> Message; maildir and imap share it.
```

Rules, all checkable:

- The kernel crate's `Cargo.toml` is the purity test. No clock, file, network, or
  config format in the kernel. The host resolves every adapter up front and hands
  the kernel `Kernel { config, store, sources, classifiers, embedder, model, now }`.
- A plugin never depends on `paddock-kernel` (`cargo tree -p paddock-rss` must not show it).
- Kernel and host tests never run a plugin; `paddock check` is the only thing that does.
  A plugin's own tests run it against a fake of the tool it wraps (a shell script on disk).
- No environment variables for configuration. Only `PADDOCK_DIR` and XDG say where the
  host is. Keys, endpoints, accounts live in `config.toml`; any secret is `NAME_cmd`, a
  command whose stdout is the value. The host runs `NAME_cmd` for sources and hands the
  plugin `NAME`; a plugin never runs a secret command.
- Anything that knows a format of the world (mail, a feed, a chat) is a plugin, not an
  adapter. Adapters are the ports' implementations and know no format.
- Host-only concerns that live as data in `Config` are marked so (`remote`, `agent`,
  `notify_cmd`); the kernel reads none of them.

## style the owner wants

- Simplicity and clarity over cleverness. Fewer files, short functions, names that say
  what a thing is. No generic names (no "engine", "manager", "service").
- Functional shape: verbs take inputs and return what happened (`Admitted`, `Report`,
  `Told`, `Why`, `Answer`), warnings included. Nothing kept on the side.
- Comments say what a thing is now. Never a comment about what used to be here.
- Doc comments on every public item; module docs at the top of each file that say the
  module's one job. Config examples in the module doc of the adapter or plugin they configure.
- Tests: unit tests beside the code, integration tests in `crates/paddock/tests/` with
  fixtures that need nothing but `sh`. Test names are sentences.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets` clean before a commit.
- Commit and push to `main` directly (the owner said so: nobody else uses this yet). Commit
  messages: a sentence on the first line, then a list of what changed and why.
- No support for old stores or configs. There are no migrations; a schema change is a
  fresh store.

## commands

```
cargo build --workspace && cargo test --workspace     # 124 tests at handover
cargo build --release                                  # paddock 6.3 MB, plugins 0.5-2.7 MB
./target/release/paddock check ./target/release/paddock-maildir --set path=/some/Maildir
PADDOCK_DIR=/tmp/h ./target/release/paddock init && $EDITOR /tmp/h/config.toml && paddock pull
paddock context                                        # the briefing an agent gets
paddock setup "add my fastmail"                        # hand the host to the agent in [agent]
```

## where the work stands

Done and pushed on `main`:

- Kernel: items with parts, actors (`from`/`to`), cites (reply, forward, quote, mention,
  attach) resolved early or late, threads from keys or from cites, labels with provenance
  and hand denial, read as a label, inboxes as nested questions with `without`, `from`,
  `to`, time terms, text (FTS5) and meaning (vectors), effects on enter once per entry
  (`label:`, `read`, `send:`, `notify`), personas as top-level inboxes with sources.
- Host: sqlite (SQLCipher, key or key_cmd), fs and exec sources, plugins by kind on PATH,
  classifiers regex/script(CEL)/exec/http/llm, model exec/ollama/openai, embedder
  exec/http/ollama/openai/local(Model2Vec, feature `local`), mirror s3/exec, agent setup,
  notify_cmd, `--remote` over ssh, `--json` on every command.
- Plugins: rss, maildir, imap (+SMTP), wacli (WhatsApp), gog (Gmail), hey (HEY).
- Façade: `paddock-app` with `Session`, `Listing`, `InboxCount`, `Event`, `Watcher`.

Next, in the order agreed with the owner:

1. **Native apps.** Linux (Omarchy: Hyprland/Wayland) in Rust against `paddock-app`
   directly, GTK4/libadwaita or a Rust toolkit. macOS in SwiftUI over a Swift package
   generated by UniFFI from `paddock-app`. The kernel's `Item` cannot carry a binding
   derive (purity), so `paddock-app` needs a small DTO layer (`ItemView` and friends)
   when the Swift app starts; do it then, not before. Apps bundle the `paddock-*`
   binaries they ship and put that directory on the PATH they hand the host.
   Settings in the app are a picker for `[agent]` and a text field that calls
   `Session::setup`; no settings screens.
2. **Plugins to try against real accounts.** wacli, gog, hey were written from their Go
   sources and tested against fakes only. `paddock check paddock-gog --set account=...`
   is the first real test. Known gaps: gog's search output has no `to` and no
   In-Reply-To (a `gog gmail get` per message would); `hey compose` returns no id, so a
   fresh compose is named `sent-<time>`; wacli media is fetched only with `media = true`.
3. **Send-only plugins** (a transactional mail API, a webhook): `pull` prints `[]`,
   `send` posts the draft. The shape is documented in README "plugins"; none exist yet.
4. **`send` from the app for a persona**: `Session::source_in(inbox)` gives the source;
   the app fills `Draft.source_id` itself.

## things that bit us, so you do not repeat them

- Tests that write a script and then exec it can hit "text file busy" when another test
  thread forks at the same moment. The plugin test helpers retry the exec; keep that.
- `mail-parser` parses `List-Id` as an address (`as_address()`), not text.
- `wacli --json messages list` keys are Go field names (`MsgID`, `ChatJID`, `FromMe`,
  `Text`, `LocalPath`, `quoted_msg_id`). `gog` search gives `messages[].{id, threadId,
  from, subject, labels, body, internalDateIso, attachments}`. `hey` wraps everything in
  `{ok, data}`; postings carry `topic_id`, entries carry `id` and `body` (Markdown).
- SQLite's `PRAGMA data_version` moves only for other connections' commits; a session
  records its own writes itself (`Session::emit`) so a later `poll` is not fooled.
- `VACUUM INTO` keeps SQLCipher encryption; that is what the mirror pushes.
- The S3 signer is hand-rolled SigV4, unit-tested against AWS's worked example
  (`crates/paddock/src/adapters/mirror.rs`). Path-style URLs, `UNSIGNED-PAYLOAD`.
- An effect's output (`send:`) is admitted with `effects = false`, else it chases itself.
- Do not add a settings UI, a TUI, a web UI, or a daemon. The owner removed all of those.

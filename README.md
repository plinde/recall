# recall&nbsp;&nbsp;&nbsp;[![Mentioned in Awesome Claude Code](https://awesome.re/mentioned-badge.svg)](https://github.com/hesreallyhim/awesome-claude-code)

Search and resume your Claude Code conversations. Also supports Codex, OpenCode and Factory (Droid).

**Tip**: Don't like reading? Tell your agent to use `recall search --help` and it'll search for you.

![screenshot](screenshot-dark.png)

## Install

This personal fork is source-only: build it locally, then install only if you choose to:

```bash
git clone https://github.com/plinde/recall.git
cd recall
make build
make install
```

`make install` is intentionally separate; it copies the locally built binary to `~/.local/bin`.

## Use

Run:
```bash
recall
```

**That's it.** Recall starts by searching every indexed conversation. Start typing to search.
Enter to jump back in.

Press `Ctrl+G` to toggle between global and launch-directory (`cwd`) scope. `/` remains a
compatibility alias for the same toggle.

Press `Ctrl+R` to refresh the index and current results immediately. Recall also refreshes in
the background every five minutes while the TUI is open; both refresh modes preserve your query,
scope, and the session you are browsing when it still exists.

To force global scope for one launch, use either:
```bash
recall --global
# alias: recall --everywhere
```

| Key | Action |
|-----|--------|
| `↑↓` | Navigate sessions |
| `Pg↑/↓` | Scroll messages |
| `Ctrl+E` | Expand message |
| `Enter` | Resume conversation |
| `Tab` | Copy session ID |
| `Ctrl+G` | Toggle global/CWD scope (`/` is an alias) |
| `Ctrl+R` | Refresh indexed results |
| `Esc` | Quit |

## Ask it to Search for You
Simply tell your agent:
```
use `recall search --help`
```

Example:
```
pls find me the last conversation where we deployed to staging, use `recall search --help`
```

## MCP
No MCP required. The `recall search` CLI fulfills the same purpose. See [Ask it to Search for You](#ask-it-to-search-for-you).

## Customize

recall's resume commands can be configured with environment variables. Every custom
`RECALL_*_CMD` value must contain `{id}`, which recall replaces with the session ID.

For example, to resume conversations in YOLO mode, add this to your `.bashrc` or `.zshrc`:
```bash
export RECALL_CLAUDE_CMD="claude --dangerously-skip-permissions --resume {id}"
export RECALL_CODEX_CMD="codex --dangerously-bypass-approvals-and-sandbox resume {id}"
export RECALL_OPENCODE_CMD="opencode --session {id}"
```

Recall defaults to global scope. Set `RECALL_DEFAULT_SCOPE=folder` to start scoped to
the launch directory instead:
```bash
export RECALL_DEFAULT_SCOPE=folder
```

Valid values are `everything` and `folder`; an invalid value stops startup with an
error. `recall --global` (or `recall --everywhere`) overrides this setting for one launch.

---

![light mode](screenshot-light.png)

---

Made with ❤️ by [zippoxer](https://github.com/zippoxer) and Claude.

# SketchyUsage

Shows the remaining Claude and Codex subscription quota in [SketchyBar](https://felixkratz.github.io/SketchyBar). Click the bar segment to open a panel with the reset countdowns, the burn-rate projection, and the Codex reset credits.

```sh
sketchyusage serve      # fetch the usage, set the bar labels, own the panel
sketchyusage toggle     # open or close the panel
sketchyusage push       # set the bar labels again from the saved state
sketchyusage --version
```

`serve` sets the labels of two SketchyBar items: `sketchyusage.claude` and `sketchyusage.codex`. Add these items to your bar, and set their `click_script` to `sketchyusage toggle`.

## Requests

`serve` reads the usage at most once every 15 minutes for each provider. A panel open reads it again only when the last read was successful and is at least 5 minutes old. After a rate-limit reply, `serve` waits at least 1 hour. After a server error, it waits 1 hour, then up to 6 hours.

`serve` only reads. It does not refresh the Claude token and does not use the Codex reset credits.

- Claude: the OAuth usage endpoint, with the token from the Claude CLI (`~/.claude/.credentials.json` or the Keychain).
- Codex: `codex app-server`, with the login from the Codex CLI.

## Install

SketchyUsage needs macOS, and the Claude CLI and the Codex CLI with a login.

With Homebrew, which builds SketchyUsage from source:

```sh
brew install raisedadead/tap/sketchyusage
brew services start sketchyusage
```

From a clone, with Rust and [just](https://github.com/casey/just):

```sh
just install            # binary to ~/.local/bin
just check              # format, lint, and tests
```

## License

ISC

# SketchyUsage

Shows Claude and Codex subscription usage in SketchyBar. A background server fetches the usage, sets the bar labels, and opens a GPUI panel when the operator clicks the usage segment.

## Rules

- Validate with `just check`.
- The operator runs every GUI check: the panel and `serve` act on the live bar.
- Rate-limit safety outranks freshness. Every request goes through the fetch policy, at its current intervals.
- The server only reads. Token renewal stays with the Claude CLI, and Codex reset credits stay unredeemed.
- State, cache and logs hold usage data only.
- Every subprocess and HTTP call carries a hard timeout.

## Release

SketchyUsage uses semantic versions. Homebrew builds it from a git tag.

1. Set `version` in `Cargo.toml`, then run `cargo build` to update `Cargo.lock`.
1. Run `just check`.
1. Commit as `chore(release): <version>` on `main`.
1. Add an annotated tag: `git tag -a v<version> -m 'sketchyusage <version>'`.
1. The operator pushes `main` and the tag. Push the tag before the formula changes.
1. Update `Formula/sketchyusage.rb` in `raisedadead/homebrew-tap`. Its `README.md` gives the steps.

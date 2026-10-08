# SketchyUsage

Shows Claude and Codex subscription usage in SketchyBar. A background server fetches the usage, sets the bar labels, and opens a GPUI panel when the operator clicks the usage segment.

## Rules

- Validate with `just check`.
- The operator runs every GUI check: the panel and `serve` act on the live bar.
- Rate-limit safety outranks freshness. Every request goes through the fetch policy, at its current intervals.
- The server only reads. Token renewal stays with the Claude CLI, and Codex reset credits stay unredeemed.
- State, cache and logs hold usage data only.
- Every subprocess and HTTP call carries a hard timeout.

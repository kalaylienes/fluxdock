# Provider evaluation

FluxDock shows how much of a quota is left. That only means something for tools
that enforce one. A tool where you supply your own API key has a cost, not a
limit, and there is no percentage to draw.

A source qualifies when all three hold:

1. **There is a real quota.** A window that refills, not a running bill.
2. **The percentage comes from the vendor.** Reconstructing a limit from token
   counts and a price table means shipping a pricing table and keeping it
   correct forever, and being wrong quietly.
3. **It can be read without interfering.** A file the tool already writes, or an
   endpoint reachable with credentials the tool already stored. No injection, no
   extracting secrets from a binary, no consuming quota to measure quota.

## Supported

| Tool | Windows | Source |
| --- | --- | --- |
| Claude Code | 5 hour, weekly, optional per model weekly | `api/oauth/usage`, with local transcripts for interpolation |
| Codex CLI | 5 hour, weekly | `token_count` events in rollout transcripts, app server as fallback |

Both report server side percentages for two rolling windows, which is exactly
the shape the widget draws.

## Evaluated, not currently supported

**Google Antigravity.** Quota exists and the paid tiers do use a five hour
window, which fits well. It writes no usage file of its own. Reading the numbers
means either scraping a port and CSRF token out of the language server's command
line, or extracting an OAuth client secret from the shipped binary to call an
internal endpoint. The first is fragile, the second is not something this project
will do. Worth revisiting if a documented interface appears.

**GitHub Copilot CLI.** An internal endpoint does return quota snapshots, and the
token is already on disk from the GitHub CLI. Two problems. The quota is a
calendar month allowance with no rolling window, so it does not map onto the two
bars the widget is built around. And on a free account the bucket that actually
runs out is absent, so the adapter could only be verified against a response
shape that no paying user sees. Verifying against a stand in is how you ship a
provider that reports the wrong number.

**Gemini CLI.** Quota is counted in requests per day rather than a rolling
window, and it is held in memory rather than written anywhere. Reading it needs a
credential from Credential Manager, a project lookup, and an internal quota call.
The request based limit also cannot be calibrated against the token based local
logs.

**Cursor and Windsurf.** No published usage API, and the quota is a monthly
spending pool rather than a refilling window. Both are editors rather than
terminal tools, which puts them outside what this widget is for.

**aider, Goose, Crush, Qwen Code, Roo Code, OpenHands.** Bring your own key.
There is no quota to show.

**opencode.** The most widely used of the remaining options, and it does have its
own plan. There is no limit endpoint yet, and the cost field in its message files
is not populated, so a percentage would have to be reconstructed from a bundled
price table. That is a maintenance commitment rather than an adapter.

## Adding one

A provider implements `UsageProvider` in `src-tauri/src/providers/`:

```rust
fn id(&self) -> &'static str;
fn watch_paths(&self) -> Vec<PathBuf>;
async fn poll(&mut self, force: bool) -> ProviderSnapshot;
```

`poll` never returns an error. Failures travel inside `ProviderSnapshot::status`
so the widget can draw a specific state instead of going blank. `force` marks a
user requested refresh; anything that costs a network call or spawns a process
should respect it and otherwise keep to its own schedule, because the file
watcher fires often.

Beyond the trait, a new source currently needs a colour pair in `styles.css`, a
field in `ProviderSettings`, a toggle in the tray menu, and an entry in the
frontend's provider union. Collapsing those into a registry is the natural next
step and is worth doing before a third source lands.

Two rules for any adapter:

- Never recompute a percentage the vendor already reports.
- Label anything derived locally as an estimate, and keep it visually distinct
  from a server number.

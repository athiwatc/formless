# Meta Muse Code in topic workflows

The `muse` harness uses Meta's installed [Muse Code](https://dev.meta.ai/docs/muse-code) CLI through [`muse serve`](https://meta-models.github.io/muse-code-sdk/), the official interactive Muse Session Protocol. Install Muse Code and sign in as usual. Formless does not edit Muse settings, save workspace trust, or pass the prompt or bridge capability on the command line.

**One-run workspace trust.** Muse grants trust to the exact workspace, not to linked worktrees or subdirectories. Formless makes a fresh directory for each attempt, so it asks you whether to trust that directory before it starts Muse. Accept only if you trust the project's Muse configuration: Muse may load its rules, skills, and MCP servers. Denying or dismissing the request skips the run. Allowing adds Muse's process-only trust flag and does not save trust.

**Output and decisions.** Muse streams assistant text to Formless and sends pending questions and approvals there. The automated approval judge is off. Formless offers only eligible one-time grants and refuses persistent rules and out-of-scope access. A read-only step disables Muse's native shell and write tools. A write step uses sandboxed tools with restricted network access. Session logging, hooks, and Contributor mode are off.

**Required bridge.** Formless sends a uniquely named MCP server on private stdin after confirming a memory-only session. Its capability requirements are not inherited by the Muse CLI or the shell. Muse refuses to contact the model unless the required bridge tools are available.

**Readiness.** Muse Code 1.4.13 and later can confirm a Meta account via [`muse usage --json`](https://dev.meta.ai/docs/muse-code/subscriptions), without starting a model turn. Formless shows authentication as unknown if Muse cannot confirm an allowance, including for API keys and local providers. The probe does not sign in, update Muse, or read credential stores directly.

### Verification

Fixture tests exercise requests and responses without running Muse. Run the separate live smoke test with Muse Code and Python 3 installed:

```sh
cargo test -p formless-core --lib --no-default-features live_muse_msp_bridge -- --ignored
```

It checks bridge registration and fail-closed startup using the local Echo provider, without signing in or invoking a Meta model.

References: [Permissions and safety](https://dev.meta.ai/docs/muse-code/permissions), [MCP and toolsets](https://dev.meta.ai/docs/muse-code/extending), and [Muse Session Protocol](https://meta-models.github.io/muse-code-sdk/).

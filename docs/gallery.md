# What can Formless do?

Formless starts with no Tools. You ask for something; when the agent has no
Tool for it, it writes one (a small JavaScript program with only the access
it declares, see [Capabilities](capabilities.md)), uses it, and keeps it. The
next similar request reuses that Tool, and the optimiser improves it over
time. These examples show the kind of Tools that come out, and the commands
around them.

Type the request in the TUI (`formless`), the web UI (`formless web`), or
headless with `formless run "…"`.

## Research and the web

1. **Summarise a page.** "Summarise https://example.com/post in five bullet
   points." → a `fetch_page` Tool (`net:$input.url`, scoped to the URL it is
   given) that returns the readable text.
2. **Compare prices across a few shops.** "Find the price of the Kindle
   Paperwhite on these three sites and make a table." → reuses `fetch_page`
   and adds an `extract_price` Tool; the table is the answer.
3. **Track a changelog.** "Every Monday at 9, tell me what's new in the
   Rust release notes." → a scheduled job (`/jobs`, or the complete command
   below) that runs the same request weekly. For notification delivery:

   ```sh
   formless jobs add "Tell me what's new in the Rust release notes" --every "monday at 9am" --notify
   ```

   For file delivery, replace `--notify` with
   `--to-file "./rust-release-notes.txt"`. Delivery requires the daemon and
   computer to be available when the job runs.

## Your repositories

4. **Repo chores.** "List every TODO in this repo, grouped by file, oldest
   first by git blame." → `git_blame_lines` (`process:git`) and
   `find_todos` (`fs:read:$input.root`).
5. **Release notes.** "Write release notes from the commits since the last
   tag." → `git_log_since_tag`; reused every release.
6. **Review a diff from the shell.** `git diff | formless run "review this
   change for bugs"`: piped input is attached as `stdin.txt`.
7. **Fix and verify.** "Rename `parse_cfg` to `parse_config` everywhere and
   run the tests." → `replace_in_files` (`fs:write` on the repo) and
   `run_tests` (`process:cargo`). `/files` shows what changed; `/review`
   opens the last Run's text hunks (`/review RUN_ID` selects an older Run).
   Use `n`/`p` to navigate, `a` to keep a hunk, `r` to reject it and `u`
   to undo the latest review change. Changed words are underlined; `f`
   lists every file, including binary and generated files excluded from
   hunk editing. `c` sends a comment with server-derived path and line
   context to the active Run, or saves it for the next explicit Run.
   `/rewind` remains available to put whole files back.

   In a Tool's code editor, Ctrl+D still runs syntax and smoke checks;
   `h` from that review opens draft hunks. Reject and undo change only the
   draft, and Ctrl+S still uses the normal checked save. Esc closes review
   without discarding the draft. Closing a pending Run-file action does
   not cancel it: reopen `/review` to inspect its recorded result and undo.
   Reconnect never repeats an action automatically. Historical deleted-file
   snapshots without their original mode, and Windows files whose full security
   metadata is unavailable, remain read-only. Securely readable hardlinks and
   aliased paths retain byte-only whole-file history, but remain read-only in
   hunk review. Tool draft review is unaffected.

## Files and conversions

8. **CSV to charts.** "Read sales.csv and give me totals per region, then a
   bar chart as SVG." → `csv_stats` and `svg_bar_chart`, both reusable.
9. **Rename photos by date.** "Rename the photos in ~/Pictures/trip by the
   date they were taken." → `read_exif` and a rename Tool limited to that
   folder. Try `/step` first to approve each rename call.
10. **Convert documents.** "Turn every Markdown file in docs/ into one HTML
    page with a table of contents." → `markdown_to_html` (a shared module
    other Tools reuse).

## Using MCP servers

11. **GitHub issues.** `formless mcp add github` (from the
    [catalog](../SPEC.md#79-external-mcp-servers)), `formless mcp login
    github`, then "Which of my open issues mention the parser?" The agent
    calls the server's tools directly, and wraps repeated combinations in a
    Tool of its own.
12. **A browser.** `formless mcp add playwright`, then "Open the docs site,
    search for 'proxy' and tell me what it says." Already set up in Claude
    Desktop or Cursor? `formless mcp import` brings those servers over.

## Everyday helpers

13. **Remember a preference.** "Always answer in Thai" → saved as a memory
    (`/memory` lists them, `/forget` removes one).
    This changes the requested answer language. To choose English/Thai interface
    text instead, use Settings → Interface → language or
    `formless config set tui.language th`; see [interface language](../SPEC.md#108-language).
14. **Compare models.** `/compare-models gpt-5 claude-sonnet "explain this regex"`
    runs the same prompt on both and shows the answers side by side with
    time and cost.
    To compare existing branches without a model call, use `/compare SESSION_A
    SESSION_B` with full Session IDs or unambiguous prefixes. In the TUI Sessions
    view, `T` toggles the lineage tree; the web Sessions list has a Tree button.
    Fork sequence labels identify the copied boundary. Missing parents remain
    labelled without hiding their children. The stored comparison shows whole
    messages and recorded branch usage; missing usage and unpriced or unallocated
    work are shown explicitly rather than reported as zero cost.
    To bring a branch's conclusions back, open the target Session and use
    `/summarise-branch SOURCE_SESSION`. This explicitly generates a candidate
    using the target's provider. Review the exact note, source, target, served
    model, reported usage and any omitted source content, then choose Insert
    one note (`i` in the TUI) or Close review. Closing a review does not stop
    generation already dispatched: it may finish and record usage, but cannot
    insert a note without confirmation. Insertion adds one attributed note;
    it does not copy the transcript or start a Run. Changes to either Session
    require a fresh candidate. A reconnect closes the review without retrying it.
15. **Plan a bigger task.** "Migrate this project from Jest to Vitest." The
    agent keeps a checklist pinned above the input (`update_plan`) and a
    "What I did" recap when it finishes.

## Seeing what it built

- `Tab` to the **Tools** view: every Tool, its code, versions, success rate
  and cost; `/tool NAME` opens one.
- `/metrics` and `formless runs` show how Runs went; `/routing` explains
  model choices.
- `formless export-all` carries your Tools, Skills and memories to another
  machine.

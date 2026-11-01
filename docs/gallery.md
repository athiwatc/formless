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
   Rust release notes." → a scheduled job (`/jobs`, or `formless jobs add
   --every "monday at 9am"`) that runs the same request weekly and delivers
   the answer as a notification (`--notify`) or into a file (`--to-file`).

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
   `run_tests` (`process:cargo`). `/files` shows what changed; `/rewind`
   puts it back.

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
    one note (`i` in the TUI) or Cancel. Insertion adds one attributed note;
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

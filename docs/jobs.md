# Schedule a task

Want a recurring briefing? Ask Formless: “Every weekday at 9am, summarize
this project's open work.” A Job runs a prompt later using your configured
agent and model. Each firing can use Tools and incur provider costs like an
ordinary Run.

This guide covers scheduling and checking results. Exact timing, approval
and overlap rules are in [SPEC §9.7](../SPEC.md#97-scheduled-jobs).

## Pick a schedule

For a one-shot task:

```sh
formless jobs add "Check the build and summarize failures" --at "in 30m"
```

One-shot times accept relative minutes, hours or days, a local
`YYYY-MM-DD HH:MM` time, or RFC 3339. The time must be in the future.
Relative offsets are limited to 366 days.

Preview a recurring schedule before adding a Job. Use supported plain English
or five-field cron:

```sh
formless jobs preview "every weekday at 9am"
formless jobs preview "0 9 * * 1-5" --json
```

The daemon interprets the schedule and projects up to three times from its
current local time. The preview leaves Jobs unchanged. It can return fewer
times or none, and does not guarantee that `jobs add` will accept the schedule
or that a Job will fire. Use `jobs add --at` for one-shot times.

To create a recurring Job:

```sh
formless jobs add "Summarize this project's open work" --every "every weekday at 9am"
formless jobs add "Summarize this project's open work" --cron "0 9 * * 1-5"
```

These are alternatives: create the one you want. Recurring schedules use
the daemon host's local time. Check the next firings printed at creation
before relying on the schedule.

CLI-created Jobs are approved by your explicit command. When the agent
proposes a recurring Job, Formless asks for approval; if nobody is connected,
it waits for you to enable it in Jobs. Review its prompt, schedule and target:

```sh
formless jobs list
formless jobs enable JOB_ID
```

Replace `JOB_ID` with the ID from the list. Enabling also approves a pending
Job. A disabled recurring Job resumes at its next future schedule slot.

## Choose where the answer goes

By default each firing starts a new Session. Use `--session SESSION_ID` to
pin an existing Session, taking its full ID from `formless sessions list`.
If that Session is busy, its prompt is queued. A deleted target pauses the
Job and is reported in history.

To append a one-shot result to a file, for example:

```sh
formless jobs add "Summarize project work" --at "in 1h" --to-file /absolute/path/to/briefing.md
```

Delivery options can be added when creating a Job:

- `--notify` requests a notification through a supported desktop service.
- `--to-file PATH` appends the answer to a file the daemon can write.
- `--webhook URL` posts the result as JSON to your chosen endpoint.

Delivery destinations receive the answer. Check that the path or endpoint
belongs to the host or service you intend. Inspect history if delivery fails.

## Keep the daemon available

Jobs need a running daemon and an awake host. By default the daemon can
idle-close while a future Job waits. If you want enabled Jobs to keep it up:

```sh
formless config set daemon.keep_alive_for_jobs true
formless daemon status
```

This setting handles idle shutdown; arrange normal service startup separately
if you need it after reboot or logout. A stopped or sleeping host can make
Jobs late. On restart, missed recurring firings are coalesced into one late
firing instead of replaying every missed slot. See
[troubleshooting](troubleshooting.md) for daemon and service diagnostics.

## Inspect, pause or remove

```sh
formless jobs history JOB_ID
formless jobs disable JOB_ID
formless jobs run JOB_ID
formless jobs rm JOB_ID
```

History shows recent firing outcomes and their Sessions. `disable` pauses future
firings. `run` explicitly fires once now without enabling a paused schedule
or changing it. `rm` removes the schedule. The TUI's `/jobs` and the web
Settings Jobs section also let you inspect, approve and manage Jobs.

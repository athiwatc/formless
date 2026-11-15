# Install and use a Skill

Want Formless to follow a repeatable method? Ask: “Use my report-writing
Skill to turn these notes into a draft.” A Skill is a folder containing
`SKILL.md` instructions and, optionally, supporting files. Enabled Skills
are available for the agent to load when relevant.

This guide covers installation, review and reuse. The full format and
loading rules are in [SPEC §7.10](../SPEC.md#710-skills).

## Install a copy for review

Start with a folder you have inspected:

```sh
formless skills add /absolute/path/to/report-writing
formless skills list
```

You can also give a Git URL or a `.zip` archive. For a repository containing
several Skills, `--subdir` chooses the folder with `SKILL.md`; `--ref` selects
a Git branch or tag. Installation may fetch remote content. New installs
start disabled by default, so use the installed name from `skills list` in
the following commands.

## Read before enabling

```sh
formless skills show report-writing
formless skills reviews report-writing
```

Read the instructions and supporting files, including any commands, sources
or data destinations they ask the agent to use. Description reviews show
retained warnings and before/after changes; an absence of warnings is not
a guarantee about the instructions.

```sh
formless skills enable report-writing
```

Enable makes the Skill available to the agent. It can load `SKILL.md` and
read supporting files for the task. A Skill supplies instructions; Tool
calls still follow their normal [Capability and approval rules](capabilities.md).
Ask for the Skill by name when you want its method used.

## Disable or remove

```sh
formless skills disable report-writing
formless skills remove report-writing
```

Disable keeps the installed files and prevents new loads. Remove deletes
the installed Skill. Start a new Session when you want to work without
instructions already loaded into an earlier conversation.

## Move a Skill to another profile or machine

```sh
formless skills export report-writing -o report-writing.skill.json
formless skills import report-writing.skill.json
```

Export includes the Skill's files, without usage metrics. Review those files
for private content before sharing the bundle. Imported Skills start disabled
for review: run `skills show`, inspect any reviews, then enable the installed
name when ready. `skills export --all` exports every installed Skill.

If installation or loading fails, check that the selected folder contains
`SKILL.md`, that the Skill is enabled, and that its name matches `skills list`.
See [troubleshooting](troubleshooting.md) for general diagnostics.

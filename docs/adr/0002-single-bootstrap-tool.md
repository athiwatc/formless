# Boot with a single Bootstrap Tool (`create_tool`) and no starter Tools

A fresh Formless install exposes exactly one callable function, `create_tool`, and every other Tool is created through it, persisted, versioned, and reused across Sessions. We rejected shipping a starter toolset (files, shell, web) because the product's premise is a harness that grows the capabilities each use case needs rather than specialising — prebuilt Tools would fossilise one workflow (coding) and hide the creation path that self-optimisation depends on. `create_tool` itself is immutable and permanently excluded from Self-Optimisation, retirement, and Capability changes.

**Status:** accepted

**Consequences:** First-run quality rests entirely on the frozen bootstrap system prompt and the creation-time validation/smoke-test gate; if the agent cannot build a needed Tool, the harness has no fallback in v1.

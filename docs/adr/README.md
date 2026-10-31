# Architecture decision records

These records describe the decisions when accepted. Later behavior is specified in [SPEC.md](../../SPEC.md); historical wording in a record is not a replacement for that contract.

| Record | Decision |
| --- | --- |
| [0001](0001-daemon-core-architecture.md) | Keep state and behavior in one daemon behind thin clients. |
| [0002](0002-single-bootstrap-tool.md) | Grow capabilities through a fixed bootstrap Tool rather than a starter toolset. |
| [0003](0003-quickjs-tool-runtime.md) | Execute JavaScript in embedded QuickJS with Capability-gated host APIs. |
| [0004](0004-success-rate-gated-self-optimisation.md) | Gate Tool promotion on measured success and sandboxed replay. |
| [0005](0005-run-script-orchestration.md) | Allow orchestration scripts to compose Tools without their own Capabilities. |

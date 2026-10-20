# SPROCKET — Serial Protocol Rocket

SPROCKET is a cross-platform serial file-transfer, deployment, and disk-imaging project for vintage computers. A modern host performs complex operations; small platform-specific clients provide access to vintage hardware.

**Status:** Initial design draft (2026-10-20). This document set is intended for review, not an implemented protocol specification.

## Design documents

- [Architecture](docs/architecture.md): scope, components, platform independence, milestones.
- [Protocol](docs/protocol.md): wire-level requirements and proposed packet design.
- [Bootstrap and transfers](docs/bootstrap.md): standard protocols, first installation, upgrades, recovery.
- [Disk imaging](docs/imaging.md): addressing, integrity, error reporting, safety.
- [Licensing](docs/licensing.md): MIT project policy and third-party obligations.
- [Roadmap](docs/roadmap.md): staged implementation and acceptance criteria.

## Intended repository layout

```text
sprocket/
├── Cargo.toml                 # Existing Rust template / workspace as appropriate
├── host/                      # Rust host crate
├── client/
│   └── dos/                   # Open Watcom, 8086-compatible DOS client
├── docs/
└── tests/                     # Integration fixtures, as needed
```

The Rust project template is maintained separately; these documents do not prescribe changes to its existing files or license headers.

## Design status vocabulary

- **Requirement**: an agreed project goal.
- **Proposal**: an implementation choice requiring review.
- **Open question**: intentionally undecided.

See the documents for details. No protocol version has been ratified yet.

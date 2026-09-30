# Documentation

Project docs are organized by purpose.

## Structure

| Folder                           | Contents                                                         | Language |
| -------------------------------- | ---------------------------------------------------------------- | -------- |
| [`architecture/`](architecture/) | High-level system shape: components, data flow, threading model  | English  |
| [`decisions/`](decisions/)       | Architecture Decision Records (ADRs)                             | English  |
| [`feature/`](feature/)           | Design docs for non-trivial features being built                 | English  |
| [`explorations/`](explorations/) | Free-form research notes, spikes, ideas under consideration      | Korean   |
| [`test/`](test/)                 | What the test suites cover, and how to run the load baseline     | English  |

## Conventions

- ADRs are snapshots in time. When a decision is overturned, add a new ADR that supersedes the old one instead of editing it.
- `architecture/` reflects the **current** system. Update in sync with code changes.
- `explorations/` is intentionally rough, speed over polish.

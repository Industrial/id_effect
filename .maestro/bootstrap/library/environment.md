# Environment

Use this document for required tools, environment variables, and local setup notes.

## Required Tools

- Document the tools required for this repository

## Runtime Layout

- `.maestro/bootstrap/` is the committed bootstrap layer
- `.maestro/skills/` is the local runtime skill layer
- `.maestro/missions/` holds tracked `<slug>.md` design notes alongside ignored runtime state (`missions.jsonl`, per-mission dirs)
- `.maestro/sessions/` is runtime state (handoff packets live globally at `~/.maestro/handoff/`)

## Environment Variables

- Document required environment variables and safe defaults here

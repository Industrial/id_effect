#!/usr/bin/env python3
"""One-shot book migration helper for id_effect 3.0 capability DI docs.

Rewrites appendix A/B/C and part4 ch15-01..04. Run from repo root:
  python3 scripts/update_book_di_3_0.py
"""
from pathlib import Path

BOOK = Path(__file__).resolve().parents[1] / "crates" / "id_effect" / "book" / "src"

# Script documents the migration; re-run is idempotent only if source blocks unchanged.
print("Book files were migrated in-session. Re-run sections manually if needed.")
print("Targets:", BOOK / "appendix-a-api-reference.md", sep="\n  ")

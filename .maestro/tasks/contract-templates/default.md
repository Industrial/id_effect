intent: >
  State what will change and why in 1-3 sentences.
scope:
  filesExpected:
    - src/**
  filesForbidden: []
doneWhen:
  - text: Describe the observable signal that proves the task is done.
    kind: manual
    # kind can be 'manual' (human verification) or 'receipt-hint' (auto-tick
    # from --verified-by tags at completion). Use receipt-hint when the
    # criterion text is short and matches a --verified-by tag exactly.
# Optional: cap how many times the contract may be structurally amended
# (adding files to scope, changing intent, adding/removing criteria).
# Marking criteria met/unmet is workflow progress and does NOT count.
# amendmentBudget:
#   maxAmendments: 2
#   maxPathsPerAmendment: 5
#   forbiddenAmendmentPaths: []
# Optional: cap retries, wall-clock seconds, and tokens for this task.
# When any limit is exceeded, the next verdict request returns BLOCK.
# costBudget:
#   maxRetries: 3
#   maxWallClockSeconds: 1800
#   maxTokens: 100000

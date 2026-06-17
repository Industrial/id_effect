# no-yolo-data-probing

## Rule

Do not run ad-hoc shell pipelines against `.maestro/` JSONL stores from source code. Read through the typed store port instead.

## Rationale

JSONL files are an append-only journal. Shell reads skip schema validation and the v1/v2 split, which has caused real corruption incidents.

## Scan Command

! rg -n "(cat|head|tail|awk|sed)\s+[^\"']*\.maestro/(tasks|plans|evidence)" --glob 'src/**' --glob '!**/*.test.ts'

## Fix Recipe

1. Replace the shell read with the matching store call.
2. If the data isn't reachable via the port, add the missing method to the port.
3. Run `bun test` to confirm behavior is unchanged.

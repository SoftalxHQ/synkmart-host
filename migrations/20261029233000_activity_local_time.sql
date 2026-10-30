-- Legacy rows used datetime('now') (UTC, no zone). Shift once to host-local wall time
-- so activity "ago" / Today filters match the register clock. New writes set RFC3339 local.
UPDATE activity_logs
SET created_at = datetime(created_at, 'localtime')
WHERE instr(created_at, 'T') = 0
  AND instr(upper(created_at), 'Z') = 0;

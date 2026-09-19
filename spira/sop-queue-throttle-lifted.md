MATCH: ^Queue throttle lifted:.*CERTIFIED depth now (\d+).*Builder admission is no longer throttled
SYMPTOM: Watchtower files an informational incident when queue depth drops below the release threshold after a throttle period, indicating recovery to normal operation
CHECK: Confirm queue depth in payload is below the release threshold (typically 6) and that builder admission can proceed
FIX: None — this is a recovery notification, not a failure state. Throttle lifted is the desired outcome.
ESCALATE: None — normal operation
REF: wiki/notes/queue-management.md

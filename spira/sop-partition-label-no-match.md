MATCH: ^UNCLAIMABLE .* — spira with no matching partition
SYMPTOM: Sentinel detects a bead with no partition label match from any persona. The error message lists partition labels but may show them both in parentheses and in the "add one of" list.
CHECK: grep -c "UNCLAIMABLE" < <(spira/lib.sh detect_unclaimable_ready 2>/dev/null) | grep -qE '^[0-9]+$' && exit 0 || exit 1
FIX: This is a recurrence indicating the message formatting fix (db-5y2e) did not hold. Root cause: The partition_labels calculation in detect_unclaimable_ready extracts labels from all personas in the chamber. If the message still shows duplicates or malformed output, verify partition_labels calculation at lib.sh:2952 correctly excludes the scope_label and that the Python string formatting (lines 3025-3027) has no duplicate parameter substitution.
ESCALATE: If the fix does not hold after verifying the message format, escalate to determine whether partition label validation logic itself needs refactoring (e.g., the personas' FAYTH_LABELS configuration or chamber structure).
REF: db-5y2e (previous incomplete fix), spira/lib.sh:2918-3029 (detect_unclaimable_ready function)

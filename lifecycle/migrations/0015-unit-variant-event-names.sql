-- Unit variants were logged as "unknown": their evidence is the bare JSON string of the variant name.
UPDATE event SET event = JSON_UNQUOTE(evidence) WHERE event = 'unknown' AND JSON_TYPE(evidence) = 'STRING';

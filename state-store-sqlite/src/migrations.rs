//! The control store's schema migrations, in their own file so the drill's trigger watches the
//! schema and not every edit to `lib.rs`.

/// Each schema migration after the base [`SCHEMA`], in order; a store's `PRAGMA user_version`
/// is how many it has applied. Append only: a migration once shipped is never edited.
pub const MIGRATIONS: &[&[&str]] = &[
    // 1 (sp-zjkgeh): every live key's newest version, so a read at the head is a range, and
    // its `data.state`, indexed, so a state filter is a range.
    &[
        "CREATE TABLE latest (
            key BLOB PRIMARY KEY,
            main INTEGER NOT NULL,
            sub INTEGER NOT NULL,
            value BLOB NOT NULL,
            create_main INTEGER,
            create_sub INTEGER,
            version INTEGER NOT NULL,
            state TEXT GENERATED ALWAYS AS (CASE
                WHEN json_valid(CAST(value AS TEXT))
                 AND json_type(CAST(value AS TEXT), '$.data.state') = 'text'
                THEN json_extract(CAST(value AS TEXT), '$.data.state') END) VIRTUAL
        ) WITHOUT ROWID",
        "INSERT INTO latest (key, main, sub, value, create_main, create_sub, version)
         SELECT key, max(main), sub, value, create_main, create_sub, version FROM versions
         GROUP BY key HAVING value IS NOT NULL",
        "CREATE INDEX latest_by_state ON latest (state, key) WHERE state IS NOT NULL",
    ],
    // 2 (sp-8ugeek): each delivery's `entered` — `{STATE: at}`, the newest applied delivery
    // audit into each state — filled once from its history, so `list --delivery` reads when a
    // delivery entered its state from the delivery and never from its history. History is
    // read as the control plane's feed reads it: an imported entry carries its audit, any
    // other names the `events/<rev>` whose branch carried it. The value is rewritten in place
    // at its own revision, in `latest` and in the `versions` row it mirrors. A store without
    // `deliveries/` keys is untouched.
    &[
        "CREATE TEMP TABLE m2_audits (id TEXT NOT NULL, to_state TEXT, at INTEGER)",
        "INSERT INTO m2_audits
         SELECT id, json_extract(v, '$.data.audit.to_state'), json_extract(v, '$.data.audit.at')
         FROM (SELECT substr(CAST(key AS TEXT), 9, instr(substr(CAST(key AS TEXT), 9), '/') - 1) AS id,
                      CAST(value AS TEXT) AS v
               FROM latest WHERE key >= CAST('history/' AS BLOB) AND key < CAST('history0' AS BLOB))
         WHERE json_valid(v)
           AND json_extract(v, '$.data.seq') IS NOT NULL
           AND json_extract(v, '$.data.audit.machine') = 'delivery'
           AND json_extract(v, '$.data.audit.applied') = 1
           AND json_extract(v, '$.data.audit.lc_key') = id",
        "INSERT INTO m2_audits
         SELECT h.id, json_extract(a.value, '$.to_state'), json_extract(a.value, '$.at')
         FROM (SELECT substr(CAST(key AS TEXT), 9, instr(substr(CAST(key AS TEXT), 9), '/') - 1) AS id, main
               FROM latest WHERE key >= CAST('history/' AS BLOB) AND key < CAST('history0' AS BLOB)
                 AND json_valid(CAST(value AS TEXT))
                 AND json_extract(CAST(value AS TEXT), '$.data.seq') IS NULL) AS h
         JOIN latest AS e ON e.key = CAST('events/' || printf('%020d', h.main) AS BLOB)
         JOIN json_each(CAST(e.value AS TEXT),
              CASE WHEN json_extract(CAST(e.value AS TEXT), '$.data.refused')
                   THEN '$.data.evidence.else' ELSE '$.data.evidence.then' END) AS a
         WHERE json_extract(a.value, '$.machine') = 'delivery'
           AND json_extract(a.value, '$.lc_key') = h.id
           AND json_extract(a.value, '$.applied') = 1",
        "CREATE TEMP TABLE m2_entered (id TEXT PRIMARY KEY, obj TEXT NOT NULL)",
        "INSERT INTO m2_entered
         SELECT id, json_group_object(to_state, at)
         FROM (SELECT id, to_state, max(at) AS at FROM m2_audits WHERE to_state IS NOT NULL GROUP BY id, to_state)
         GROUP BY id",
        "UPDATE latest
         SET value = CAST(json_set(CAST(value AS TEXT), '$.data.entered',
             json(coalesce((SELECT obj FROM m2_entered WHERE id = substr(CAST(latest.key AS TEXT), 12)), '{}'))) AS BLOB)
         WHERE key >= CAST('deliveries/' AS BLOB) AND key < CAST('deliveries0' AS BLOB)
           AND instr(substr(CAST(key AS TEXT), 12), '/') = 0
           AND json_valid(CAST(value AS TEXT))
           AND json_type(CAST(value AS TEXT), '$.data') = 'object'
           AND json_type(CAST(value AS TEXT), '$.data.entered') IS NULL",
        "UPDATE versions SET value = l.value
         FROM latest AS l
         WHERE l.key = versions.key AND l.main = versions.main
           AND l.key >= CAST('deliveries/' AS BLOB) AND l.key < CAST('deliveries0' AS BLOB)
           AND versions.value IS NOT l.value",
        "DROP TABLE m2_audits",
        "DROP TABLE m2_entered",
    ],
    // 3: `(owner, key)` for every live key whose `metadata.ownerReferences` names the
    // owner, so the keys an owner leaves behind are a range. `events/`, `requests/` and
    // `history/` hold no owners and are left out.
    &[
        "CREATE TABLE owner_refs (
            owner BLOB NOT NULL,
            key BLOB NOT NULL,
            PRIMARY KEY (owner, key)
        ) WITHOUT ROWID",
        "INSERT OR IGNORE INTO owner_refs (owner, key)
         SELECT CAST(j.value AS BLOB), l.key
         FROM (SELECT key, CAST(value AS TEXT) AS v FROM latest
               WHERE NOT (key >= CAST('events/' AS BLOB) AND key < CAST('events0' AS BLOB))
                 AND NOT (key >= CAST('requests/' AS BLOB) AND key < CAST('requests0' AS BLOB))
                 AND NOT (key >= CAST('history/' AS BLOB) AND key < CAST('history0' AS BLOB))
                 AND json_valid(CAST(value AS TEXT))) AS l,
              json_each(l.v, '$.metadata.ownerReferences') AS j
         WHERE j.type = 'text'",
    ],
    // 4 (sp-3yru0z): a live key's absolute expiry, `metadata.expiresAt`, as an indexed column,
    // so the keys due now are a range. Each existing `telemetry/` and `intents/` key with a
    // positive `metadata.ttl_secs` is dated from its `events/` record's `at` when it has one,
    // and otherwise from open time, which is the life a restart gave it and never shorter.
    // Rewritten in place at its own revision, in `latest` and in the `versions` row it mirrors.
    &[
        "ALTER TABLE latest ADD COLUMN expires_at INTEGER GENERATED ALWAYS AS (CASE
            WHEN json_valid(CAST(value AS TEXT))
            THEN json_extract(CAST(value AS TEXT), '$.metadata.expiresAt') END) VIRTUAL",
        "CREATE INDEX latest_by_expiry ON latest (expires_at, key) WHERE expires_at IS NOT NULL",
        "UPDATE latest
         SET value = CAST(json_set(CAST(value AS TEXT), '$.metadata.expiresAt',
             coalesce(
                 (SELECT json_extract(CAST(e.value AS TEXT), '$.data.at') FROM latest AS e
                  WHERE e.key = CAST('events/' || printf('%020d', latest.main) AS BLOB)
                    AND json_valid(CAST(e.value AS TEXT))
                    AND json_type(CAST(e.value AS TEXT), '$.data.at') = 'integer'),
                 CAST(unixepoch('subsec') * 1000 AS INTEGER))
             + json_extract(CAST(value AS TEXT), '$.metadata.ttl_secs') * 1000) AS BLOB)
         WHERE (key >= CAST('telemetry/' AS BLOB) AND key < CAST('telemetry0' AS BLOB)
                OR key >= CAST('intents/' AS BLOB) AND key < CAST('intents0' AS BLOB))
           AND json_valid(CAST(value AS TEXT))
           AND json_type(CAST(value AS TEXT), '$.metadata.ttl_secs') = 'integer'
           AND json_extract(CAST(value AS TEXT), '$.metadata.ttl_secs') > 0",
        "UPDATE versions SET value = l.value
         FROM latest AS l
         WHERE l.key = versions.key AND l.main = versions.main
           AND (l.key >= CAST('telemetry/' AS BLOB) AND l.key < CAST('telemetry0' AS BLOB)
                OR l.key >= CAST('intents/' AS BLOB) AND l.key < CAST('intents0' AS BLOB))
           AND versions.value IS NOT l.value",
    ],
    // 5 (sp-m342rx): a live key's `data.opened_at` as an indexed column, so the newest rounds
    // are a range of the index read backwards.
    &[
        "ALTER TABLE latest ADD COLUMN opened_at INTEGER GENERATED ALWAYS AS (CASE
            WHEN json_valid(CAST(value AS TEXT))
             AND json_type(CAST(value AS TEXT), '$.data.opened_at') = 'integer'
            THEN json_extract(CAST(value AS TEXT), '$.data.opened_at') END) VIRTUAL",
        "CREATE INDEX latest_by_opened ON latest (opened_at, key) WHERE opened_at IS NOT NULL",
    ],
    // 6 (sp-aiti78): each round's `batch_events` — `{fetched: [{event, evidence, at}], last_at}`,
    // its applied batch audits of the pass and eject events in revision order and the newest
    // `at` of any — filled once from its history, so `list --batches` reads a round's passes,
    // ejections and last event from the round and never from its history. History is read as in
    // migration 2: an imported entry carries its audit (and comes first, at its seq), any other
    // names the `events/<rev>` whose branch carried it. The value is rewritten in place at its
    // own revision, in `latest` and in the `versions` row it mirrors. A store without `batches/`
    // keys is untouched.
    &[
        "CREATE TEMP TABLE m6_rounds (key BLOB PRIMARY KEY, id TEXT NOT NULL)",
        "INSERT INTO m6_rounds
         SELECT key, id
         FROM (SELECT key, value,
                      substr(substr(CAST(key AS TEXT), 9), instr(substr(CAST(key AS TEXT), 9), '/') + 1) AS id
               FROM latest WHERE key >= CAST('batches/' AS BLOB) AND key < CAST('batches0' AS BLOB))
         WHERE CASE WHEN json_valid(CAST(value AS TEXT))
                    THEN json_extract(CAST(value AS TEXT), '$.data.batch_id') = id END",
        "CREATE TEMP TABLE m6_audits (id TEXT NOT NULL, main INTEGER NOT NULL, sub INTEGER NOT NULL,
                                      event TEXT, evidence TEXT, at INTEGER)",
        "INSERT INTO m6_audits
         SELECT r.id, 0, json_extract(CAST(h.value AS TEXT), '$.data.seq'),
                json_extract(CAST(h.value AS TEXT), '$.data.audit.event'),
                CAST(h.value AS TEXT) -> '$.data.audit.evidence',
                json_extract(CAST(h.value AS TEXT), '$.data.audit.at')
         FROM m6_rounds AS r
         JOIN latest AS h ON h.key >= CAST('history/' || r.id || '/' AS BLOB)
                         AND h.key < CAST('history/' || r.id || '0' AS BLOB)
         WHERE CASE WHEN json_valid(CAST(h.value AS TEXT))
                    THEN json_extract(CAST(h.value AS TEXT), '$.data.seq') IS NOT NULL
                     AND json_extract(CAST(h.value AS TEXT), '$.data.audit.machine') = 'batch'
                     AND json_extract(CAST(h.value AS TEXT), '$.data.audit.applied') = 1
                     AND json_extract(CAST(h.value AS TEXT), '$.data.audit.lc_key') = r.id END",
        "CREATE TEMP TABLE m6_revs (id TEXT NOT NULL, main INTEGER NOT NULL, PRIMARY KEY (id, main))",
        "INSERT OR IGNORE INTO m6_revs
         SELECT r.id, h.main
         FROM m6_rounds AS r
         JOIN latest AS h ON h.key >= CAST('history/' || r.id || '/' AS BLOB)
                         AND h.key < CAST('history/' || r.id || '0' AS BLOB)
         WHERE CASE WHEN json_valid(CAST(h.value AS TEXT))
                    THEN json_extract(CAST(h.value AS TEXT), '$.data.seq') IS NULL END",
        "INSERT INTO m6_audits
         SELECT v.id, v.main, a.key, json_extract(a.value, '$.event'), a.value -> '$.evidence',
                json_extract(a.value, '$.at')
         FROM m6_revs AS v
         JOIN latest AS e ON e.key = CAST('events/' || printf('%020d', v.main) AS BLOB)
         JOIN json_each(CASE WHEN json_valid(CAST(e.value AS TEXT)) THEN CAST(e.value AS TEXT) ELSE '{}' END,
              CASE WHEN json_extract(CASE WHEN json_valid(CAST(e.value AS TEXT)) THEN CAST(e.value AS TEXT) ELSE '{}' END,
                                     '$.data.refused')
                   THEN '$.data.evidence.else' ELSE '$.data.evidence.then' END) AS a
         WHERE json_extract(a.value, '$.machine') = 'batch'
           AND json_extract(a.value, '$.lc_key') = v.id
           AND json_extract(a.value, '$.applied') = 1",
        "UPDATE latest
         SET value = CAST(json_set(CAST(value AS TEXT), '$.data.batch_events', json_object(
             'fetched', json(coalesce((
                 SELECT json_group_array(json_object('event', event, 'evidence', json(evidence), 'at', at))
                 FROM (SELECT event, evidence, at FROM m6_audits
                       WHERE id = (SELECT id FROM m6_rounds WHERE key = latest.key)
                         AND event IN ('PassStarted', 'SuitesStarted', 'PassGreen', 'PassRed',
                                       'PassIncomplete', 'PassPreempted', 'PassRebuilt', 'Eject')
                       ORDER BY main, sub)), '[]')),
             'last_at', (SELECT max(at) FROM m6_audits
                         WHERE id = (SELECT id FROM m6_rounds WHERE key = latest.key)))) AS BLOB)
         WHERE key IN (SELECT key FROM m6_rounds)
           AND CASE WHEN json_valid(CAST(value AS TEXT))
                    THEN json_type(CAST(value AS TEXT), '$.data') = 'object'
                     AND json_type(CAST(value AS TEXT), '$.data.batch_events') IS NULL END",
        "UPDATE versions SET value = l.value
         FROM latest AS l
         WHERE l.key = versions.key AND l.main = versions.main
           AND l.key IN (SELECT key FROM m6_rounds)
           AND versions.value IS NOT l.value",
        "DROP TABLE m6_audits",
        "DROP TABLE m6_revs",
        "DROP TABLE m6_rounds",
    ],
    // 7 (sp-5s1fvq): each lifecycle record under a manual hold keeps `manual_hold`, the newest
    // applied bead `Hold` of kind Manual (or its legacy name Operator) in its history, as
    // `{at, cause, detail}`, so the watchers that read when and why a hold was raised read it
    // from the record and never from history. History is read as migration 2 reads it. The
    // value is rewritten in place at its own revision, in `latest` and in the `versions` row it
    // mirrors. A record under no manual hold is untouched.
    &[
        "CREATE TEMP TABLE m7_holds (id TEXT NOT NULL, main INTEGER NOT NULL, hkey TEXT NOT NULL, at INTEGER, cause TEXT, detail TEXT)",
        "INSERT INTO m7_holds
         SELECT id, main, hkey, json_extract(a, '$.at'), json_extract(a, '$.evidence.Hold.cause'), json_extract(a, '$.evidence.Hold.detail')
         FROM (SELECT substr(CAST(key AS TEXT), 9, instr(substr(CAST(key AS TEXT), 9), '/') - 1) AS id,
                      main, CAST(key AS TEXT) AS hkey, json_extract(CAST(value AS TEXT), '$.data.audit') AS a
               FROM latest WHERE key >= CAST('history/' AS BLOB) AND key < CAST('history0' AS BLOB)
                 AND json_valid(CAST(value AS TEXT))
                 AND json_extract(CAST(value AS TEXT), '$.data.seq') IS NOT NULL)
         WHERE json_valid(a)
           AND json_extract(a, '$.machine') = 'bead' AND json_extract(a, '$.event') = 'Hold'
           AND json_extract(a, '$.applied') = 1 AND json_extract(a, '$.lc_key') = id
           AND json_extract(a, '$.evidence.Hold.kind') IN ('Manual', 'Operator')",
        "INSERT INTO m7_holds
         SELECT h.id, h.main, h.hkey, json_extract(a.value, '$.at'), json_extract(a.value, '$.evidence.Hold.cause'), json_extract(a.value, '$.evidence.Hold.detail')
         FROM (SELECT substr(CAST(key AS TEXT), 9, instr(substr(CAST(key AS TEXT), 9), '/') - 1) AS id, main, CAST(key AS TEXT) AS hkey
               FROM latest WHERE key >= CAST('history/' AS BLOB) AND key < CAST('history0' AS BLOB)
                 AND json_valid(CAST(value AS TEXT))
                 AND json_extract(CAST(value AS TEXT), '$.data.seq') IS NULL) AS h
         JOIN latest AS e ON e.key = CAST('events/' || printf('%020d', h.main) AS BLOB)
         JOIN json_each(CAST(e.value AS TEXT),
              CASE WHEN json_extract(CAST(e.value AS TEXT), '$.data.refused')
                   THEN '$.data.evidence.else' ELSE '$.data.evidence.then' END) AS a
         WHERE json_extract(a.value, '$.machine') = 'bead'
           AND json_extract(a.value, '$.event') = 'Hold'
           AND json_extract(a.value, '$.lc_key') = h.id
           AND json_extract(a.value, '$.applied') = 1
           AND json_extract(a.value, '$.evidence.Hold.kind') IN ('Manual', 'Operator')",
        "CREATE TEMP TABLE m7_newest (id TEXT PRIMARY KEY, obj TEXT NOT NULL)",
        "INSERT INTO m7_newest
         SELECT id, json_object('at', at, 'cause', cause, 'detail', detail)
         FROM (SELECT *, row_number() OVER (PARTITION BY id ORDER BY at DESC, main DESC, hkey DESC) AS n FROM m7_holds)
         WHERE n = 1 AND cause IS NOT NULL",
        "UPDATE latest
         SET value = CAST(json_set(CAST(value AS TEXT), '$.data.manual_hold',
             json((SELECT obj FROM m7_newest WHERE id = substr(CAST(latest.key AS TEXT), 11)))) AS BLOB)
         WHERE key >= CAST('lifecycle/' AS BLOB) AND key < CAST('lifecycle0' AS BLOB)
           AND instr(substr(CAST(key AS TEXT), 11), '/') = 0
           AND json_valid(CAST(value AS TEXT))
           AND json_type(CAST(value AS TEXT), '$.data.holds') = 'array'
           AND json_type(CAST(value AS TEXT), '$.data.manual_hold') IS NULL
           AND (EXISTS (SELECT 1 FROM json_each(CAST(latest.value AS TEXT), '$.data.holds') AS j WHERE j.value IN ('Manual', 'Operator')))
           AND EXISTS (SELECT 1 FROM m7_newest WHERE id = substr(CAST(latest.key AS TEXT), 11))",
        "UPDATE versions SET value = l.value
         FROM latest AS l
         WHERE l.key = versions.key AND l.main = versions.main
           AND l.key >= CAST('lifecycle/' AS BLOB) AND l.key < CAST('lifecycle0' AS BLOB)
           AND versions.value IS NOT l.value",
        "DROP TABLE m7_holds",
        "DROP TABLE m7_newest",
    ],
];

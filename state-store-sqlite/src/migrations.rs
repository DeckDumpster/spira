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
    // 8 (sp-hoypai): a fact's `data.kind` and `data.at` as indexed columns, so the facts of a
    // kind newer than a time are a range of the index and never a read of every fact.
    &[
        "ALTER TABLE latest ADD COLUMN fact_kind TEXT GENERATED ALWAYS AS (CASE
            WHEN key >= CAST('facts/' AS BLOB) AND key < CAST('facts0' AS BLOB)
             AND json_valid(CAST(value AS TEXT))
             AND json_type(CAST(value AS TEXT), '$.data.kind') = 'text'
             AND json_type(CAST(value AS TEXT), '$.data.at') = 'integer'
            THEN json_extract(CAST(value AS TEXT), '$.data.kind') END) VIRTUAL",
        "ALTER TABLE latest ADD COLUMN fact_at INTEGER GENERATED ALWAYS AS (CASE
            WHEN key >= CAST('facts/' AS BLOB) AND key < CAST('facts0' AS BLOB)
             AND json_valid(CAST(value AS TEXT))
             AND json_type(CAST(value AS TEXT), '$.data.at') = 'integer'
            THEN json_extract(CAST(value AS TEXT), '$.data.at') END) VIRTUAL",
        "CREATE INDEX latest_by_fact ON latest (fact_kind, fact_at, key) WHERE fact_kind IS NOT NULL",
    ],
];

#[cfg(test)]
mod cost {
    use super::MIGRATIONS;
    use crate::{exec, SCHEMA};
    use rusqlite::{params, Connection};
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    const ROUNDS: usize = 10;
    const GROWTH: usize = 4;
    const BOUND: u64 = 6;
    /// Shipped before this check and never editable; each entry must still be over the bound,
    /// so the list cannot outlive the debt and a new migration cannot be added to it unseen.
    const SHIPPED_SUPERLINEAR: &[usize] = &[6];

    fn obj(metadata: Value, data: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({ "metadata": metadata, "data": data })).unwrap()
    }

    fn audit(machine: &str, id: &str, event: &str, to: &str, at: i64) -> Value {
        json!({ "machine": machine, "lc_key": id, "event": event, "to_state": to, "at": at,
                "applied": true,
                "evidence": { "n": at, "Hold": { "kind": "Manual", "cause": "c", "detail": "d" } } })
    }

    /// A store of production shape: per round a batch, a delivery, 8 history entries each
    /// (half imported with `data.seq`, half naming an `events/` revision of 3 audits), and
    /// about one `telemetry/` and one `intents/` row, each with a ttl and an owner.
    fn shaped_rows(rounds: usize) -> Vec<(String, i64, Vec<u8>)> {
        let mut rows = Vec::new();
        let mut rev = 1i64;
        let mut next = || {
            rev += 1;
            rev
        };
        for i in 0..rounds {
            let id = format!("r-{i}");
            let owner = json!({ "ownerReferences": [format!("batches/spira/{id}")] });
            rows.push((
                format!("batches/spira/{id}"),
                next(),
                obj(
                    json!({}),
                    json!({ "batch_id": id, "state": "OPEN", "opened_at": i }),
                ),
            ));
            rows.push((
                format!("deliveries/d-{i}"),
                next(),
                obj(
                    owner.clone(),
                    json!({ "bead_id": format!("d-{i}"), "state": "Batched" }),
                ),
            ));
            rows.push((
                format!("lifecycle/d-{i}"),
                next(),
                obj(json!({}), json!({ "holds": ["Manual"] })),
            ));
            for id in [format!("r-{i}"), format!("d-{i}")] {
                for seq in 0..4i64 {
                    let machine = if seq == 0 && id.starts_with('d') {
                        "bead"
                    } else if id.starts_with('r') {
                        "batch"
                    } else {
                        "delivery"
                    };
                    let event = ["Hold", "PassRed", "Eject", "PassGreen"][seq as usize];
                    rows.push((
                    format!("history/{id}/0/import/{seq}"),
                    1,
                    obj(json!({}), json!({ "seq": seq + 1, "audit": audit(machine, &id, event, "BATCHED", 10 + seq) })),
                ));
                }
                for k in 0..4i64 {
                    let m = next();
                    rows.push((
                        format!("history/{id}/{m}/r{k}"),
                        m,
                        obj(json!({}), json!({})),
                    ));
                    let then: Vec<Value> = (0..3)
                        .map(|a| {
                            audit(
                                if a == 2 { "delivery" } else { "batch" },
                                &id,
                                ["PassStarted", "PassRed", "Eject"][a],
                                "QUEUED",
                                100 + a as i64,
                            )
                        })
                        .collect();
                    rows.push((
                    format!("events/{m:020}"),
                    m,
                    obj(json!({}), json!({ "refused": false, "at": 1000 + m, "evidence": { "then": then, "else": [] } })),
                ));
                }
            }
            for prefix in ["telemetry", "intents"] {
                rows.push((format!("{prefix}/{id}"), next(), obj(json!({ "ttl_secs": 60, "ownerReferences": [format!("batches/spira/{id}")] }), json!({ "state": "x" }))));
            }
        }
        rows
    }

    fn store_at(version: usize, rows: &[(String, i64, Vec<u8>)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        for sql in SCHEMA
            .iter()
            .chain(MIGRATIONS[..version].iter().flat_map(|m| m.iter()))
        {
            exec(&conn, sql, []).unwrap();
        }
        let tables: &[&str] = if version >= 1 {
            &["versions", "latest"]
        } else {
            &["versions"]
        };
        for table in tables {
            let sql = format!(
                "INSERT INTO {table} (key, main, sub, value, create_main, create_sub, version)
                 VALUES (?1, ?2, 0, ?3, ?2, 0, 1)"
            );
            for (k, main, v) in rows {
                exec(&conn, &sql, params![k.as_bytes(), main, v]).unwrap();
            }
        }
        conn
    }

    fn run(version: usize, rounds: usize) -> (u64, u128) {
        measure(version - 1, MIGRATIONS[version - 1], rounds)
    }

    fn measure(at: usize, migration: &[&str], rounds: usize) -> (u64, u128) {
        let conn = store_at(at, &shaped_rows(rounds));
        let before = conn.total_changes();
        let steps = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&steps);
        conn.progress_handler(
            1,
            Some(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                false
            }),
        );
        let start = Instant::now();
        for sql in migration {
            exec(&conn, sql, []).unwrap();
        }
        let writes = migration
            .iter()
            .any(|sql| sql.starts_with("INSERT") || sql.starts_with("UPDATE"));
        assert!(
            !writes || conn.total_changes() > before,
            "the generated store gave the migration no rows to rewrite"
        );
        (steps.load(Ordering::Relaxed), start.elapsed().as_millis())
    }

    #[test]
    fn no_migration_does_work_that_grows_faster_than_the_store() {
        let mut red = Vec::new();
        for k in 1..=MIGRATIONS.len() {
            let (s1, ms1) = run(k, ROUNDS);
            let (s4, ms4) = run(k, ROUNDS * GROWTH);
            eprintln!("migration {k}: {s1}/{s4} VM steps, {ms1}/{ms4} ms");
            let over = s4 > BOUND * s1;
            let ratio = s4 as f64 / s1.max(1) as f64;
            if over && !SHIPPED_SUPERLINEAR.contains(&k) {
                red.push(format!(
                    "migration {k}: {s1} -> {s4} VM steps over {GROWTH}x the rows (x{ratio:.1}, bound x{BOUND})"
                ));
            } else if !over && SHIPPED_SUPERLINEAR.contains(&k) {
                red.push(format!(
                    "migration {k}: within bound (x{ratio:.1}); drop it from SHIPPED_SUPERLINEAR"
                ));
            }
        }
        assert!(red.is_empty(), "{}", red.join("\n"));
    }

    #[test]
    fn a_correlated_subquery_over_an_unindexed_temp_table_is_over_the_bound() {
        let planted = [
            "CREATE TEMP TABLE planted (id TEXT NOT NULL)",
            "INSERT INTO planted SELECT CAST(key AS TEXT) FROM latest",
            "UPDATE latest SET value = value WHERE (SELECT count(*) FROM planted WHERE id = CAST(latest.key AS TEXT)) > 5",
        ];
        let (s1, _) = measure(MIGRATIONS.len(), &planted, ROUNDS);
        let (s4, _) = measure(MIGRATIONS.len(), &planted, ROUNDS * GROWTH);
        assert!(s4 > BOUND * s1, "{s1} -> {s4}");
    }
}

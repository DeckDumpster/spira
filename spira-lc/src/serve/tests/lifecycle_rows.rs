use super::*;

#[test_id(tafqpyb)]
#[test]
fn a_content_create_gets_its_lifecycle_row_in_the_same_request() {
    let w = World::new();
    let ans = served(&w, &Filing(&w, "sp-new"), &["create", "T", "--type", "task", "--silent"], &[]);
    assert_eq!((ans.code, ans.stdout.as_str(), ans.stderr.as_str()), (0, "sp-new\n", ""), "{ans:?}");
    assert_eq!(state(&w, "sp-new").map(|r| r.state), Some("READY".to_string()));
    let ans = served(&w, &Filing(&w, "sp-ev"), &["create", "Probe", "--type", "event", "--silent"], &[]);
    assert_eq!(ans.code, 0, "{ans:?}");
    assert!(state(&w, "sp-ev").is_none(), "a rowless type gets no row");
}

/// `World` behind the store client's transport.
struct InProcess(std::sync::Mutex<World>);

impl spira_store::lc_call::Transport for InProcess {
    fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
        let world = self.0.lock().unwrap();
        let mut m = &*world;
        let (code, out) = m.call(argv);
        Ok(spira_store::lc_call::Reply { code, out, err: String::new(), read_ended: false })
    }
}

#[test_id(t0w07s6)]
#[test]
fn a_content_create_gives_the_row_the_repo_the_create_was_labelled_with() {
    let w = World::new();
    let ans = served(&w, &Filing(&w, "sp-new"), &["create", "T", "--silent", "--labels", "repo:spira"], &[]);
    assert_eq!((ans.code, ans.stderr.as_str()), (0, ""), "{ans:?}");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&w.ok(&["list", "--live"])).unwrap();
    let row = rows.iter().find(|r| r["bead_id"] == "sp-new").expect("the row exists");
    assert_eq!(row["repo"], "spira", "{row}");
}

fn listed_row(world: &InProcess, id: &str) -> serde_json::Value {
    let live = world.0.lock().unwrap().ok(&["list", "--live"]);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&live).unwrap();
    rows.into_iter().find(|r| r["bead_id"] == id).unwrap_or_else(|| panic!("no row for {id}: {live}"))
}

fn sync_after_write(client: &spira_store::lc_call::LcClient, args: &[&str], stdout: &str) -> Result<(), String> {
    let ids = spira_store::planning::claim_inputs_changed(args, stdout);
    let argv: Vec<&str> = std::iter::once("sync-claim-inputs").chain(ids.iter().map(String::as_str)).collect();
    let reply = client.call(&argv)?;
    if reply.code == 0 { Ok(()) } else { Err(reply.text()) }
}

fn filed_world(labels: &[&str], parent: Option<&str>) -> (std::sync::Arc<InProcess>, spira_store::lc_call::LcClient) {
    let world = std::sync::Arc::new(InProcess(std::sync::Mutex::new(World::new())));
    world.0.lock().unwrap().file("sp-new", "task", parent, labels);
    let client = spira_store::lc_call::LcClient::over(world.clone(), true);
    world.0.lock().unwrap().ok(&["create-bead", "sp-new", "--ready"]);
    (world, client)
}

#[test_id(tz8i1mr)]
#[test]
fn a_filed_beads_parent_and_labels_are_on_its_record_once_the_write_is_synced() {
    // literal-ok: test fixture label
    let (world, client) = filed_world(&["repo:spira", "no-loop"], Some("sp-e"));
    assert_eq!(listed_row(&world, "sp-new")["claim"], serde_json::Value::Null, "nothing has synced it yet");
    // literal-ok: test fixture label
    sync_after_write(&client, &["create", "T", "--silent", "--parent", "sp-e", "--labels", "repo:spira,no-loop"], "sp-new\n").unwrap();
    // literal-ok: test fixture label
    assert_eq!(listed_row(&world, "sp-new")["claim"], serde_json::json!({"blocks": [], "parent": "sp-e", "labels": ["repo:spira", "no-loop"]}));
}

#[test_id(thq64lw)]
#[test]
fn a_dep_add_then_a_dep_remove_move_the_records_blocks_and_a_related_edge_does_not() {
    let (world, client) = filed_world(&[], None);
    let blocks = || listed_row(&world, "sp-new")["claim"]["blocks"].clone();
    let edge = |to: &str, kind: &str| world.0.lock().unwrap().plan.borrow_mut().get_mut("sp-new").unwrap().dependencies.push((to.into(), kind.into()));
    edge("sp-r", "related");
    sync_after_write(&client, &["dep", "relate", "sp-new", "sp-r"], "").unwrap();
    assert_eq!(blocks(), serde_json::json!([]));
    edge("sp-b", "blocks");
    sync_after_write(&client, &["dep", "add", "sp-new", "sp-b"], "").unwrap();
    assert_eq!(blocks(), serde_json::json!(["sp-b"]));
    world.0.lock().unwrap().plan.borrow_mut().get_mut("sp-new").unwrap().dependencies.retain(|(to, _)| to != "sp-b");
    sync_after_write(&client, &["dep", "remove", "sp-new", "sp-b"], "").unwrap();
    assert_eq!(blocks(), serde_json::json!([]));
}

#[test_id(ta9ccnm)]
#[test]
fn a_label_added_through_the_content_door_is_on_the_record_in_the_same_request() {
    struct Labelling<'w>(&'w World);
    impl crate::composed::Content for Labelling<'_> {
        fn read(&self, _: &[String]) -> Result<String, crate::composed::ReadError> {
            Ok(String::new())
        }
        fn write(&self, _: &[&str]) -> Result<String, String> {
            Ok(String::new())
        }
        fn write_stdin(&self, args: &[String], _: Option<&str>) -> Result<String, String> {
            if let ["label", "add", id, label] = args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                self.0.plan.borrow_mut().get_mut(*id).unwrap().labels.push(label.to_string());
            }
            Ok(String::new())
        }
        fn mutate(&self, m: &crate::content_write::Mutation) -> Result<String, String> {
            self.write_stdin(&m.argv, None)
        }
        fn delete(&self, _: &[String]) -> Result<(), String> {
            Ok(())
        }
        fn ask_label(&self) -> Result<String, String> {
            Ok(String::new())
        }
        fn registry(&self) -> spira_config::repos::Registry {
            spira_config::repos::Registry::new(Some("spira|r/spira|main|gate.sh\n"), &Default::default(), std::path::Path::new("r/spira"))
        }
    }
    let w = World::new();
    filed(&w, "sp-a");
    let ans = served(&w, &Labelling(&w), &["label", "add", "sp-a", "incident"], &[]);
    assert_eq!((ans.code, ans.stderr.as_str()), (0, ""), "{ans:?}");
    let row: serde_json::Value = serde_json::from_str::<Vec<serde_json::Value>>(&w.ok(&["list", "--live"])).unwrap().into_iter().find(|r| r["bead_id"] == "sp-a").unwrap();
    assert_eq!(row["claim"]["labels"], serde_json::json!(["incident"]), "{row}");
}

#[test_id(t9gtki6)]
#[test]
fn set_carrier_names_the_integration_on_each_member_and_clear_takes_it_off_and_a_member_without_a_record_is_named() {
    let w = World::new();
    filed(&w, "sp-a");
    filed(&w, "sp-b");
    let carried = |id: &str| -> serde_json::Value {
        serde_json::from_str::<Vec<serde_json::Value>>(&w.ok(&["list", "--live"])).unwrap().into_iter().find(|r| r["bead_id"] == id).unwrap()["carried_by"].clone()
    };
    assert_eq!(carried("sp-a"), serde_json::Value::Null);
    w.ok(&["set-carrier", "sp-a", "sp-b", "--by", "sp-int"]);
    assert_eq!((carried("sp-a"), carried("sp-b")), ("sp-int".into(), "sp-int".into()));
    let (code, out) = w.lc(&["set-carrier", "sp-a", "sp-ghost", "sp-b", "--clear"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("sp-ghost") && !out.contains("sp-a"), "{out}");
    assert_eq!((carried("sp-a"), carried("sp-b")), (serde_json::Value::Null, serde_json::Value::Null), "the rest are written");
}

#[test_id(tzzq6gy)]
#[test]
fn set_carrier_only_if_unset_keeps_a_member_already_carried_and_sets_one_that_is_not() {
    let world = std::sync::Arc::new(InProcess(std::sync::Mutex::new(World::new())));
    {
        let w = world.0.lock().unwrap();
        filed(&w, "sp-a");
        filed(&w, "sp-b");
        w.ok(&["set-carrier", "sp-a", "--by", "sp-i1"]);
    }
    let client = spira_store::lc_call::LcClient::over(world.clone(), true);
    let reply = client.call(&["set-carrier", "sp-a", "sp-b", "--only-if-unset", "--by", "sp-i2"]).unwrap();
    assert_eq!(reply.code, 0, "{reply:?}");
    assert!(reply.out.contains("kept sp-a") && !reply.out.contains("kept sp-b"), "{reply:?}");
    assert_eq!(listed_row(&world, "sp-a")["carried_by"], "sp-i1");
    assert_eq!(listed_row(&world, "sp-b")["carried_by"], "sp-i2");
}

#[test_id(tu6uxuz)]
#[test]
fn a_claim_input_sync_that_fails_after_the_content_write_is_reported_with_its_repair_and_the_write_stands() {
    let w = World::new();
    filed(&w, "sp-a");
    w.plan.borrow_mut().remove("sp-a");
    let ans = served(&w, &Filing(&w, "sp-a"), &["label", "add", "sp-a", "x"], &[]);
    assert_eq!(ans.code, 0, "{ans:?}");
    assert!(ans.stderr.contains("claim inputs not written; repair: spira-lc sync-claim-inputs sp-a"), "{ans:?}");
}

fn create_bead_served(w: &World) -> super::Answer {
    let req = super::Request { argv: ["create-bead", "sp-new", "--repo", "spira", "--ready"].map(String::from).to_vec(), stdin: None, caller: Default::default(), task: None, request_id: None };
    let (mut m, mut b) = (w, w);
    super::answer(req, &mut m, &mut b, &Content::default(), &|| Err("no run dir here".to_string()))
}

#[test_id(tefq3n8)]
#[test]
fn a_row_created_over_the_socket_carries_its_claim_inputs_from_the_content_store() {
    let w = World::new();
    w.file("sp-new", "task", Some("sp-e"), &["incident"]);
    let ans = create_bead_served(&w);
    assert_eq!((ans.code, ans.stderr.as_str()), (0, ""), "{ans:?}");
    let row = {
        let rows: Vec<serde_json::Value> = serde_json::from_str(&w.ok(&["list", "--live"])).unwrap();
        rows.into_iter().find(|r| r["bead_id"] == "sp-new").unwrap()
    };
    assert_eq!(row["claim"], serde_json::json!({"blocks": [], "parent": "sp-e", "labels": ["incident"]}));
    assert_ne!(row["claim_view"]["verdict"], "cannot-tell", "{row}");
}

#[test_id(tomgplr)]
#[test]
fn a_create_whose_claim_sync_fails_stands_and_names_the_repair() {
    let w = World::new();
    w.file("sp-new", "task", Some("sp-e"), &["incident"]);
    w.plan.borrow_mut().remove("sp-new");
    let ans = create_bead_served(&w);
    assert_eq!(ans.code, 0, "{ans:?}");
    assert!(state(&w, "sp-new").is_some(), "the row stands");
    assert!(ans.stderr.contains("repair: spira-lc sync-claim-inputs sp-new"), "{ans:?}");
}

#[test_id(tj6uut5)]
#[test]
fn sync_claim_inputs_only_if_unset_keeps_a_written_claim_and_fills_a_null_one() {
    let (world, client) = filed_world(&["incident"], Some("sp-e"));
    {
        let w = world.0.lock().unwrap();
        w.file("sp-set", "task", None, &["other"]);
        w.ok(&["create-bead", "sp-set", "--ready"]);
        w.ok(&["set-claim-inputs", "sp-set", "--block", "sp-b"]);
    }
    let reply = client.call(&["sync-claim-inputs", "--only-if-unset", "sp-set", "sp-new"]).unwrap();
    assert_eq!(reply.code, 0, "{}", reply.text());
    assert!(reply.text().contains("kept sp-set: already written") && !reply.text().contains("kept sp-new"), "{}", reply.text());
    assert_eq!(listed_row(&world, "sp-set")["claim"], serde_json::json!({"blocks": ["sp-b"], "parent": null, "labels": []}));
    assert_eq!(listed_row(&world, "sp-new")["claim"], serde_json::json!({"blocks": [], "parent": "sp-e", "labels": ["incident"]}));
}

/// The served path's machine: the primitives only, refusing a composed verb as `Local` does.
/// `stale` names records whose `list` row still reads `claim: null`, as when another writer's
/// claim lands between the sweep's list and its write.
struct Primitives<'w> {
    world: &'w World,
    stale: &'w [&'w str],
}

impl crate::callers::Machine for Primitives<'_> {
    fn call(&mut self, args: &[String]) -> (i32, String) {
        if args.first().is_some_and(|v| crate::composed::is_served(v)) {
            return (2, "usage: spira-lc show <bead-id> | show-batch ...".to_string());
        }
        let mut w = self.world;
        let (code, out) = w.call(args);
        if args.first().map(String::as_str) != Some("list") || self.stale.is_empty() {
            return (code, out);
        }
        let mut rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        rows.iter_mut().filter(|r| self.stale.contains(&r["bead_id"].as_str().unwrap_or(""))).for_each(|r| r["claim"] = serde_json::Value::Null);
        (code, serde_json::Value::Array(rows).to_string())
    }
}

#[test_id(t7m0bkf)]
#[test]
fn a_served_backfill_sets_a_null_claim_and_keeps_a_written_one() {
    let w = World::new();
    w.file("sp-new", "task", Some("sp-e"), &["incident"]);
    w.ok(&["create-bead", "sp-new", "--ready"]);
    w.file("sp-set", "task", None, &["other"]);
    w.ok(&["create-bead", "sp-set", "--ready"]);
    w.ok(&["set-claim-inputs", "sp-set", "--block", "sp-b"]);
    let req = super::Request {
        argv: ["backfill-claim-inputs", "--apply", "sp-new", "sp-set"].iter().map(|a| a.to_string()).collect(),
        stdin: None,
        caller: Default::default(),
        task: None,
        request_id: None,
    };
    let ans = super::answer(req, &mut Primitives { world: &w, stale: &["sp-set"] }, &mut &w, &Content::default(), &|| Err("no run dir here".to_string()));
    assert_eq!(ans.code, 0, "{}{}", ans.stdout, ans.stderr);
    assert!(ans.stdout.contains("1 set, 1 kept, 0 failed"), "{}", ans.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&w.ok(&["list", "--live"])).unwrap();
    let claim = |id: &str| rows.iter().find(|r| r["bead_id"] == id).unwrap()["claim"].clone();
    assert_eq!(claim("sp-new"), serde_json::json!({"blocks": [], "parent": "sp-e", "labels": ["incident"]}));
    assert_eq!(claim("sp-set"), serde_json::json!({"blocks": ["sp-b"], "parent": null, "labels": []}));
}

#[test_id(tscocfw)]
#[test]
fn syncing_an_id_with_no_record_names_it_and_makes_none() {
    let world = std::sync::Arc::new(InProcess(std::sync::Mutex::new(World::new())));
    world.0.lock().unwrap().file("sp-ghost", "task", None, &[]);
    let client = spira_store::lc_call::LcClient::over(world.clone(), true);
    let reply = client.call(&["sync-claim-inputs", "sp-ghost"]).unwrap();
    assert_ne!(reply.code, 0, "{reply:?}");
    assert!(reply.text().contains("sp-ghost"), "{reply:?}");
    assert_eq!(world.0.lock().unwrap().ok(&["list", "--live"]).trim(), "[]");
}

#[test_id(tjl0s52)]
#[test]
fn a_lifecycle_row_the_server_cannot_write_is_reported_and_bds_create_stands() {
    let w = World::new();
    let ans = served(&w, &Filing(&w, "not an id"), &["create", "T", "--silent"], &[]);
    assert_eq!((ans.code, ans.stdout.as_str()), (0, "not an id\n"), "bd's create stands: no retry of it");
    assert!(ans.stderr.starts_with("spira-lc content: LIFECYCLE: cannot read any created bead id"), "reported, never swallowed: {ans:?}");
}

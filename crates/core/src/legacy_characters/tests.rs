use super::*;

const OLD: &str = include_str!("old_schema.sql");
const ALICE: &str = "a11ce000-0000-4000-8000-000000000001";
const BOB: &str = "b0b00000-0000-4000-8000-000000000002";
const SKIN: &str = "5k1n0000-0000-4000-8000-000000000003";
const WORK: &str = "w0rk0000-0000-4000-8000-000000000004";

/// 一个小的旧库（hybrid）：作品、人物、皮肤，各种链接，相册与人工结论，候选，一条继承层级关系。
fn library() -> Connection {
    library_from(OLD)
}

fn library_from(old: &str) -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    crate::configure(&c).unwrap();
    c.execute_batch(old).unwrap();
    c.execute_batch(&format!(
        "INSERT INTO meta VALUES ('identity_policy','hybrid');
         INSERT INTO assets(uuid) VALUES ('a'),('b'),('c');
         INSERT INTO entities(id,name,work,kind,work_id) VALUES ('{WORK}','','','work',NULL),('{ALICE}','','Example','character','{WORK}'),
          ('{BOB}','','Example','character',NULL);
         INSERT INTO entities(id,name,kind,parent_id,variant,mascot,mascot_origin) VALUES ('{SKIN}','','skin','{ALICE}','alice_(summer)',1,'user');
         INSERT INTO work_review VALUES ('{WORK}','confirmed','',1);
         INSERT INTO links(system,key,entity_id,current,source,added_at) VALUES
          ('ccip','alice','{ALICE}',1,'import','2026-09-01 00:00:00'),('wd','alice_tag','{ALICE}',1,'import','2026-09-01 00:00:00'),
          ('danbooru','bob','{BOB}',0,'user','2026-09-02 00:00:00'),('qualifier','example','{WORK}',1,'import','2026-09-01 00:00:00');
         INSERT INTO identity_baseline VALUES ('{ALICE}',NULL,'','character',NULL),('{SKIN}',NULL,'alice_(summer)','character',NULL),
          ('{BOB}',NULL,'','character',NULL);
         INSERT INTO identity_relations(id,a,b,kind,status) VALUES (7,'{SKIN}','{ALICE}','form_of','accepted');
         INSERT INTO identity_evidence VALUES (7,'{fp}','legacy-hierarchy','{{\"inherited_parent_id\":\"x\"}}','2026-09-03 00:00:00');
         INSERT INTO identity_decisions VALUES (3,7,'proposed','accepted','保留既有层级并使其可撤销；不代表新增人工确认','2026-09-03 00:00:00');
         INSERT INTO actions(label,before_json) VALUES ('否定','[{{\"uuid\":\"a\",\"entity\":\"{ALICE}\",\"assertion\":{{\"status\":\"confirmed\",\"origin\":\"album\"}},\"deferred\":false}}]');
         INSERT INTO assertions VALUES ('a','{ALICE}','rejected','review'),('b','{BOB}','confirmed','legacy_import');
         INSERT INTO candidates VALUES ('a','{ALICE}','WD',0.9),('b','{ALICE}','CCIP',0.95),('b','{BOB}','WD',0.7);",
        fp = "ab".repeat(32)
    ))
    .unwrap();
    crate::migrate(&mut c).unwrap();
    c
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |x| x.get(0)).unwrap()
}

#[test]
fn a_small_library_syncs_with_no_differences_and_twice_adds_nothing() {
    let mut c = library();
    let r = sync(&mut c).unwrap();
    assert!(r.committed, "{}", r.to_json());
    assert_eq!(r.tables.len(), 10);
    let rows = |c: &Connection| -> Vec<i64> {
        [
            "terms",
            "term_tree",
            "term_links",
            "term_verdicts",
            "term_batches",
            "term_evidence",
            "term_evidence_set_rows",
            "term_legacy_ids",
        ]
        .iter()
        .map(|t| count(c, &format!("SELECT count(*) FROM {t}")))
        .collect()
    };
    let before = rows(&c);
    let again = sync(&mut c).unwrap();
    assert!(again.committed);
    assert_eq!(rows(&c), before);
    assert_eq!(again.verdicts_added + again.terms_created + again.evidence_added + again.batches_added, 0);
    // 链接改名、相册值进批次、没有事件的旧结论进锚点 0 的基线。
    assert_eq!(count(&c, "SELECT count(*) FROM term_links WHERE system='wd_character' AND key='alice_tag'"), 1);
    assert_eq!(count(&c, "SELECT anchor FROM term_batches WHERE kind='album'"), 0);
    assert_eq!(count(&c, "SELECT count(*) FROM term_verdicts WHERE via='legacy_import' AND anchor=0"), 1);
    assert_eq!(count(&c, "SELECT count(*) FROM term_verdicts WHERE action_id=1 AND verdict='no' AND via='review'"), 1);
    // 继承层级：迁移批次上的一条 yes，未复核（没有 reviewed）。
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM term_relation_latest WHERE verdict='yes' AND via='migration' AND reviewed='[]'"
        ),
        1
    );
}

#[test]
fn the_comparison_notices_one_changed_row_in_every_compat_table() {
    let mut c = library();
    assert!(sync(&mut c).unwrap().committed);
    // 每张影子表改一行（持钥匙），比对必须报出来。
    let tampers = [
        ("entities", format!("UPDATE shadow_entities SET work='改过' WHERE id='{BOB}'")),
        ("links", "UPDATE shadow_links SET current=1-current WHERE key='bob'".into()),
        ("work_review", "UPDATE shadow_work_review SET note='改过'".into()),
        ("assertions", "UPDATE shadow_assertions SET origin='sweep' WHERE uuid='a'".into()),
        ("candidates", "UPDATE shadow_candidates SET score=score+1e-12 WHERE uuid='a'".into()),
        ("identity_relations", "UPDATE shadow_identity_relations SET status='proposed'".into()),
        ("identity_evidence", "UPDATE shadow_identity_evidence SET payload=payload||' '".into()),
        ("identity_decisions", "UPDATE shadow_identity_decisions SET previous_status='rejected'".into()),
        ("identity_decision_evidence", "INSERT INTO shadow_identity_decision_evidence VALUES (3,'x')".into()),
        ("identity_baseline", format!("UPDATE term_identity_baseline SET variant='x' WHERE id='{BOB}'")),
    ];
    for (table, sql) in tampers {
        let tx = c.transaction().unwrap();
        begin_writing(&tx).unwrap();
        tx.execute(&sql, []).unwrap();
        let mut r = Report::default();
        compare(&tx, true, &mut r).unwrap();
        let diff = &r.tables[table];
        assert!(diff["missing"].as_i64().unwrap() + diff["extra"].as_i64().unwrap() > 0, "{table}: {diff}");
        assert!(!r.clean());
        drop(tx);
    }
}

#[test]
fn shadow_tables_copy_the_old_shape_without_foreign_keys() {
    let sql = "CREATE TABLE links(system TEXT NOT NULL,key TEXT NOT NULL,entity_id TEXT NOT NULL REFERENCES entities(id),\n current INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(system,key))";
    assert_eq!(
        shadow_ddl(sql, "links").unwrap(),
        "CREATE TABLE shadow_links(system TEXT NOT NULL,key TEXT NOT NULL,entity_id TEXT NOT NULL ,\n current INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(system,key))"
    );
}

fn target(u: &str, e: &str) -> Target {
    (u.into(), e.into())
}

fn row(s: &str, o: &str) -> Row {
    (s.into(), o.into())
}

#[test]
fn reconstruction_places_values_that_have_no_step() {
    let t = target("c", ALICE);
    let mut events = BTreeMap::new();
    // 第 3 步合并清掉了源身份上的结论，之后相册导入又写了一行，第 9 步看到了它。
    events.insert(
        t.clone(),
        vec![
            (3, Event::MergeClear { before: row("confirmed", "review"), fixed: false }),
            (9, Event::Snapshot { before: Some(row("confirmed", "album")) }),
        ],
    );
    let current = BTreeMap::from([(t.clone(), row("rejected", "review"))]);
    let mut r = Report::default();
    let wants = reconstruct(&events, &current, &mut r);
    assert_eq!(r.unexplained_count, 0, "{:?}", r.unexplained);
    let shape: Vec<(String, &str, String)> = wants
        .iter()
        .map(|w| {
            let at = match w.at {
                At::Action(a) => format!("action {a}"),
                At::Batch { kind, anchor } => format!("{kind} @{anchor}"),
            };
            (at, w.verdict, w.via.clone())
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("baseline @2".into(), "yes", "review".into()),
            ("action 3".into(), "clear", "merge".into()),
            ("album @8".into(), "yes", "album".into()),
            ("action 9".into(), "no", "review".into()),
        ]
    );
    // hybrid 库里的旧合并：搬动进锚点为那一步的迁移批次，不挂在那一步上。
    let mut fixed = BTreeMap::new();
    fixed.insert(t.clone(), vec![(3, Event::MergeSet { set: row("confirmed", "sweep"), fixed: true })]);
    let now = BTreeMap::from([(t.clone(), row("confirmed", "sweep"))]);
    let wants = reconstruct(&fixed, &now, &mut r);
    assert!(matches!(wants[0].at, At::Batch { kind: "migration", anchor: 3 }));
}

#[test]
fn reconstruction_refuses_what_history_cannot_explain() {
    let t = target("c", ALICE);
    type Case = (Vec<(i64, Event)>, Option<Row>); // (事件序列, 当前值)
    let cases: Vec<Case> = vec![
        // 这一步应写 review / sweep，之后却是相册行。
        (vec![(4, Event::Snapshot { before: None })], Some(row("confirmed", "album"))),
        // 这一步之后的值被删了（没有撤销、也不是合并）。
        (vec![(4, Event::Snapshot { before: None })], None),
        // 合并清掉之后出现了非相册的值。
        (
            vec![(4, Event::MergeClear { before: row("confirmed", "review"), fixed: false })],
            Some(row("rejected", "review")),
        ),
    ];
    for (list, now) in cases {
        let mut r = Report::default();
        let events = BTreeMap::from([(t.clone(), list)]);
        let current = now.map(|v| BTreeMap::from([(t.clone(), v)])).unwrap_or_default();
        reconstruct(&events, &current, &mut r);
        assert_eq!(r.unexplained_count, 1, "{:?}", r.unexplained);
    }
}

#[test]
fn preflight_refuses_and_leaves_nothing() {
    for (sql, check) in [
        (format!("UPDATE entities SET kind='oc' WHERE id='{BOB}'"), "entities.kind 不是 character / skin / work"),
        (
            format!("INSERT INTO identity_relations(a,b,kind) VALUES ('{ALICE}','{BOB}','duplicate_of'),('{BOB}','{ALICE}','duplicate_of')"),
            "同一对身份有两个方向的 duplicate_of",
        ),
        ("INSERT INTO candidates VALUES ('c','nobody','WD',0.5)".into(), "候选指向不存在的身份"),
        ("UPDATE meta SET value='v2' WHERE key='identity_policy'".into(), "只支持身份策略未设置或 hybrid"),
    ] {
        let mut c = library();
        c.execute(&sql, []).unwrap();
        let r = sync(&mut c).unwrap();
        assert!(!r.committed);
        assert!(r.preflight.iter().any(|p| p["check"] == check), "{check}: {:?}", r.preflight);
        assert_eq!(count(&c, "SELECT count(*) FROM terms"), 0);
        assert_eq!(count(&c, "SELECT count(*) FROM sqlite_master WHERE name LIKE 'shadow_%'"), 0);
    }
}

/// 真实「我的图库」的 entities 由历次 ALTER TABLE 加列，work_id、original 排在 label_only 之前（S5 副本上发现）。
#[test]
fn a_library_whose_columns_came_in_another_order_syncs_the_same() {
    const REAL: &str = "CREATE TABLE entities(id TEXT PRIMARY KEY,name TEXT NOT NULL,work TEXT NOT NULL DEFAULT '',external_key TEXT UNIQUE, variant TEXT NOT NULL DEFAULT '', parent_id TEXT REFERENCES entities(id), tag TEXT, merged_into TEXT REFERENCES entities(id), kind TEXT NOT NULL DEFAULT '', artist TEXT NOT NULL DEFAULT '', work_id TEXT REFERENCES entities(id), original INTEGER NOT NULL DEFAULT 0, label_only INTEGER NOT NULL DEFAULT 0, mascot INTEGER NOT NULL DEFAULT 0, mascot_origin TEXT);";
    let line = OLD.lines().find(|l| l.starts_with("CREATE TABLE entities(")).unwrap();
    let mut c = library_from(&OLD.replace(line, REAL));
    assert_ne!(columns(&c, "entities").unwrap(), ENTITY_COLUMNS);
    let r = sync(&mut c).unwrap();
    assert!(r.committed, "{}", r.to_json());
    assert_eq!(
        c.query_row("SELECT sql FROM sqlite_master WHERE name='shadow_entities'", [], |x| x.get::<_, String>(0))
            .unwrap(),
        shadow_ddl(REAL.trim_end_matches(';'), "entities").unwrap()
    );
    let first = count(&c, "SELECT count(*) FROM term_verdicts");
    assert!(sync(&mut c).unwrap().committed);
    assert_eq!(count(&c, "SELECT count(*) FROM term_verdicts"), first);
    // 列的集合不同（这里多一列）仍拒绝。
    let mut c =
        library_from(&OLD.replace(line, &REAL.replace(", mascot_origin TEXT", ", mascot_origin TEXT, extra TEXT")));
    let r = sync(&mut c).unwrap();
    assert!(!r.committed);
    assert!(r.preflight.iter().any(|p| p["check"] == "旧表的列与收编约定不同"), "{:?}", r.preflight);
}

#[test]
fn refused_after_characters_moved_to_the_core() {
    let mut c = library();
    c.execute("INSERT INTO meta VALUES (?1,'core')", [CHARACTERS_IN_CORE]).unwrap();
    assert!(matches!(sync(&mut c), Err(Error::Conflict(_))));
}

#[test]
fn candidate_scope_anchor_appears_once_under_both_policies() {
    for select in [CANDIDATES_RESOLVED, compat_select("candidates").unwrap()] {
        let scoped = crate::character_projection::scoped(select, CANDIDATE_SCOPE.0, CANDIDATE_SCOPE.1).unwrap();
        assert_eq!(scoped.matches("FROM candidate_scope scoped").count(), 1);
    }
}

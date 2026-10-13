//! Host-local information with research workers absent. Does not start tasks or grant network access.
use crate::{
    http::LibraryContext,
    http_json::Json,
    http_read::{rows, strings},
    Error, Result,
};
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
pub(crate) const ROUTES: &[&str] =
    &["/api/about", "/api/sync", "/api/names", "/api/index", "/api/provenance", "/api/accounts", "/api/oc/groups"];
fn off() -> Json {
    Json::object([("status", "off".into())])
}
pub(crate) fn read(context: &LibraryContext, url: &str) -> Result<Json> {
    let q = crate::http_read::query(url);
    let path = url.split('?').next().unwrap_or(url);
    match path {
        "/api/about" => return Ok(about()),
        "/api/index" => return Ok(Json::object([("state", off())])),
        "/api/provenance" => return Ok(Json::object([("available", false.into())])),
        // OC sub-grouping is an on-demand research computation, never a stored cache.
        "/api/oc/groups" => return Err(Error::Invalid("图像特征未加载，暂时无法按相似细分".into())),
        "/api/accounts" => return accounts(context),
        _ => {}
    }
    let mut c = crate::sqlite::open_with_flags(&context.db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    let tx = c.transaction()?;
    let result = match path {
        "/api/sync" => {
            let mut history = rows(&tx, "SELECT * FROM sync_runs ORDER BY id DESC LIMIT 8", &[])?;
            for r in &mut history {
                let s = r.get("sources").expect("app_local internal invariant").text();
                r.set("sources", if s.is_empty() { None::<String>.into() } else { crate::http_json::parse_python(s)? });
            }
            Json::object([("state", off()), ("history", Json::Array(history))])
        }
        "/api/names" => names(&tx, q.get("progress").is_some_and(|p| p == "1"))?,
        _ => return Err(Error::Invalid("未迁移宿主接口".into())),
    };
    tx.commit()?;
    Ok(result)
}
fn about() -> Json {
    let platform = crate::platform_system::platform();
    Json::object([
        ("name", env!("ILLUSTASH_NAME").into()),
        ("version", env!("ILLUSTASH_VERSION").into()),
        ("channel", env!("ILLUSTASH_CHANNEL").into()),
        ("build", env!("ILLUSTASH_BUILD").into()),
        ("schema", 36i64.into()),
        ("python", None::<String>.into()),
        ("platform", platform.into()),
        (
            "libraries",
            Json::object([
                ("Pillow", None::<String>.into()),
                ("NumPy", None::<String>.into()),
                ("ONNX Runtime", None::<String>.into()),
            ]),
        ),
        ("runtime", env!("ILLUSTASH_RUSTC").into()),
    ])
}
pub(crate) fn names(c: &Connection, progress: bool) -> Result<Json> {
    let mut out = Json::object([(
        "state",
        Json::object([("status", "off".into()), ("done", 0i64.into()), ("total", 0i64.into())]),
    )]);
    if progress {
        return Ok(out);
    }
    let active:HashSet<_>=strings(c,"SELECT entity FROM memberships UNION SELECT entity FROM candidates UNION SELECT parent_id FROM entities WHERE id IN (SELECT entity FROM memberships UNION SELECT entity FROM candidates) AND parent_id IS NOT NULL",&[])?.into_iter().collect();
    // One library-wide load serves both the scope (formerly a second read of entities and links) and the choices (#102).
    let view = crate::names_view::View::load_with_work(c)?;
    let mut keys: [HashSet<String>; 3] = Default::default();
    for e in view.identities() {
        if e.merged_into.is_some()
            || matches!(e.kind, "oc" | "work")
            || e.original
            || !e.linked
            || !active.contains(e.id)
        {
            continue;
        }
        if e.parent.is_none() {
            keys[0].insert(e.tag.into());
        }
        for (i, s) in [(1, e.work), (2, e.variant)] {
            if !s.is_empty() {
                keys[i].insert(s.into());
            }
        }
    }
    let mut counts = Json::Object(Vec::new());
    for (i, (plural, kind)) in
        [("characters", "character"), ("works", "work"), ("skins", "skin")].into_iter().enumerate()
    {
        let mut zh = 0;
        let mut ja = 0;
        let mut readable = 0;
        for key in &keys[i] {
            let choice = view.choose(kind, key)?;
            zh += i64::from(choice.names.first().and_then(|x| x.as_ref()).is_some_and(|s| !s.is_empty()));
            ja += i64::from(choice.names.get(1).and_then(|x| x.as_ref()).is_some_and(|s| !s.is_empty()));
            readable += i64::from(choice.source == "readable");
        }
        counts.set(
            plural,
            Json::object([
                ("total", (keys[i].len() as i64).into()),
                ("zh", zh.into()),
                ("ja", ja.into()),
                ("readable", readable.into()),
            ]),
        );
    }
    out.set("counts", counts);
    let settings: HashMap<_, _> = c
        .prepare("SELECT key,value FROM name_settings")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let key = settings.get("model_key").or_else(|| settings.get("deepseek_key")).filter(|s| !s.is_empty());
    out.set(
        "model_api",
        Json::object([
            ("configured", key.is_some().into()),
            ("hint", key.map(|s| s.chars().skip(s.chars().count().saturating_sub(4)).collect::<String>()).into()),
            (
                "model",
                settings
                    .get("model_name")
                    .or_else(|| settings.get("deepseek_model"))
                    .map(String::as_str)
                    .unwrap_or("deepseek-chat")
                    .into(),
            ),
            ("base", settings.get("model_base").map(String::as_str).unwrap_or("https://api.deepseek.com").into()),
        ]),
    );
    Ok(out)
}
fn accounts(context: &LibraryContext) -> Result<Json> {
    let mut out = crate::app_accounts::status()?;
    let mut x = Json::object([("imported", 0i64.into())]);
    if let Some(path) = context.library.as_ref().map(|p| p.join("provenance.sqlite")).filter(|p| p.exists()) {
        let c = crate::sqlite::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut resolved = 0;
        let mut pending = 0;
        let mut total = 0;
        for r in rows(&c, "SELECT status,count(*) AS n FROM leads WHERE source='x_following' GROUP BY status", &[])? {
            let n = r.get("n").expect("app_local internal invariant").integer();
            total += n;
            match r.get("status").expect("app_local internal invariant").text() {
                "resolved" => resolved = n,
                "pending" => pending = n,
                _ => {}
            }
        }
        let time = rows(&c, "SELECT max(added_at) AS t FROM leads WHERE source='x_following'", &[])?
            .remove(0)
            .get("t")
            .expect("app_local internal invariant")
            .clone();
        x = Json::object([
            ("imported", total.into()),
            ("resolved", resolved.into()),
            ("pending", pending.into()),
            ("imported_at", time),
        ]);
    }
    out.set("x", x);
    let name = context
        .library
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p.join("library.json")).ok())
        .and_then(|s| crate::http_json::parse_python(&s).ok())
        .and_then(|v| v.get("name").cloned())
        .unwrap_or_else(|| None::<String>.into());
    out.set("library", name);
    Ok(out)
}

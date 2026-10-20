//! 会话 POST 业务路由；入口安全检查由 app_http 保持。
use super::http_host_get::{expired, reply, reply_if_current, waited};
use super::*;
use std::sync::RwLockWriteGuard;

/// Requests a session answers while its package is damaged: none of them writes to the package's databases.
const DAMAGED_ALLOWS: [&str; 7] = [
    "/api/integrity/rescue",
    "/api/libraries/open",
    "/api/libraries/new",
    "/api/libraries/import",
    "/api/libraries/forget",
    "/api/backup",
    "/api/preferences",
];
type Translate<'a> = &'a dyn Fn(&str) -> String;

pub(super) fn handle(request: Request, host: &Host, received: u64, text: &str, body: Json) {
    // A native chooser's prompt in the page's language (#173).
    let language = header(&request, "X-Illustash-Language").map(str::to_owned);
    let translate = |text: &str| crate::app_preferences::tr(&host.root, language.as_deref(), text);
    match request.url() {
        "/api/libraries/choose" => return choose(request, host, received, &body, &translate),
        // App-wide and possibly a network round trip (#191): never holds the session, so switching is not delayed.
        "/api/updates" => return reply(request, crate::app_updates::post(&body), true),
        "/api/models" => return models(request, host, body, &translate),
        _ => {}
    }
    let mut body = body;
    if request.url() == "/api/accounts/x/import" && !body.get("path").is_some_and(truthy) {
        match archive(host, received, &translate) {
            Picked::Path(path) => body.set("path", path),
            Picked::Cancelled => return cancelled(request),
            Picked::Switched => return expired(request),
            Picked::Failed(e) => return failure(request, e, true),
        }
    }
    let mut session = host.session.write().expect("http_host_post session lock poisoned");
    if !host.accepts(&session, received) || header(&request, "X-CSRF-Token") != Some(session.token.as_str()) {
        return expired(request);
    }
    // A damaged package takes no more writes (#212): only leaving it, backing up library.sqlite and app-wide choices.
    if let Some(damage) = session.damage() {
        if !DAMAGED_ALLOWS.contains(&request.url()) {
            drop(session);
            return failure(request, damage.error(), true);
        }
    }
    match request.url() {
        "/api/cache" => return cache(request, host, session, &body),
        "/api/entities" | "/api/character/profile" | "/api/local-links" => {
            return profiles(request, host, received, session, &body)
        }
        "/api/names" if body.get("action").map(Json::text) == Some("model_api_test") => {
            let result = session.names.source(&session.library);
            drop(session);
            return reply_if_current(request, host, received, result.and_then(|s| crate::app_names::test(&s)), true);
        }
        "/api/relate" => return relate(request, host, received, session, &body),
        // Imports are background tasks (#26, #88): the request validates, starts the task and answers at once; the
        // page polls GET /api/imports and may cancel. A recognition import stays bound to the library it started
        // on. Cancelling, switching libraries or quitting stops either import before its publication point.
        "/api/recognition/import" => {
            let started = recognition_import(host, &session, &body);
            drop(session);
            return reply(request, started.map(started_task), true);
        }
        "/api/libraries/import" => {
            let started = library_import(host, &session, &body);
            drop(session);
            return reply(request, started.map(started_task), true);
        }
        "/api/imports/cancel" => {
            drop(session);
            return respond(request, host.imports.cancel(crate::app_imports::CANCELLED), 200, "core");
        }
        _ => {}
    }
    match route(request.url(), host, received, &mut session, text, &body, language.as_deref()) {
        Some(result) => {
            drop(session);
            reply(request, result, true)
        }
        None => unavailable(request),
    }
}
fn cancelled(request: Request) {
    respond(request, Json::object([("cancelled", true.into())]), 200, "core");
}
fn started_task(task: Json) -> Json {
    Json::object([("started", true.into()), ("task", task)])
}
fn choose(request: Request, host: &Host, received: u64, body: &Json, translate: Translate) {
    let result = crate::app_platform::choose(
        body.get("kind").map(Json::text).unwrap_or(""),
        body.get("multiple").is_some_and(truthy),
        &crate::platform_process::stopping,
        translate,
    )
    .map_err(management_error);
    reply_if_current(request, host, received, result, true);
}
/// App-wide (#168): the model download runs on its own thread and outlives a library switch. Importing without
/// paths asks the native chooser first, without holding the session.
fn models(request: Request, host: &Host, mut body: Json, translate: Translate) {
    if body.get("action").map(Json::text) == Some("import") && body.get("paths").is_none() {
        match crate::app_platform::choose("models", true, &crate::platform_process::stopping, translate) {
            Ok(picked) => match picked.get("paths") {
                Some(paths @ Json::Array(list)) if !list.is_empty() => body.set("paths", paths.clone()),
                _ => return cancelled(request),
            },
            Err(e) => return failure(request, management_error(e), true),
        }
    }
    // Deleting the models waits while this session's recognition or character index uses them.
    let in_use = body.get("action").map(Json::text) == Some("delete") && {
        let session = host.session.read().expect("http_host_post session lock poisoned");
        session.syncer.running() || session.index.writing()
    };
    reply(request, crate::app_models::post(&host.root, &body, in_use), true);
}
enum Picked {
    Path(Json),
    Cancelled,
    /// The session switched while the chooser was open.
    Switched,
    Failed(Error),
}
/// The archive the person picks for an X import. The native picker runs without holding the session;
/// cancellation or a switch must never deliver the chosen file into a replacement package.
fn archive(host: &Host, received: u64, translate: Translate) -> Picked {
    let picked = match crate::app_platform::choose("archive", false, &crate::platform_process::stopping, translate) {
        Ok(picked) => picked,
        Err(e) => return Picked::Failed(management_error(e)),
    };
    if !host.current(received) {
        return Picked::Switched;
    }
    match picked.get("path").filter(|path| truthy(path)) {
        Some(path) => Picked::Path(path.clone()),
        None => Picked::Cancelled,
    }
}
fn cache(request: Request, host: &Host, session: RwLockWriteGuard<Session>, body: &Json) {
    if !matches!(body.get("action"), Some(Json::Scalar(serde_json::Value::String(action))) if action == "clear") {
        drop(session);
        return failure(request, Error::Invalid("未知操作".into()), true);
    }
    // Bind after framing and session validation; waiting for image work must
    // not hold the session lock or retarget this operation after a switch.
    let library = session.library.clone();
    drop(session);
    reply(request, host.images.clear(&library, &host.library_forced_off(&library)), true);
}
/// Character profile writes: their copyrights are looked up without the session lock, then written under it.
fn profiles(request: Request, host: &Host, received: u64, session: RwLockWriteGuard<Session>, body: &Json) {
    let library = session.library.clone();
    let mut context = session.context(&host.tags);
    context.db = library.instance_path().join("library.sqlite");
    context.source = Some(library.instance_path().join("source.sqlite"));
    let source = session.names.source(&library);
    drop(session);
    let prepared = source.and_then(|mut source| {
        let c = crate::app_library::read_connection(&context.db)?;
        crate::app_profiles::prepare(&c, body, request.url(), &mut source)
    });
    let session = host.session.write().expect("http_host_post session lock poisoned");
    if !host.accepts(&session, received) {
        return expired(request);
    }
    let result = prepared.and_then(|p| crate::app_commands::request_prepared(&context, request.url(), body, Some(&p)));
    drop(session);
    reply(request, result, true);
}
fn relate(request: Request, host: &Host, received: u64, mut session: RwLockWriteGuard<Session>, body: &Json) {
    let value = serde_json::from_str::<serde_json::Value>(&body.dump()).unwrap_or_default();
    let ids = value["ids"].as_array();
    if ids.is_none_or(|ids| {
        ids.iter().any(|v| !v.is_string())
            || !(2..=12).contains(
                &ids.iter()
                    .map(|v| v.as_str().expect("validated internal text field"))
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
            )
    }) {
        drop(session);
        return failure(request, Error::Invalid("请选择 2–12 张图".into()), true);
    }
    let lib = session.library.clone();
    let result = session.research.query(&lib, "relate", value);
    drop(session);
    reply_if_current(request, host, received, waited(result), true);
}
fn recognition_import(host: &Host, session: &Session, body: &Json) -> Result<Json> {
    let resume = matches!(body.get("resume"), Some(Json::Scalar(serde_json::Value::Bool(true))));
    let path = match body.get("path") {
        Some(Json::Scalar(serde_json::Value::String(path))) => Some(PathBuf::from(path)),
        _ => None,
    };
    if session.index.writing() {
        return Err(Error::Invalid("角色索引正在更新，请稍后".into()));
    }
    if !resume && path.is_none() {
        return Err(Error::Invalid("需要结果包目录 path".into()));
    }
    let identity = crate::app_tasks::TaskIdentity::new(&session.library, session.generation, &session.token)?;
    let file = path
        .as_deref()
        .filter(|_| !resume)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (library, root) = (session.library.clone(), host.root.clone());
    host.imports.start(identity, crate::app_imports::Kind::Recognition, &file, move |progress| {
        use crate::app_imports::Observer;
        let cancel = progress.cancellation();
        let out = match path.filter(|_| !resume) {
            Some(path) => crate::app_recognition::import_package_observed(
                &library,
                &path,
                &root,
                cancel,
                &crate::app_research::dispatch,
                progress,
            )?,
            None => {
                progress.stage("projecting");
                crate::app_recognition::resume(&library, cancel, &crate::app_research::dispatch)?
            }
        };
        // Keep legacy response insertion order (digest, duplicate, status, error).
        Ok(Json::Object(
            ["digest", "duplicate", "status", "error"]
                .iter()
                .filter_map(|k| out.get(*k).map(|v| ((*k).to_string(), v.clone().into())))
                .collect(),
        ))
    })
}
fn library_import(host: &Host, session: &Session, body: &Json) -> Result<Json> {
    let archive = expand_path(body.get("path").map(Json::text).unwrap_or(""))?;
    let identity = crate::app_tasks::TaskIdentity::new(&session.library, session.generation, &session.token)?;
    let file = archive.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    host.imports.start(identity, crate::app_imports::Kind::Archive, &file, move |progress| {
        let (path, name) = crate::app_archives::import(&archive, &progress.cancellation_arc(), progress)?;
        // Published: a registration failure keeps the complete package and is reported with it, like
        // creation (#97, #111); importing again would only make a second, numbered package.
        let listed = crate::app_libraries::remember_created(&path, &name);
        let mut out = Json::object([("path", path.to_string_lossy().as_ref().into()), ("name", name.into())]);
        if let Err(error) = listed {
            out.set("listed", false.into());
            out.set("error", crate::app_libraries::unlisted_reason(&error).into());
        }
        Ok(out)
    })
}
/// The routes answered once the session lock is released; none for an unknown route.
fn route(
    url: &str,
    host: &Host,
    received: u64,
    session: &mut Session,
    text: &str,
    body: &Json,
    language: Option<&str>,
) -> Option<Result<Json>> {
    Some(match url {
        "/api/tag"
        | "/api/undo"
        | "/api/artist/verdict"
        | "/api/curate"
        | "/api/fold"
        | "/api/entities"
        | "/api/character/profile"
        | "/api/local-links"
        | "/api/works"
        | "/api/character/mascot"
        | "/api/kind"
        | "/api/oc/shared"
        | "/api/change"
        | "/api/audit/answer"
        | "/api/ask/answer"
        | "/api/reassign"
        | "/api/sweep"
        | "/api/identity/decision"
        | "/api/identity/skip"
        | "/api/merge" => crate::app_commands::request(&session.context(&host.tags), url, body),
        "/api/provenance" => session.provenance.post(body),
        "/api/onboarding" => {
            let library = session.library.clone();
            let works = body.get("works").is_some_and(truthy);
            session.onboarding.post(&library, body).and_then(|()| {
                crate::app_onboarding::Onboarding::read(session, &host.root, &session.context(&host.tags), works)
            })
        }
        route if route.starts_with("/api/accounts/") => {
            crate::app_accounts::action(&session.library, &route["/api/accounts/".len()..], body)
        }
        "/api/index" => {
            let library = session.library.clone();
            let busy = session.syncer.running();
            if host.imports.recognition_running(session.generation)
                && matches!(body.get("action").map(Json::text), Some("apply" | "drop"))
            {
                Err(Error::Invalid("识别结果正在导入，完成后再更新角色索引".into()))
            } else {
                session.index.post(&library, body, busy)
            }
        }
        "/api/names" => {
            let library = session.library.clone();
            session.names.post(&library, body)
        }
        "/api/sync" => sync(host, received, session),
        "/api/similar" | "/api/regroup" => research(url, session, body),
        // Read-only, so its names follow the page's language like other reads (app_http.rs).
        "/api/suggest" => crate::http_read::PageLanguage::set(language)
            .with(|| crate::http_local::suggest(&session.context(&host.tags), body)),
        "/api/startup" => crate::app_settings::set_startup_json(&session.library, text)
            .and_then(|_| host.library_settings(&session.library)),
        // App-wide, so not part of /api/settings, whose reply stays as recorded; pages read it from /i18n.js.
        "/api/preferences" => {
            let locales = crate::app_preferences::locales(&crate::app_preferences::locales_folder(&host.root));
            crate::app_preferences::set_json(text, &locales)
                .map(|_| Json::object([("preferences", crate::app_preferences::info(&locales))]))
        }
        "/api/backup" => crate::app_settings::backup(&session.library)
            .map(|path| Json::object([("path", path.to_string_lossy().as_ref().into())])),
        "/api/libraries/new" => create_library(host, body),
        "/api/libraries/sources/add" | "/api/libraries/sources/remove" => {
            sources(url, received, session, body).map_err(management_error)
        }
        "/api/libraries/preset" => crate::app_settings::preset(
            &mut session.library,
            body.get("source").map(Json::text).unwrap_or(""),
            body.get("preset").map(Json::text).unwrap_or(""),
        )
        .map_err(management_error),
        "/api/libraries/forget" => forget(session, body),
        "/api/libraries/export" => host.exports.start(&session.library, session.generation),
        "/api/integrity" => Ok(session.check_now()),
        "/api/integrity/rescue" => crate::app_create::optional_reference(&host.reference)
            .and_then(|reference| session.start_rescue(reference.map(Path::to_owned))),
        "/api/libraries/open" => open(host, received, session, body),
        _ => return None,
    })
}
fn sync(host: &Host, received: u64, session: &mut Session) -> Result<Json> {
    if session.index.writing() {
        return Err(Error::Invalid("角色索引正在更新，请稍后".into()));
    }
    if host.imports.recognition_running(session.generation) {
        return Err(Error::Invalid("识别结果正在导入，请稍后".into()));
    }
    let library = session.library.clone();
    let token = session.token.clone();
    session.syncer.start(&library, received, &token).map(|started| Json::object([("started", started.into())]))
}
/// The similar scan (/api/similar) or regrouping (/api/regroup): started, or cancelled.
fn research(url: &str, session: &mut Session, body: &Json) -> Result<Json> {
    if url == "/api/similar" && !session.research.similar_enabled {
        return Err(Error::Invalid("相似扫描已关闭".into()));
    }
    let lib = session.library.clone();
    if body.get("action").map(Json::text) == Some("cancel") {
        session.research.cancel();
        return Ok(Json::object([("cancelled", true.into())]));
    }
    let kind = if url == "/api/similar" { "similar" } else { "regroup" };
    session.research.start(&lib, kind, serde_json::json!({}), None).map(|_| Json::object([("started", true.into())]))
}
fn sources(url: &str, received: u64, session: &mut Session, body: &Json) -> Result<Json> {
    // A running sync holds a pinned copy of the sources; change them only between syncs.
    if session.syncer.running() || session.index.writing() {
        return Err(Error::Invalid("正在同步或更新角色索引，请稍后再修改来源".into()));
    }
    let mut result = if url == "/api/libraries/sources/add" {
        add_sources(received, session, body)?
    } else {
        let confirm = body.get("confirm").is_some_and(truthy);
        let result = crate::app_sources::remove(
            &mut session.library,
            body.get("source").map(Json::text).unwrap_or(""),
            confirm,
        )?;
        if !confirm {
            return Ok(result);
        }
        result
    };
    // Scan a new source now; a removed source's pictures leave the browsing database with this sync.
    let (library, token) = (session.library.clone(), session.token.clone());
    result.set("sync", session.syncer.start(&library, received, &token).unwrap_or(false).into());
    Ok(result)
}
/// Several folders picked at once share one origin and one sync: `paths`, or a single `path`.
fn add_sources(received: u64, session: &mut Session, body: &Json) -> Result<Json> {
    let raw: Vec<&str> = match body.get("paths") {
        Some(Json::Array(items)) => items.iter().map(Json::text).collect(),
        _ => vec![body.get("path").map(Json::text).unwrap_or("")],
    };
    let (kind, preset) =
        (body.get("kind").map(Json::text).unwrap_or(""), body.get("preset").map(Json::text).unwrap_or(""));
    let mut added = Vec::new();
    for path in raw {
        let path = if path.is_empty() { PathBuf::new() } else { crate::app_paths::resolve(&expand_path(path)?)? };
        let path = path.to_str().ok_or_else(|| Error::Invalid("来源路径不是 UTF-8".into()))?;
        match crate::app_sources::add(&mut session.library, kind, path, preset) {
            Ok(source) => added.push(source),
            Err(error) => {
                // Folders added before the refused one stay added and are scanned now.
                if !added.is_empty() {
                    let (library, token) = (session.library.clone(), session.token.clone());
                    let _ = session.syncer.start(&library, received, &token);
                }
                return Err(error);
            }
        }
    }
    Ok(if added.len() == 1 {
        added.pop().expect("one added source")
    } else {
        Json::object([("added", Json::Array(added))])
    })
}
fn forget(session: &Session, body: &Json) -> Result<Json> {
    let raw =
        body.get("path").map(Json::text).filter(|s| !s.is_empty()).ok_or_else(|| Error::Invalid("无效路径".into()))?;
    if Path::new(raw) == session.library.display_path() {
        return Err(Error::Invalid("不能移出当前打开的图库".into()));
    }
    crate::app_libraries::forget(&expand_path(raw)?).map_err(management_error)?;
    Ok(Json::object([("ok", true.into())]))
}
fn open(host: &Host, received: u64, session: &mut Session, body: &Json) -> Result<Json> {
    let path =
        body.get("path")
            .and_then(|value| {
                if let Json::Scalar(serde_json::Value::String(path)) = value {
                    Some(path.as_str())
                } else {
                    None
                }
            })
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::Invalid("不是图库包".into()))?;
    let path = expand_path(path)?;
    if !path.join("library.json").is_file() {
        return Err(Error::Invalid("不是图库包".into()));
    }
    // The session's own package with its rescued source.sqlite ready (#228): put in place as it opens again.
    let own = crate::app_paths::canonical(&path).is_ok_and(|p| p == session.library.instance_path());
    if own && session.rescue.ready() {
        return host.reopen_rescued(session, received);
    }
    // Another package whose source.sqlite a check found damaged: rescued first when the person agreed to.
    if !own && body.get("rescue").is_some_and(truthy) {
        let reference = crate::app_create::optional_reference(&host.reference)?;
        let report = crate::app_service::rescue_package(&path, None, reference, &|_, _| {})?.json();
        let mut out = host.switch_locked(session, received, &path)?;
        session.rescued = Some(report.clone());
        out.set("rescued", report);
        return Ok(out);
    }
    // A package in an older format opens only after the person agrees to upgrade it (#193); it is looked at
    // only once opening failed, so libraries in the current format pay nothing for this.
    match host.switch_locked(session, received, &path) {
        Err(Error::Schema(why)) => match crate::app_upgrade::needed(&path) {
            None => Err(crate::app_upgrade::pre_v8(&path).map_or(Error::Schema(why), Error::Invalid)),
            Some(_) if !body.get("upgrade").is_some_and(truthy) => {
                Err(Error::Conflict(crate::app_upgrade::NEEDED.into()))
            }
            Some(prepared) => {
                // Said as it happened: not committed (the package is as it was) or committed, with the backup.
                let report =
                    crate::app_upgrade::run(&prepared, None).map_err(|failure| Error::Invalid(failure.to_string()))?;
                let backup = crate::app_upgrade::shown(&report.backup);
                drop(report);
                let mut out = host.switch_locked(session, received, &path)?;
                session.upgraded = Some(backup.clone());
                out.set("upgraded", Json::object([("backup", backup.as_str().into())]));
                Ok(out)
            }
        },
        other => other,
    }
}

//! 会话 POST 业务路由；入口安全检查由 app_http 保持。
use super::*;

pub(super) fn handle(request: Request, host: &Host, received: u64, text: &str, body: Json) {
    if request.url() == "/api/libraries/choose" {
        let result = crate::app_platform::choose(
            body.get("kind").map(Json::text).unwrap_or(""),
            body.get("multiple").is_some_and(truthy),
            &crate::platform_process::stopping,
        )
        .map_err(management_error);
        if !host.current(received) {
            respond(request, error("会话已过期，请刷新页面"), 403, "core");
            return;
        }
        match result {
            Ok(value) => respond(request, value, 200, "core"),
            Err(e) => failure(request, e, true),
        }
        return;
    }
    // The native archive picker runs without holding the session; cancellation or
    // a switch must never deliver the chosen file into a replacement package.
    let mut body = body;
    if request.url() == "/api/accounts/x/import" && !body.get("path").is_some_and(truthy) {
        match crate::app_platform::choose("archive", false, &crate::platform_process::stopping)
            .map_err(management_error)
        {
            Ok(picked) => {
                if !host.current(received) {
                    respond(request, error("会话已过期，请刷新页面"), 403, "core");
                    return;
                }
                if !picked.get("path").is_some_and(truthy) {
                    respond(request, Json::object([("cancelled", true.into())]), 200, "core");
                    return;
                }
                body.set("path", picked.get("path").expect("http_host_post internal invariant").clone());
            }
            Err(e) => {
                failure(request, e, true);
                return;
            }
        }
    }
    let mut session = host.session.write().expect("http_host_post session lock poisoned");
    if !host.accepts(&session, received) || header(&request, "X-CSRF-Token") != Some(session.token.as_str()) {
        respond(request, error("会话已过期，请刷新页面"), 403, "core");
        return;
    }
    if request.url() == "/api/cache" {
        if !matches!(body.get("action"), Some(Json::Scalar(serde_json::Value::String(action))) if action == "clear") {
            drop(session);
            failure(request, Error::Invalid("未知操作".into()), true);
            return;
        }
        // Bind after framing and session validation; waiting for image work must
        // not hold the session lock or retarget this operation after a switch.
        let library = session.library.clone();
        drop(session);
        match host.images.clear(
            &library,
            &forced_off(
                &library,
                host.names_enabled,
                host.sync_disabled,
                host.provenance_enabled,
                host.index_automatic,
            ),
        ) {
            Ok(payload) => respond(request, payload, 200, "core"),
            Err(e) => failure(request, e, true),
        }
        return;
    }
    if ["/api/entities", "/api/character/profile", "/api/local-links"].contains(&request.url()) {
        let library = session.library.clone();
        let mut context = session.context(&host.tags);
        context.db = library.instance_path().join("library.sqlite");
        context.source = Some(library.instance_path().join("source.sqlite"));
        let source = session.names.source(&library);
        drop(session);
        let prepared = source.and_then(|mut source| {
            let c = crate::app_library::read_connection(&context.db)?;
            crate::app_profiles::prepare(&c, &body, request.url(), &mut source)
        });
        let session = host.session.write().expect("http_host_post session lock poisoned");
        if !host.accepts(&session, received) {
            respond(request, error("会话已过期，请刷新页面"), 403, "core");
            return;
        }
        let result =
            prepared.and_then(|p| crate::app_commands::request_prepared(&context, request.url(), &body, Some(&p)));
        drop(session);
        match result {
            Ok(v) => respond(request, v, 200, "core"),
            Err(e) => failure(request, e, true),
        }
        return;
    }
    if request.url() == "/api/names" && body.get("action").map(Json::text) == Some("model_api_test") {
        let result = session.names.source(&session.library);
        drop(session);
        let result = result.and_then(|source| crate::app_names::test(&source));
        if !host.current(received) {
            respond(request, error("会话已过期，请刷新页面"), 403, "core");
            return;
        }
        match result {
            Ok(v) => respond(request, v, 200, "core"),
            Err(e) => failure(request, e, true),
        }
        return;
    }
    if request.url() == "/api/relate" {
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
            failure(request, Error::Invalid("请选择 2–12 张图".into()), true);
            return;
        }
        let lib = session.library.clone();
        let result = session.research.query(&lib, "relate", value);
        drop(session);
        let result = result.and_then(|rx| rx.recv().map_err(|_| Error::Invalid("研究任务已结束".into()))?);
        if !host.current(received) {
            respond(request, error("会话已过期，请刷新页面"), 403, "core");
            return;
        }
        match result {
            Ok(v) => respond(request, v, 200, "core"),
            Err(e) => failure(request, e, true),
        }
        return;
    }
    // Imports are background tasks (#26, #88): the request validates, starts the task and answers at once; the page
    // polls GET /api/imports and may cancel. A recognition import stays bound to the library it started on. Cancelling,
    // switching libraries or quitting stops either import before its publication point.
    if request.url() == "/api/recognition/import" {
        let resume = matches!(body.get("resume"), Some(Json::Scalar(serde_json::Value::Bool(true))));
        let path = match body.get("path") {
            Some(Json::Scalar(serde_json::Value::String(path))) => Some(PathBuf::from(path)),
            _ => None,
        };
        let started = (|| {
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
        })();
        drop(session);
        match started {
            Ok(task) => respond(request, Json::object([("started", true.into()), ("task", task)]), 200, "core"),
            Err(e) => failure(request, e, true),
        }
        return;
    }
    if request.url() == "/api/libraries/import" {
        let started = (|| {
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
        })();
        drop(session);
        match started {
            Ok(task) => respond(request, Json::object([("started", true.into()), ("task", task)]), 200, "core"),
            Err(e) => failure(request, e, true),
        }
        return;
    }
    if request.url() == "/api/imports/cancel" {
        drop(session);
        respond(request, host.imports.cancel(crate::app_imports::CANCELLED), 200, "core");
        return;
    }
    let result = match request.url() {
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
        | "/api/merge" => crate::app_commands::request(&session.context(&host.tags), request.url(), &body),
        "/api/provenance" => session.provenance.post(&body),
        "/api/onboarding" => {
            let library = session.library.clone();
            let works = body.get("works").is_some_and(truthy);
            session.onboarding.post(&library, &body).and_then(|()| {
                crate::app_onboarding::Onboarding::read(&session, &host.root, &session.context(&host.tags), works)
            })
        }
        route if route.starts_with("/api/accounts/") => {
            crate::app_accounts::action(&session.library, &route["/api/accounts/".len()..], &body)
        }
        "/api/index" => {
            let library = session.library.clone();
            let busy = session.syncer.running();
            if host.imports.recognition_running(session.generation)
                && matches!(body.get("action").map(Json::text), Some("apply" | "drop"))
            {
                Err(Error::Invalid("识别结果正在导入，完成后再更新角色索引".into()))
            } else {
                session.index.post(&library, &body, busy)
            }
        }
        "/api/names" => {
            let library = session.library.clone();
            session.names.post(&library, &body)
        }
        "/api/sync" => {
            if session.index.writing() {
                drop(session);
                failure(request, Error::Invalid("角色索引正在更新，请稍后".into()), true);
                return;
            }
            if host.imports.recognition_running(session.generation) {
                drop(session);
                failure(request, Error::Invalid("识别结果正在导入，请稍后".into()), true);
                return;
            }
            let library = session.library.clone();
            let token = session.token.clone();
            session.syncer.start(&library, received, &token).map(|started| Json::object([("started", started.into())]))
        }
        "/api/similar" | "/api/regroup" => {
            if request.url() == "/api/similar" && !session.research.similar_enabled {
                drop(session);
                failure(request, Error::Invalid("相似扫描已关闭".into()), true);
                return;
            }
            let lib = session.library.clone();
            if body.get("action").map(Json::text) == Some("cancel") {
                session.research.cancel();
                Ok(Json::object([("cancelled", true.into())]))
            } else {
                session
                    .research
                    .start(
                        &lib,
                        if request.url() == "/api/similar" { "similar" } else { "regroup" },
                        serde_json::json!({}),
                        None,
                    )
                    .map(|_| Json::object([("started", true.into())]))
            }
        }
        "/api/suggest" => crate::http_local::suggest(&session.context(&host.tags), &body),
        "/api/startup" => crate::app_settings::set_startup_json(&session.library, text).and_then(|_| {
            settings(
                &session.library,
                host.names_enabled,
                host.sync_disabled,
                host.provenance_enabled,
                host.index_automatic,
            )
        }),
        // App-wide, so not part of /api/settings, whose reply stays as recorded; pages read it from /i18n.js.
        "/api/preferences" => crate::app_preferences::set_json(text)
            .map(|_| Json::object([("preferences", crate::app_preferences::info())])),
        "/api/backup" => crate::app_settings::backup(&session.library)
            .map(|path| Json::object([("path", path.to_string_lossy().as_ref().into())])),
        "/api/libraries/new" => create_library(host, &body),
        "/api/libraries/sources/add" | "/api/libraries/sources/remove" => (|| {
            // A running sync holds a pinned copy of the sources; change them only between syncs.
            if session.syncer.running() || session.index.writing() {
                return Err(Error::Invalid("正在同步或更新角色索引，请稍后再修改来源".into()));
            }
            let mut result = if request.url() == "/api/libraries/sources/add" {
                // Several folders picked at once share one origin and one sync: `paths`, or a single `path`.
                let raw: Vec<&str> = match body.get("paths") {
                    Some(Json::Array(items)) => items.iter().map(Json::text).collect(),
                    _ => vec![body.get("path").map(Json::text).unwrap_or("")],
                };
                let (kind, preset) =
                    (body.get("kind").map(Json::text).unwrap_or(""), body.get("preset").map(Json::text).unwrap_or(""));
                let mut added = Vec::new();
                for path in raw {
                    let path =
                        if path.is_empty() { PathBuf::new() } else { crate::app_paths::resolve(&expand_path(path)?)? };
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
                if added.len() == 1 {
                    added.pop().expect("one added source")
                } else {
                    Json::object([("added", Json::Array(added))])
                }
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
        })()
        .map_err(management_error),
        "/api/libraries/preset" => crate::app_settings::preset(
            &mut session.library,
            body.get("source").map(Json::text).unwrap_or(""),
            body.get("preset").map(Json::text).unwrap_or(""),
        )
        .map_err(management_error),
        "/api/libraries/forget" => (|| {
            let raw = body
                .get("path")
                .map(Json::text)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Error::Invalid("无效路径".into()))?;
            if Path::new(raw) == session.library.display_path() {
                return Err(Error::Invalid("不能移出当前打开的图库".into()));
            }
            crate::app_libraries::forget(&expand_path(raw)?).map_err(management_error)?;
            Ok(Json::object([("ok", true.into())]))
        })(),
        "/api/libraries/export" => host.exports.start(&session.library, session.generation),
        "/api/libraries/open" => (|| {
            let path = body
                .get("path")
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
            host.switch_locked(&mut session, received, &path)
        })(),
        _ => {
            unavailable(request);
            return;
        }
    };
    drop(session);
    match result {
        Ok(payload) => respond(request, payload, 200, "core"),
        Err(e) => failure(request, e, true),
    }
}

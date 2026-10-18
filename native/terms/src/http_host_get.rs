//! 会话 GET 业务路由；入口安全检查由 app_http 保持。
use super::*;

pub(super) fn handle(request: Request, host: &Host, received: u64) {
    let session = host.session.read().expect("http_host_get session lock poisoned");
    if !host.accepts(&session, received) {
        respond(request, error("会话已过期，请刷新页面"), 403, "core");
        return;
    }
    let path = request.url().split('?').next().unwrap_or(request.url());
    if crate::app_files::serves(path) {
        // Resolve the old session's asset under the lock; decoding and socket writes must not
        // delay switching. The resulting request never reopens the replacement library.
        let result = crate::app_files::prepare(&session.library, &host.root, request.url());
        drop(session);
        match result.and_then(|file| host.images.load(file)) {
            Ok(file) => {
                let _ = request.respond(crate::app_files::response(file));
            }
            Err(crate::app_files::Failure::Status(status)) => {
                respond(request, error("图片不存在或原图暂不可读"), status, "core")
            }
            Err(crate::app_files::Failure::Core(e)) => failure(request, e, false),
        }
        return;
    }
    if crate::http_read::remote(request.url()) || path == "/api/local-links" {
        let mut source = session.names.source(&session.library);
        drop(session);
        let result = source
            .as_mut()
            .map_err(|_| Error::Invalid("名称服务配置不正确".into()))
            .and_then(|source| crate::app_names::remote(source, request.url()));
        if !host.current(received) {
            respond(request, error("会话已过期，请刷新页面"), 403, "core");
            return;
        }
        match result {
            Ok(v) => respond(request, v, 200, "core"),
            Err(e) => failure(request, e, false),
        }
        return;
    }
    if path == "/api/oc/groups" {
        drop(session);
        let mut session = host.session.write().expect("http_host_get session lock poisoned");
        if !host.accepts(&session, received) {
            respond(request, error("会话已过期，请刷新页面"), 403, "core");
            return;
        }
        let q = crate::http_read::query(request.url());
        let entity = q.get("entity").map(String::as_str).unwrap_or("");
        let c = crate::app_library::read_connection(&session.library.db());
        let entity = c.and_then(|c| crate::character_commands::resolve(&c, entity));
        let lib = session.library.clone();
        let result = entity.and_then(|entity| session.research.query(&lib, "oc", serde_json::json!({"entity":entity})));
        drop(session);
        let result = result.and_then(|rx| rx.recv().map_err(|_| Error::Invalid("研究任务已结束".into()))?);
        if !host.current(received) {
            respond(request, error("会话已过期，请刷新页面"), 403, "core");
            return;
        }
        match result {
            Ok(v) => respond(request, v, 200, "core"),
            Err(e) => failure(request, e, false),
        }
        return;
    }
    let result = match path {
        "/api/provenance" => session.provenance.info().map(|v| Some((v, 200))),
        "/api/index" => {
            if crate::http_read::query(request.url()).get("progress").is_some_and(|v| v == "1") {
                Ok(Some((session.index.progress(), 200)))
            } else {
                session.index.info(&session.library).map(|v| Some((v, 200)))
            }
        }
        "/api/similar" | "/api/clusters" => {
            crate::http_read::read(&session.context(&host.tags), request.url()).map(|value| {
                value.map(|(mut value, status)| {
                    value.set(
                        if path == "/api/similar" { "state" } else { "learner" },
                        if path == "/api/similar" {
                            session.research.similar_state()
                        } else {
                            session.research.learner_state()
                        },
                    );
                    (value, status)
                })
            })
        }
        "/api/names" => {
            if crate::http_read::query(request.url()).get("progress").is_some_and(|s| s == "1") {
                Ok(Some((session.names.progress(), 200)))
            } else {
                session.names.status(&session.library).map(|v| Some((v, 200)))
            }
        }
        "/api/sync" => session.syncer.status(&session.library).map(|p| Some((p, 200))),
        "/api/integrity" => Ok(Some((session.integrity(), 200))),
        // App-wide (#191): the update check's choice and last result, not the library's.
        "/api/updates" => crate::app_updates::info().map(|p| Some((p, 200))),
        "/api/onboarding" => {
            let works = crate::http_read::query(request.url()).get("works").is_some_and(|v| v == "1");
            crate::app_onboarding::Onboarding::read(&session, &host.root, &session.context(&host.tags), works)
                .map(|p| Some((p, 200)))
        }
        // The import slot belongs to the application: after a switch the page still reads why the last one stopped.
        "/api/imports" => Ok(Some((host.imports.state(), 200))),
        path if crate::app_local::ROUTES.contains(&path) => {
            crate::app_local::read(&session.context(&host.tags), request.url()).map(|p| Some((p, 200)))
        }
        "/api/settings" => settings(
            &session.library,
            host.names_enabled,
            host.sync_disabled,
            host.provenance_enabled,
            host.index_automatic,
        )
        .map(|mut p| {
            p.set("tasks", Json::object([("similar", session.research.similar_state())]));
            Some((p, 200))
        }),
        "/api/libraries" => crate::app_libraries::info(&session.library).map(|mut p| {
            p.set("export", host.exports.state(&session.library, session.generation));
            // Only when listing it failed (#176): the page says so once; the usual answer is unchanged.
            if let Some(why) = &session.unlisted {
                p.set("unlisted", why.as_str().into());
            }
            // Only after an upgrade on opening (#193): where the pre-upgrade backup is.
            if let Some(backup) = &session.upgraded {
                p.set("upgraded", Json::object([("backup", backup.as_str().into())]));
            }
            Some((p, 200))
        }),
        _ if !crate::http_read::remote(request.url()) => {
            crate::http_read::read(&session.context(&host.tags), request.url())
        }
        _ => Ok(None),
    };
    // The request owns its old snapshot; socket backpressure cannot block switching or DB commands.
    // Never reopen against the replacement session to deliver this response.
    drop(session);
    match result {
        Ok(Some((payload, status))) => respond(request, payload, status, "core"),
        Ok(None) => unavailable(request),
        Err(e) => failure(request, e, false),
    }
}

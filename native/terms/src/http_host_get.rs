//! 会话 GET 业务路由；入口安全检查由 app_http 保持。GET 与 POST 共用的应答小工具也在这里。
use super::*;
use std::sync::{mpsc::Receiver, RwLockReadGuard};

/// The session this request arrived in is gone (or never was), so the page must reload.
pub(super) fn expired(request: Request) {
    respond(request, error("会话已过期，请刷新页面"), 403, "core");
}
/// Answers with `result`; `write` says whether a failure is reported as a failed save.
pub(super) fn reply(request: Request, result: Result<Json>, write: bool) {
    match result {
        Ok(v) => respond(request, v, 200, "core"),
        Err(e) => failure(request, e, write),
    }
}
/// Answers with what was worked out without the session lock, unless the session switched meanwhile.
pub(super) fn reply_if_current(request: Request, host: &Host, received: u64, result: Result<Json>, write: bool) {
    if !host.current(received) {
        expired(request);
        return;
    }
    reply(request, result, write)
}
/// A research query's answer, once the task sends it.
pub(super) fn waited(query: Result<Receiver<Result<Json>>>) -> Result<Json> {
    query.and_then(|rx| rx.recv().map_err(|_| Error::Invalid("研究任务已结束".into()))?)
}
impl Host {
    /// The library's settings, with what this host forces off.
    pub(super) fn library_settings(&self, library: &Library) -> Result<Json> {
        settings(library, self.names_enabled, self.sync_disabled, self.provenance_enabled, self.index_automatic)
    }
    /// The tasks this host forces off for the library.
    pub(super) fn library_forced_off(&self, library: &Library) -> Vec<String> {
        forced_off(library, self.names_enabled, self.sync_disabled, self.provenance_enabled, self.index_automatic)
    }
}

pub(super) fn handle(request: Request, host: &Host, received: u64) {
    let session = host.session.read().expect("http_host_get session lock poisoned");
    if !host.accepts(&session, received) {
        expired(request);
        return;
    }
    let path = request.url().split('?').next().unwrap_or(request.url());
    if crate::app_files::serves(path) {
        file(request, host, session);
        return;
    }
    if crate::http_read::remote(request.url()) || path == "/api/local-links" {
        remote_names(request, host, received, session);
        return;
    }
    if path == "/api/oc/groups" {
        drop(session);
        oc_groups(request, host, received);
        return;
    }
    let result = read(&request, host, &session, path);
    // The request owns its old snapshot; socket backpressure cannot block switching or DB commands.
    // Never reopen against the replacement session to deliver this response.
    drop(session);
    match result {
        Ok(Some((payload, status))) => respond(request, payload, status, "core"),
        Ok(None) => unavailable(request),
        Err(e) => failure(request, e, false),
    }
}
/// Resolve the old session's asset under the lock; decoding and socket writes must not delay switching. The
/// resulting request never reopens the replacement library.
fn file(request: Request, host: &Host, session: RwLockReadGuard<Session>) {
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
}
/// Names looked up on the network, without holding the session.
fn remote_names(request: Request, host: &Host, received: u64, session: RwLockReadGuard<Session>) {
    let mut source = session.names.source(&session.library);
    drop(session);
    let result = source
        .as_mut()
        .map_err(|_| Error::Invalid("名称服务配置不正确".into()))
        .and_then(|source| crate::app_names::remote(source, request.url()));
    reply_if_current(request, host, received, result, false);
}
fn oc_groups(request: Request, host: &Host, received: u64) {
    let mut session = host.session.write().expect("http_host_get session lock poisoned");
    if !host.accepts(&session, received) {
        expired(request);
        return;
    }
    let q = crate::http_read::query(request.url());
    let entity = q.get("entity").map(String::as_str).unwrap_or("");
    let c = crate::app_library::read_connection(&session.library.db());
    let entity = c.and_then(|c| crate::character_commands::resolve(&c, entity));
    let lib = session.library.clone();
    let result = entity.and_then(|entity| session.research.query(&lib, "oc", serde_json::json!({"entity":entity})));
    drop(session);
    reply_if_current(request, host, received, waited(result), false);
}
/// Whether the URL asks for progress (`progress=1`) rather than the status.
fn progress(request: &Request) -> bool {
    crate::http_read::query(request.url()).get("progress").is_some_and(|v| v == "1")
}
fn read(request: &Request, host: &Host, session: &Session, path: &str) -> Result<Option<(Json, u16)>> {
    match path {
        "/api/provenance" => session.provenance.info().map(|v| Some((v, 200))),
        "/api/index" => {
            if progress(request) {
                Ok(Some((session.index.progress(), 200)))
            } else {
                session.index.info(&session.library).map(|v| Some((v, 200)))
            }
        }
        "/api/similar" | "/api/clusters" => {
            crate::http_read::read(&session.context(&host.tags), request.url()).map(|value| {
                value.map(|(mut value, status)| {
                    if path == "/api/similar" {
                        value.set("state", session.research.similar_state());
                    } else {
                        value.set("learner", session.research.learner_state());
                    }
                    (value, status)
                })
            })
        }
        "/api/names" => {
            if progress(request) {
                Ok(Some((session.names.progress(), 200)))
            } else {
                session.names.status(&session.library).map(|v| Some((v, 200)))
            }
        }
        "/api/sync" => session.syncer.status(&session.library).map(|p| Some((p, 200))),
        "/api/integrity" => Ok(Some((session.integrity(), 200))),
        // App-wide (#191): the update check's choice and last result, not the library's.
        "/api/updates" => crate::app_updates::info().map(|p| Some((p, 200))),
        // App-wide (#168): whether the recognition models are in place, and the download or import in progress.
        "/api/models" => crate::app_models::info(&host.root).map(|p| Some((p, 200))),
        "/api/onboarding" => {
            let works = crate::http_read::query(request.url()).get("works").is_some_and(|v| v == "1");
            crate::app_onboarding::Onboarding::read(session, &host.root, &session.context(&host.tags), works)
                .map(|p| Some((p, 200)))
        }
        // The import slot belongs to the application: after a switch the page still reads why the last one stopped.
        "/api/imports" => Ok(Some((host.imports.state(), 200))),
        path if crate::app_local::ROUTES.contains(&path) => {
            crate::app_local::read(&session.context(&host.tags), request.url()).map(|p| Some((p, 200)))
        }
        "/api/settings" => host.library_settings(&session.library).map(|mut p| {
            p.set("tasks", Json::object([("similar", session.research.similar_state())]));
            Some((p, 200))
        }),
        "/api/libraries" => libraries(host, session).map(|p| Some((p, 200))),
        _ if !crate::http_read::remote(request.url()) => {
            crate::http_read::read(&session.context(&host.tags), request.url())
        }
        _ => Ok(None),
    }
}
fn libraries(host: &Host, session: &Session) -> Result<Json> {
    let mut p = crate::app_libraries::info(&session.library)?;
    p.set("export", host.exports.state(&session.library, session.generation));
    // Only when listing it failed (#176): the page says so once; the usual answer is unchanged.
    if let Some(why) = &session.unlisted {
        p.set("unlisted", why.as_str().into());
    }
    // Only after an upgrade on opening (#193): where the pre-upgrade backup is.
    if let Some(backup) = &session.upgraded {
        p.set("upgraded", Json::object([("backup", backup.as_str().into())]));
    }
    // Only after a rescue of source.sqlite on opening (#228): what it did and where the damaged file went.
    if let Some(report) = &session.rescued {
        p.set("rescued", report.clone());
    }
    Ok(p)
}

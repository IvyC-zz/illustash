//! 会话 GET 业务路由；入口安全检查由 app_http 保持。GET 与 POST 共用的应答小工具也在这里。
use super::*;
use std::sync::{mpsc::Receiver, RwLockReadGuard};

/// 这个请求所属的会话已经不在（或从未存在），页面必须重新加载。
pub(super) fn expired(request: Request) {
    respond(request, error("会话已过期，请刷新页面"), 403, "core");
}
/// 用 `result` 应答；`write` 决定失败是否按保存失败报告。
pub(super) fn reply(request: Request, result: Result<Json>, write: bool) {
    match result {
        Ok(v) => respond(request, v, 200, "core"),
        Err(e) => failure(request, e, write),
    }
}
/// 用不持会话锁算出的结果应答，除非这期间会话已经切换。
pub(super) fn reply_if_current(request: Request, host: &Host, received: u64, result: Result<Json>, write: bool) {
    if !host.current(received) {
        expired(request);
        return;
    }
    reply(request, result, write)
}
/// 研究查询的答复，等任务发来后返回。
pub(super) fn waited(query: Result<Receiver<Result<Json>>>) -> Result<Json> {
    query.and_then(|rx| rx.recv().map_err(|_| Error::Invalid("研究任务已结束".into()))?)
}
impl Host {
    /// 图库的设置，连同本宿主强制关闭的项目。
    pub(super) fn library_settings(&self, library: &Library) -> Result<Json> {
        settings(library, self.names_enabled, self.sync_disabled, self.provenance_enabled, self.index_automatic)
    }
    /// 本宿主为该图库强制关闭的任务。
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
    if path == "/api/entity-preview" {
        preview(request, host, received, session);
        return;
    }
    if path == "/api/oc/groups" {
        drop(session);
        oc_groups(request, host, received);
        return;
    }
    let result = read(&request, host, &session, path);
    // 请求持有自己的旧快照；套接字的背压不会阻塞切换或数据库命令。
    // 绝不为了发出这个响应而针对替换后的会话重新打开。
    drop(session);
    match result {
        Ok(Some((payload, status))) => respond(request, payload, status, "core"),
        Ok(None) => unavailable(request),
        Err(e) => failure(request, e, false),
    }
}
/// 在锁内解析旧会话的资产；解码和套接字写入不能拖慢切换。之后的请求处理从不重新打开替换后的图库。
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
/// 联网查询的名字，查询时不持有会话。
fn remote_names(request: Request, host: &Host, received: u64, session: RwLockReadGuard<Session>) {
    let mut source = session.names.source(&session.library);
    drop(session);
    let result = source
        .as_mut()
        .map_err(|_| Error::Invalid("名称服务配置不正确".into()))
        .and_then(|source| crate::app_names::remote(source, request.url()));
    reply_if_current(request, host, received, result, false);
}
/// Danbooru 角色的预览图（#293），查询和下载时不持有会话。Danbooru 上没有该角色的全年龄帖子时返回 404，
/// 页面随后保留占位图。
fn preview(request: Request, host: &Host, received: u64, session: RwLockReadGuard<Session>) {
    let mut source = session.names.source(&session.library);
    let thumbnails = session.library.thumbnails();
    drop(session);
    let tag = crate::http_read::query(request.url()).remove("tag").unwrap_or_default();
    let result = source
        .as_mut()
        .map_err(|_| Error::Invalid("名称服务配置不正确".into()))
        .and_then(|source| crate::app_name_sources::preview(source, &thumbnails, &tag));
    if !host.current(received) {
        expired(request);
        return;
    }
    match result {
        Ok(Some(bytes)) => {
            let _ = request.respond(crate::app_files::response(crate::app_files::File::jpeg(bytes)));
        }
        Ok(None) => respond(request, error("Danbooru 上没有这个角色的全年龄图"), 404, "core"),
        Err(e) => failure(request, e, false),
    }
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
/// URL 要的是进度（`progress=1`）而不是状态。
fn progress(request: &Request) -> bool {
    crate::http_read::query(request.url()).get("progress").is_some_and(|v| v == "1")
}
fn read(request: &Request, host: &Host, session: &Session, path: &str) -> Result<Option<(Json, u16)>> {
    match path {
        "/api/diagnostics" => Ok(Some((crate::diagnostics::report().into(), 200))),
        "/api/provenance" => session.provenance.info().map(|v| Some((v, 200))),
        // 上次检查时每个关注作品的覆盖情况（#561）；/api/index 的回复冻结，另开这个路由。
        "/api/index/coverage" => Ok(Some((session.index.coverage(), 200))),
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
        // 全应用范围（#191）：检查更新的选项和上次结果，不属于图库。
        "/api/updates" => crate::app_updates::info().map(|p| Some((p, 200))),
        // 全应用范围（#168）：识别模型是否就位，以及正在进行的下载或导入。
        "/api/models" => crate::app_models::info(&host.root).map(|p| Some((p, 200))),
        "/api/onboarding" => {
            let works = crate::http_read::query(request.url()).get("works").is_some_and(|v| v == "1");
            crate::app_onboarding::Onboarding::read(session, &host.root, &session.context(&host.tags), works)
                .map(|p| Some((p, 200)))
        }
        // 用图动作（#450）：能不能拖出（只在桌面 App 里），以及最近一次导出到文件夹的状态。
        "/api/files" => Ok(Some((
            Json::object([
                ("drag", crate::app_desktop::available().into()),
                ("export", session.asset_exports.state(session.generation)),
            ]),
            200,
        ))),
        // 导入槽位属于应用：切换图库后，页面仍能读到上一次导入为什么停下。
        "/api/imports" => Ok(Some((host.imports.state(), 200))),
        "/api/human-conclusions" => Ok(Some((
            Json::object([("automatic", session.human_backups.status()), ("export", session.human_exports.state())]),
            200,
        ))),
        "/api/human-conclusions/report" => {
            let q = crate::http_read::query(request.url());
            let id = q.get("id").filter(|s| !s.is_empty()).ok_or_else(|| Error::invalid("需要报告编号"))?;
            let parse = |key: &str, default: usize| -> Result<usize> {
                q.get(key)
                    .map(|v| v.parse::<usize>().map_err(|_| Error::invalid("分页参数不正确")))
                    .unwrap_or(Ok(default))
            };
            let (offset, limit) = (parse("offset", 0)?, parse("limit", 100)?);
            if limit == 0 || limit > 500 || offset > illustash_core::human_conclusions::format::MAX_RECORDS {
                return Err(Error::invalid("每页须为 1–500 条，偏移不能超过格式记录上限"));
            }
            Ok(Some((session.human_exports.page(&session.library.db(), id, offset, limit)?, 200)))
        }
        path if crate::app_local::ROUTES.contains(&path) => {
            crate::app_local::read(&session.context(&host.tags), request.url()).map(|p| Some((p, 200)))
        }
        "/api/settings" => host.library_settings(&session.library).map(|mut p| {
            p.set("tasks", Json::object([("similar", session.research.similar_state())]));
            // 只有桌面外壳读的全应用设置（#451）：浏览器里打开的图库页没有这一节。
            if crate::app_desktop::available() {
                let after = crate::app_preferences::after_tasks();
                p.set("desktop", Json::object([(crate::app_preferences::AFTER_TASKS, after.into())]));
            }
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
    // 只在把它登记进图库列表失败时（#176）：页面提示一次；平常的应答不变。
    if let Some(why) = &session.unlisted {
        p.set("unlisted", why.as_str().into());
    }
    // 只在打开时做过升级之后（#193）：升级前的备份在哪里。
    if let Some(backup) = &session.upgraded {
        p.set("upgraded", Json::object([("backup", backup.as_str().into())]));
    }
    // 只在打开时抢救过 source.sqlite 之后（#228）：做了什么，损坏的文件移到了哪里。
    if let Some(report) = &session.rescued {
        p.set("rescued", report.clone());
    }
    Ok(p)
}

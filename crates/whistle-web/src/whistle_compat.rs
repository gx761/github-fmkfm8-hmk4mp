//! whistle 兼容适配层。
//!
//! 直接服务 whistle 编译好的前端（`whistle-htdocs/`，见 VENDORED.md），并实现
//! whistle 前端所需的 `/cgi-bin/*` 后端协议，把 whistle-rs 的抓包/规则数据映射成
//! whistle 的 session/规则 结构，让其原生 UI 完整跑起来。
//!
//! 已实现：
//! - Network 抓包（`init` / `get-data` / `get-frames`）；
//! - **规则编辑**：`rules/{list,add,remove,rename,select,unselect,enable-default,
//!   disable-default,allow-multiple-choice,enable-back-rules-first,disable-all-rules,
//!   move-to,enabled,recycle/*}`；
//! - **Values 编辑**：`values/{list,add,remove,rename,move-to,get,value,recycle/*}`。
//!
//! 所有写操作落到 [`WhistleData`]（持久化到 `whistle-ui.json`），并即时重算**生效规则**
//! 热替换进代理内核（[`WebState::whistle_apply_and_save`]）。
//!
//! 协议要点（逆向自 whistle 2.10.4 `biz/webui`）：
//! - 写操作为 `application/x-www-form-urlencoded`，被保存的规则文本是 **`value`** 参数；
//!   `select`/`enable-default`/`add`/`unselect`/`values/add` 均会 upsert `value`；
//! - `select` 系列响应的 `list` 是**已选名字数组**，而 `rules/list` 的 `list` 是对象数组
//!   `{index,name,data,selected}`；
//! - `ec===0` 表示成功，非 0 表示失败。

use std::collections::HashMap;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::Router;
use include_dir::{include_dir, Dir};
use serde_json::{json, Map, Value};
use whistle_capture::Traffic;

use crate::whistle_store::WhistleData;
use crate::WebState;

/// 内嵌的 whistle 前端静态资源（编译期打包）。
static HTDOCS: Dir = include_dir!("$CARGO_MANIFEST_DIR/whistle-htdocs");

/// 单次 get-data 返回的最大新条目数。
const COUNT: usize = 600;
/// 与所内嵌前端匹配的 whistle 版本号（避免前端版本不匹配提示）。
const WVERSION: &str = "2.10.4";
/// 写请求体大小上限（与 whistle 的 body-parser 3mb 对齐，留足余量）。
const BODY_LIMIT: usize = 8 * 1024 * 1024;

/// 构建 whistle 兼容 UI 的路由。
pub fn whistle_router(state: WebState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/cgi-bin/init", any(init))
        .route("/cgi-bin/get-data", any(get_data))
        .route("/cgi-bin/get-frames", any(get_frames))
        .route("/cgi-bin/server-info", any(server_info))
        .route("/cgi-bin/status", any(status))
        .route("/cgi-bin/log/get", any(log_get))
        // ---- 规则 ----
        .route("/cgi-bin/rules/list", get(rules_list))
        .route("/cgi-bin/rules/enabled", get(rules_enabled))
        .route("/cgi-bin/rules/add", post(rules_add))
        .route("/cgi-bin/rules/remove", post(rules_remove))
        .route("/cgi-bin/rules/rename", post(rules_rename))
        .route("/cgi-bin/rules/select", post(rules_select))
        .route("/cgi-bin/rules/unselect", post(rules_unselect))
        .route("/cgi-bin/rules/enable-default", post(rules_enable_default))
        .route(
            "/cgi-bin/rules/disable-default",
            post(rules_disable_default),
        )
        .route(
            "/cgi-bin/rules/allow-multiple-choice",
            post(rules_allow_multiple),
        )
        .route(
            "/cgi-bin/rules/enable-back-rules-first",
            post(rules_back_first),
        )
        .route("/cgi-bin/rules/disable-all-rules", post(rules_disable_all))
        .route("/cgi-bin/rules/move-to", post(rules_move_to))
        .route("/cgi-bin/rules/recycle/list", get(rules_recycle_list))
        .route("/cgi-bin/rules/recycle/view", get(rules_recycle_view))
        .route("/cgi-bin/rules/recycle/remove", post(rules_recycle_remove))
        .route(
            "/cgi-bin/intercept-https-connects",
            post(intercept_https_connects),
        )
        // ---- Values ----
        .route("/cgi-bin/values/list", get(values_list))
        .route("/cgi-bin/values/add", post(values_add))
        .route("/cgi-bin/values/remove", post(values_remove))
        .route("/cgi-bin/values/rename", post(values_rename))
        .route("/cgi-bin/values/move-to", post(values_move_to))
        .route("/cgi-bin/values/get", get(values_get))
        .route("/cgi-bin/values/value", get(values_value))
        .route("/cgi-bin/values/recycle/list", get(values_recycle_list))
        .route("/cgi-bin/values/recycle/view", get(values_recycle_view))
        .route(
            "/cgi-bin/values/recycle/remove",
            post(values_recycle_remove),
        )
        // 其余静态资源与未实现的 cgi-bin 走兜底。
        .route("/{*path}", get(static_or_stub).post(static_or_stub))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

// ============ 表单解析 ============

/// 解析后的请求参数（保留重复键，以支持 `list[]=a&list[]=b`）。
struct Params(Vec<(String, String)>);

impl Params {
    /// 从 urlencoded 文本解析。
    fn parse(body: &str) -> Self {
        Params(
            form_urlencoded::parse(body.as_bytes())
                .into_owned()
                .collect(),
        )
    }

    /// 取首个匹配键的值。
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// 取某键的所有值（同时匹配 `key` 与 `key[]`）。
    fn list(&self, key: &str) -> Vec<String> {
        let bracket = format!("{key}[]");
        self.0
            .iter()
            .filter(|(k, _)| k == key || *k == bracket)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// 真值判断（whistle 约定：`1` / 非空且非 `0`/`false`）。
    fn truthy(&self, key: &str) -> bool {
        self.get(key)
            .map(|v| !v.is_empty() && v != "0" && v != "false")
            .unwrap_or(false)
    }
}

// ============ 静态资源 ============

/// 首页：内嵌 index.html。
async fn index() -> Response {
    serve_embedded("index.html")
        .unwrap_or_else(|| (axum::http::StatusCode::NOT_FOUND, "no index").into_response())
}

/// 静态资源；`cgi-bin/*` 未实现端点返回 `{ec:0}` 占位（避免前端 404 崩溃）。
async fn static_or_stub(Path(path): Path<String>) -> Response {
    if path.starts_with("cgi-bin/") {
        return axum::Json(json!({ "ec": 0 })).into_response();
    }
    serve_embedded(&path)
        .unwrap_or_else(|| (axum::http::StatusCode::NOT_FOUND, "not found").into_response())
}

fn serve_embedded(path: &str) -> Option<Response> {
    let file = HTDOCS.get_file(path)?;
    let ct = content_type(path);
    Some(
        (
            [(axum::http::header::CONTENT_TYPE, ct)],
            file.contents().to_vec(),
        )
            .into_response(),
    )
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

// ============ 启动 / 轮询 ============

/// `cgi-bin/init`：前端启动数据。
async fn init(State(s): State<WebState>) -> axum::Json<Value> {
    let intercept = s.intercept_all.load(std::sync::atomic::Ordering::Relaxed);
    let d = s.whistle.read().unwrap();
    axum::Json(json!({
        "ec": 0,
        "version": WVERSION,
        "wName": "whistle-rs",
        "disableInstaller": false,
        "account": Value::Null,
        "supportH2": true,
        "hasInvalidCerts": false,
        "enableHttp2": true,
        "interceptHttpsConnects": intercept,
        "server": server_obj(),
        "clientId": "whistle-rs",
        "clientIp": "127.0.0.1",
        "lastDataId": "0",
        "lastSvrLogId": Value::Null,
        "rules": rules_payload(&d),
        "values": values_payload(&d),
        "plugins": {},
        "disabledPlugins": {},
        "disabledAllPlugins": false,
        "disabledAllRules": d.disabled_all_rules,
        "allowMultipleChoice": d.allow_multiple_choice,
        "backRulesFirst": d.back_rules_first,
    }))
}

/// `cgi-bin/get-data`：Network 轮询。
async fn get_data(
    State(s): State<WebState>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::Json<Value> {
    // 升序（按 id）排列全部条目。
    let mut all = s.store.list(2000);
    all.reverse();

    let end_id = all.last().map(session_id);
    let cursor = q.get("startTime").and_then(|x| parse_cursor(x));

    let news: Vec<&Traffic> = match cursor {
        Some(c) => all.iter().filter(|t| t.id > c).take(COUNT).collect(),
        None => {
            let n = all.len();
            all[n.saturating_sub(COUNT)..].iter().collect()
        }
    };

    let mut data = Map::new();
    for t in &news {
        data.insert(session_id(t), session_json(t));
    }
    // 前端请求具体 ids（如取详情 body）时也带上。
    if let Some(ids) = q.get("ids") {
        for idstr in ids.split(',').filter(|x| !x.is_empty()) {
            if let Some(num) = parse_cursor(idstr) {
                if let Some(t) = all.iter().find(|t| t.id == num) {
                    data.insert(session_id(t), session_json(t));
                }
            }
        }
    }

    let new_ids: Vec<String> = news.iter().map(|t| session_id(t)).collect();
    let last_id = new_ids.last().cloned().or_else(|| end_id.clone());
    // whistle 前端会读取 n.ids.length（请求的具体 id 回显）；缺失会导致整段网络处理崩溃。
    let req_ids: Vec<String> = q
        .get("ids")
        .map(|s| {
            s.split(',')
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    // WebSocket 帧（当前选中会话）。
    let frames = q
        .get("curReqId")
        .and_then(|cur| frames_for(&all, cur))
        .unwrap_or_else(|| Value::Array(vec![]));

    let intercept = s.intercept_all.load(std::sync::atomic::Ordering::Relaxed);
    let d = s.whistle.read().unwrap();
    // whistle 的 get-data 顶层是元信息，网络数据嵌套在 `data`（= proxy.getData 的结果）。
    let network = json!({
        "ids": req_ids,
        "newIds": new_ids,
        "data": Value::Object(data),
        "lastId": last_id,
        "endId": end_id,
        "hasNew": false,
        "frames": frames,
        "lastFrameId": Value::Null,
        "tunnelIps": {},
        "socketStatus": Value::Null,
        "composerTime": Value::Null,
    });
    axum::Json(json!({
        "ec": 0,
        "wName": "whistle-rs",
        "version": WVERSION,
        "supportH2": true,
        "hasInvalidCerts": false,
        "clientIp": "127.0.0.1",
        "server": server_obj(),
        "curSvrLogId": Value::Null,
        "lastSvrLogId": Value::Null,
        "svrLog": [],
        "plugins": {},
        "disabledPlugins": {},
        "allowMultipleChoice": d.allow_multiple_choice,
        "backRulesFirst": d.back_rules_first,
        "enabledCount": d.enabled_count(),
        "disabledAllPlugins": false,
        "disabledAllRules": d.disabled_all_rules,
        "interceptHttpsConnects": intercept,
        "enableHttp2": true,
        "defaultRulesIsDisabled": d.default_disabled,
        "list": [],
        "data": network,
    }))
}

async fn get_frames(
    State(s): State<WebState>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::Json<Value> {
    let all = s.store.list(2000);
    let frames = q
        .get("curReqId")
        .and_then(|cur| frames_for(&all, cur))
        .unwrap_or_else(|| Value::Array(vec![]));
    axum::Json(json!({ "ec": 0, "frames": frames }))
}

async fn server_info() -> axum::Json<Value> {
    axum::Json(json!({ "ec": 0, "server": server_obj() }))
}

async fn status() -> axum::Json<Value> {
    axum::Json(json!({ "ec": 0 }))
}

/// `cgi-bin/log/get`：服务端日志轮询（前端读取 `log` 数组）。
async fn log_get() -> axum::Json<Value> {
    axum::Json(json!({ "ec": 0, "log": [], "ids": [], "newIds": [] }))
}

// ============ 规则：读取 ============

async fn rules_list(State(s): State<WebState>) -> axum::Json<Value> {
    let d = s.whistle.read().unwrap();
    axum::Json(rules_payload(&d))
}

/// `cgi-bin/rules/enabled`：查看生效规则（只读）。
async fn rules_enabled(State(s): State<WebState>) -> axum::Json<Value> {
    let text = s.whistle.read().unwrap().effective_text();
    axum::Json(json!({ "ec": 0, "mflag": "", "list": text }))
}

// ============ 规则：写入 ============

/// `cgi-bin/rules/add`：新建分组（或写入 Default 文本）。
async fn rules_add(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let name = p.get("name").unwrap_or("").to_string();
    let value = p.get("value").unwrap_or("").to_string();
    let selected = p.truthy("selected");
    let mut recycle_list: Option<Vec<String>> = None;
    if !name.is_empty() {
        let mut d = s.whistle.write().unwrap();
        if name == "Default" {
            d.default_rules = value;
            if selected {
                d.default_disabled = false;
            }
        } else {
            d.add_rule(&name, &value);
            if p.truthy("addToTop") {
                d.move_rule_top(&name);
            }
            if selected {
                d.select_rule(&name);
            }
        }
        if let Some(rf) = p.get("recycleFilename") {
            d.drop_recycled_rule(rf);
            recycle_list = Some(d.rules_recycle.iter().map(|x| x.name.clone()).collect());
        }
    }
    s.whistle_apply_and_save();
    match recycle_list {
        Some(list) => axum::Json(json!({ "ec": 0, "list": list })),
        None => axum::Json(json!({ "ec": 0 })),
    }
}

/// `cgi-bin/rules/remove`：删除一个或多个分组。
async fn rules_remove(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let mut names = p.list("list");
    if names.is_empty() {
        if let Some(n) = p.get("name") {
            names.push(n.to_string());
        }
    }
    {
        let mut d = s.whistle.write().unwrap();
        for n in &names {
            d.remove_rule(n);
        }
    }
    s.whistle_apply_and_save();
    axum::Json(json!({ "ec": 0 }))
}

/// `cgi-bin/rules/rename`：重命名分组。
async fn rules_rename(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    if let (Some(name), Some(new_name)) = (p.get("name"), p.get("newName")) {
        let mut d = s.whistle.write().unwrap();
        d.rename_rule(name, new_name);
    }
    s.whistle_apply_and_save();
    axum::Json(json!({ "ec": 0 }))
}

/// `cgi-bin/rules/select`：选中分组并保存其文本（编辑保存入口）。
async fn rules_select(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let name = p.get("name").unwrap_or("").to_string();
    let value = p.get("value").unwrap_or("").to_string();
    let resp = {
        let mut d = s.whistle.write().unwrap();
        if !name.is_empty() {
            d.add_rule(&name, &value); // upsert 文本
            d.select_rule(&name);
        }
        select_response(&d)
    };
    s.whistle_apply_and_save();
    axum::Json(resp)
}

/// `cgi-bin/rules/unselect`：取消选中（仍保存文本）。
async fn rules_unselect(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let name = p.get("name").unwrap_or("").to_string();
    let value = p.get("value").unwrap_or("").to_string();
    let resp = {
        let mut d = s.whistle.write().unwrap();
        if !name.is_empty() {
            d.add_rule(&name, &value);
            d.unselect_rule(&name);
        }
        select_response(&d)
    };
    s.whistle_apply_and_save();
    axum::Json(resp)
}

/// `cgi-bin/rules/enable-default`：启用并保存 Default 规则文本。
async fn rules_enable_default(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let resp = {
        let mut d = s.whistle.write().unwrap();
        if let Some(v) = p.get("value") {
            d.default_rules = v.to_string();
        }
        d.default_disabled = false;
        select_response(&d)
    };
    s.whistle_apply_and_save();
    axum::Json(resp)
}

/// `cgi-bin/rules/disable-default`：禁用 Default 规则。
async fn rules_disable_default(State(s): State<WebState>) -> axum::Json<Value> {
    let resp = {
        let mut d = s.whistle.write().unwrap();
        d.default_disabled = true;
        select_response(&d)
    };
    s.whistle_apply_and_save();
    axum::Json(resp)
}

/// `cgi-bin/rules/allow-multiple-choice`：切换单选/多选。
async fn rules_allow_multiple(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    {
        let mut d = s.whistle.write().unwrap();
        d.allow_multiple_choice = p.truthy("allowMultipleChoice");
        // 切回单选时仅保留最后一个选中项。
        if !d.allow_multiple_choice {
            if let Some(last) = d.selected_names().last().cloned() {
                for r in &mut d.rules {
                    r.selected = r.name == last;
                }
            }
        }
    }
    s.whistle_apply_and_save();
    axum::Json(json!({ "ec": 0 }))
}

/// `cgi-bin/rules/enable-back-rules-first`：切换后置规则优先。
async fn rules_back_first(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    {
        let mut d = s.whistle.write().unwrap();
        d.back_rules_first = p.truthy("backRulesFirst");
    }
    s.whistle_apply_and_save();
    axum::Json(json!({ "ec": 0 }))
}

/// `cgi-bin/rules/disable-all-rules`：总开关。
async fn rules_disable_all(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    {
        let mut d = s.whistle.write().unwrap();
        d.disabled_all_rules = p.truthy("disabledAllRules");
    }
    s.whistle_apply_and_save();
    axum::Json(json!({ "ec": 0 }))
}

/// `cgi-bin/intercept-https-connects`：切换「解密所有 HTTPS」（whistle 同名开关）。
///
/// 关闭时只解密命中规则的 host（默认），其余 HTTPS 盲隧道直通——这样未安装根证书
/// 也不会导致普通 HTTPS 站点打不开。
async fn intercept_https_connects(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let on = p.truthy("interceptHttpsConnects");
    s.intercept_all
        .store(on, std::sync::atomic::Ordering::Relaxed);
    axum::Json(json!({ "ec": 0 }))
}

/// `cgi-bin/rules/move-to`：拖拽排序。
async fn rules_move_to(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let ok = {
        let mut d = s.whistle.write().unwrap();
        match (p.get("from"), p.get("to")) {
            (Some(from), Some(to)) => {
                if p.get("toTop") == Some("true") {
                    d.move_rule_top(from)
                } else {
                    d.move_rule_to(from, to)
                }
            }
            _ => false,
        }
    };
    s.whistle_apply_and_save();
    axum::Json(json!({ "ec": if ok { 0 } else { 2 } }))
}

// ============ 规则：回收站 ============

async fn rules_recycle_list(State(s): State<WebState>) -> axum::Json<Value> {
    let d = s.whistle.read().unwrap();
    let list: Vec<String> = d.rules_recycle.iter().map(|x| x.name.clone()).collect();
    axum::Json(json!({ "ec": 0, "list": list }))
}

async fn rules_recycle_view(
    State(s): State<WebState>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::Json<Value> {
    let d = s.whistle.read().unwrap();
    let name = q.get("name").map(String::as_str).unwrap_or("");
    match d.rules_recycle.iter().find(|x| x.name == name) {
        Some(it) => axum::Json(json!({ "ec": 0, "data": it.data })),
        None => axum::Json(json!({ "ec": 3 })),
    }
}

async fn rules_recycle_remove(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let list: Vec<String> = {
        let mut d = s.whistle.write().unwrap();
        if let Some(name) = p.get("name") {
            d.drop_recycled_rule(name);
        }
        d.rules_recycle.iter().map(|x| x.name.clone()).collect()
    };
    axum::Json(json!({ "ec": 0, "list": list }))
}

// ============ Values：读取 ============

async fn values_list(State(s): State<WebState>) -> axum::Json<Value> {
    let d = s.whistle.read().unwrap();
    axum::Json(values_payload(&d))
}

/// `cgi-bin/values/get`：Values 编辑器设置 + 全量列表（无 `ec`）。
async fn values_get(State(s): State<WebState>) -> axum::Json<Value> {
    let d = s.whistle.read().unwrap();
    axum::Json(json!({
        "fontSize": Value::Null,
        "theme": Value::Null,
        "showLineNumbers": false,
        "values": values_list_items(&d),
    }))
}

/// `cgi-bin/values/value?key=<name>`：取单个 value 文本（无 `ec`）。
async fn values_value(
    State(s): State<WebState>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::Json<Value> {
    let d = s.whistle.read().unwrap();
    let key = q.get("key").map(String::as_str).unwrap_or("");
    let value = d
        .values
        .iter()
        .find(|v| v.name == key)
        .map(|v| v.data.clone())
        .unwrap_or_default();
    axum::Json(json!({ "value": value }))
}

// ============ Values：写入 ============

/// `cgi-bin/values/add`：新建/保存 value（编辑保存入口）。
async fn values_add(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let name = p.get("name").unwrap_or("").to_string();
    let value = p.get("value").unwrap_or("").to_string();
    let mut recycle_list: Option<Vec<String>> = None;
    if !name.is_empty() {
        let mut d = s.whistle.write().unwrap();
        d.add_value(&name, &value);
        if let Some(rf) = p.get("recycleFilename") {
            d.drop_recycled_value(rf);
            recycle_list = Some(d.values_recycle.iter().map(|x| x.name.clone()).collect());
        }
        d.save(&crate::whistle_store::data_path(&s.data_dir));
    }
    match recycle_list {
        Some(list) => axum::Json(json!({ "ec": 0, "list": list })),
        None => axum::Json(json!({ "ec": 0 })),
    }
}

async fn values_remove(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let mut names = p.list("list");
    if names.is_empty() {
        if let Some(n) = p.get("name") {
            names.push(n.to_string());
        }
    }
    {
        let mut d = s.whistle.write().unwrap();
        for n in &names {
            d.remove_value(n);
        }
        d.save(&crate::whistle_store::data_path(&s.data_dir));
    }
    axum::Json(json!({ "ec": 0 }))
}

async fn values_rename(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    if let (Some(name), Some(new_name)) = (p.get("name"), p.get("newName")) {
        let mut d = s.whistle.write().unwrap();
        d.rename_value(name, new_name);
        d.save(&crate::whistle_store::data_path(&s.data_dir));
    }
    axum::Json(json!({ "ec": 0 }))
}

async fn values_move_to(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let ok = {
        let mut d = s.whistle.write().unwrap();
        let r = match (p.get("from"), p.get("to")) {
            (Some(from), Some(to)) => d.move_value_to(from, to),
            _ => false,
        };
        d.save(&crate::whistle_store::data_path(&s.data_dir));
        r
    };
    axum::Json(json!({ "ec": if ok { 0 } else { 2 } }))
}

async fn values_recycle_list(State(s): State<WebState>) -> axum::Json<Value> {
    let d = s.whistle.read().unwrap();
    let list: Vec<String> = d.values_recycle.iter().map(|x| x.name.clone()).collect();
    axum::Json(json!({ "ec": 0, "list": list }))
}

async fn values_recycle_view(
    State(s): State<WebState>,
    Query(q): Query<HashMap<String, String>>,
) -> axum::Json<Value> {
    let d = s.whistle.read().unwrap();
    let name = q.get("name").map(String::as_str).unwrap_or("");
    match d.values_recycle.iter().find(|x| x.name == name) {
        Some(it) => axum::Json(json!({ "ec": 0, "data": it.data })),
        None => axum::Json(json!({ "ec": 3 })),
    }
}

async fn values_recycle_remove(State(s): State<WebState>, body: String) -> axum::Json<Value> {
    let p = Params::parse(&body);
    let list: Vec<String> = {
        let mut d = s.whistle.write().unwrap();
        if let Some(name) = p.get("name") {
            d.drop_recycled_value(name);
        }
        d.values_recycle.iter().map(|x| x.name.clone()).collect()
    };
    axum::Json(json!({ "ec": 0, "list": list }))
}

// ============ 映射辅助 ============

fn server_obj() -> Value {
    // 前端 updateServerInfo 会读取 ipv4/ipv6（数组）等字段，缺失会崩溃。
    json!({
        "name": "whistle-rs",
        "version": WVERSION,
        "nodeVersion": "rust",
        "whistleId": "whistle-rs",
        "pid": 0,
        "host": "127.0.0.1",
        "ipv4": [],
        "ipv6": [],
        "ipv6Only": false,
        "username": "whistle-rs",
    })
}

/// 规则面板载荷（对齐 whistle `rules/list` 字段集）。
fn rules_payload(d: &WhistleData) -> Value {
    json!({
        "ec": 0,
        "enabledCount": d.enabled_count(),
        "defaultRulesIsDisabled": d.default_disabled,
        "defaultRules": d.default_rules,
        "allowMultipleChoice": d.allow_multiple_choice,
        "backRulesFirst": d.back_rules_first,
        "list": rules_list_items(d),
    })
}

/// 规则分组列表项（`{index,name,data,selected}`）。
fn rules_list_items(d: &WhistleData) -> Value {
    let items: Vec<Value> = d
        .rules
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let is_group = r.name.starts_with('\r');
            json!({
                "index": i,
                "name": r.name,
                "data": if is_group { "" } else { r.data.as_str() },
                "selected": r.selected && !is_group,
            })
        })
        .collect();
    Value::Array(items)
}

fn values_payload(d: &WhistleData) -> Value {
    json!({ "ec": 0, "list": values_list_items(d) })
}

fn values_list_items(d: &WhistleData) -> Value {
    let items: Vec<Value> = d
        .values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            json!({
                "index": i,
                "name": v.name,
                "data": v.data,
                "selected": false,
            })
        })
        .collect();
    Value::Array(items)
}

/// `select`/`unselect`/`enable-default`/`disable-default` 的统一响应：
/// `{ec, defaultRulesIsDisabled, list:[选中名字]}`。
fn select_response(d: &WhistleData) -> Value {
    json!({
        "ec": 0,
        "defaultRulesIsDisabled": d.default_disabled,
        "list": d.selected_names(),
    })
}

/// session id：`"<startTime>-<id>"`，与 whistle 一致。
fn session_id(t: &Traffic) -> String {
    format!("{}-{}", t.start_time, t.id)
}

/// 从 startTime 游标或 id 串中解析出 whistle-rs 数值 id（末段）。
fn parse_cursor(s: &str) -> Option<u64> {
    if s.is_empty() || s == "0" || s == "-1" || s == "-2" || s == "-3" {
        return None;
    }
    s.rsplit('-').next().and_then(|n| n.parse::<u64>().ok())
}

fn headers_obj(hs: &[whistle_capture::Header]) -> Value {
    let mut m = Map::new();
    for h in hs {
        m.insert(h.name.to_ascii_lowercase(), Value::String(h.value.clone()));
    }
    Value::Object(m)
}

fn raw_names(hs: &[whistle_capture::Header]) -> Value {
    let mut m = Map::new();
    for h in hs {
        m.insert(h.name.to_ascii_lowercase(), Value::String(h.name.clone()));
    }
    Value::Object(m)
}

/// 把一条 Traffic 映射为 whistle session 对象。
/// 构造 whistle session 的 `rules` 字段（命中规则视图）。
///
/// 结构对齐 whistle 2.10.4 `matched-rule.js` 的消费：按协议名分组，每项至少含
/// `rawPattern`/`matcher`/`raw`，多匹配协议（如 reqHeaders）再带 `list`。
/// `traffic.rules` 是命中的操作 token（如 `reqHeaders://swimlane=...`），
/// 以请求 host 作为展示用的 rawPattern。
fn session_rules(t: &Traffic) -> Value {
    if t.rules.is_empty() {
        return Value::Object(Map::new());
    }
    let pattern = t.host.clone();
    // 按协议分组并保持出现顺序。
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for raw in &t.rules {
        let proto = raw
            .split_once("://")
            .map(|(p, _)| p)
            .unwrap_or("host")
            .to_string();
        match groups.iter_mut().find(|(p, _)| *p == proto) {
            Some((_, v)) => v.push(raw.clone()),
            None => groups.push((proto, vec![raw.clone()])),
        }
    }
    let mut obj = Map::new();
    for (proto, raws) in groups {
        let list: Vec<Value> = raws
            .iter()
            .map(|r| {
                json!({
                    "name": proto,
                    "rawPattern": pattern,
                    "matcher": r,
                    "raw": format!("{pattern} {r}"),
                })
            })
            .collect();
        let first = &raws[0];
        obj.insert(
            proto.clone(),
            json!({
                "name": proto,
                "rawPattern": pattern,
                "pattern": pattern,
                "matcher": first,
                "rawMatcher": first,
                "raw": format!("{pattern} {first}"),
                "list": list,
            }),
        );
    }
    Value::Object(obj)
}

fn session_json(t: &Traffic) -> Value {
    let dur = t.duration_ms.unwrap_or(0);
    let end = t.start_time + dur;
    let is_https = matches!(t.protocol.as_str(), "https" | "wss" | "tunnel");
    json!({
        "id": session_id(t),
        "url": t.url,
        "method": t.method,
        "httpVersion": "1.1",
        "isHttps": is_https,
        "startTime": t.start_time,
        "dnsTime": t.start_time,
        "requestTime": t.start_time,
        "responseTime": end,
        "endTime": end,
        "req": {
            "method": t.method,
            "httpVersion": "1.1",
            "headers": headers_obj(&t.req_headers),
            "rawHeaderNames": raw_names(&t.req_headers),
            "size": t.req_body_size,
            "body": t.req_body.clone().unwrap_or_default(),
        },
        "res": {
            "statusCode": t.status.unwrap_or(0),
            "statusMessage": "",
            "httpVersion": "1.1",
            "headers": headers_obj(&t.res_headers),
            "rawHeaderNames": raw_names(&t.res_headers),
            "size": t.res_body_size,
            "body": t.res_body.clone().unwrap_or_default(),
        },
        "rules": session_rules(t),
        "rulesHeaders": {},
        "frames": Value::Array(vec![]),
        "version": WVERSION,
    })
}

/// 为某个会话构造 whistle frame 数组（WebSocket 消息）。
fn frames_for(all: &[Traffic], cur: &str) -> Option<Value> {
    let num = parse_cursor(cur)?;
    let t = all.iter().find(|t| t.id == num)?;
    if t.ws_messages.is_empty() {
        return Some(Value::Array(vec![]));
    }
    let frames: Vec<Value> = t
        .ws_messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            json!({
                "reqId": cur,
                "frameId": format!("{}-{}", t.start_time, i),
                "isClient": m.dir == "send",
                "length": m.size,
                "opcode": m.opcode,
                "text": m.text.clone().unwrap_or_default(),
                "closed": m.opcode == 0x8,
            })
        })
        .collect();
    Some(Value::Array(frames))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use std::sync::{Arc, RwLock};
    use tower::ServiceExt;
    use whistle_capture::CaptureStore;
    use whistle_rules::RuleSet;

    fn test_state(tag: &str) -> (WebState, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("whistle-rs-compat-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = WebState {
            store: Arc::new(CaptureStore::new(100)),
            rules: Arc::new(RwLock::new(RuleSet::default())),
            rules_text: Arc::new(RwLock::new(String::new())),
            proxy_addr: "127.0.0.1:0".to_string(),
            ui_mode: "whistle".to_string(),
            whistle: Arc::new(RwLock::new(WhistleData::default())),
            data_dir: dir.clone(),
            intercept_all: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        (s, dir)
    }

    async fn post(router: &Router, uri: &str, body: &str) -> Value {
        let req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn get(router: &Router, uri: &str) -> Value {
        let req = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn editing_rules_updates_effective_ruleset_and_persists() {
        let (s, dir) = test_state("rules");
        let router = whistle_router(s.clone());

        // 1) 新建并选中一个分组 → 生效规则应包含它。
        let r = post(
            &router,
            "/cgi-bin/rules/add",
            "name=g1&value=a.com%20host%3A%2F%2F1.1.1.1&selected=1",
        )
        .await;
        assert_eq!(r["ec"], 0);
        assert_eq!(s.rules.read().unwrap().len(), 1);

        // 2) rules/list 回显该分组（对象数组，selected=true，data 已保存）。
        let list = get(&router, "/cgi-bin/rules/list").await;
        let items = list["list"].as_array().unwrap();
        let g1 = items.iter().find(|x| x["name"] == "g1").unwrap();
        assert_eq!(g1["selected"], true);
        assert!(g1["data"].as_str().unwrap().contains("a.com"));

        // 3) select 端点保存编辑后的文本（upsert value），响应 list 为已选名字数组。
        let r = post(
            &router,
            "/cgi-bin/rules/select",
            "name=g1&value=a.com%20host%3A%2F%2F9.9.9.9",
        )
        .await;
        assert_eq!(r["ec"], 0);
        assert_eq!(r["list"][0], "g1");
        assert!(s.rules_text.read().unwrap().contains("9.9.9.9"));

        // 4) 启用并保存 Default 规则 → 生效规则增加一条。
        let r = post(
            &router,
            "/cgi-bin/rules/enable-default",
            "value=b.com%20host%3A%2F%2F2.2.2.2",
        )
        .await;
        assert_eq!(r["ec"], 0);
        assert_eq!(r["defaultRulesIsDisabled"], false);
        assert_eq!(s.rules.read().unwrap().len(), 2);

        // 5) 总开关关闭所有规则 → 生效规则清空。
        let r = post(
            &router,
            "/cgi-bin/rules/disable-all-rules",
            "disabledAllRules=1",
        )
        .await;
        assert_eq!(r["ec"], 0);
        assert_eq!(s.rules.read().unwrap().len(), 0);
        // 重新打开。
        post(
            &router,
            "/cgi-bin/rules/disable-all-rules",
            "disabledAllRules=0",
        )
        .await;
        assert_eq!(s.rules.read().unwrap().len(), 2);

        // 6) 持久化文件已写入。
        let saved = crate::whistle_store::WhistleData::load(&crate::whistle_store::data_path(&dir))
            .expect("whistle-ui.json 应已写入");
        assert!(saved.rules.iter().any(|x| x.name == "g1"));
        assert!(saved.default_rules.contains("b.com"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn editing_values_roundtrips() {
        let (s, dir) = test_state("values");
        let router = whistle_router(s.clone());

        let r = post(&router, "/cgi-bin/values/add", "name=tok&value=secret123").await;
        assert_eq!(r["ec"], 0);

        let list = get(&router, "/cgi-bin/values/list").await;
        let item = list["list"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == "tok")
            .unwrap();
        assert_eq!(item["data"], "secret123");

        let single = get(&router, "/cgi-bin/values/value?key=tok").await;
        assert_eq!(single["value"], "secret123");

        // 删除 → 进回收站 → 还原。
        post(&router, "/cgi-bin/values/remove", "name=tok").await;
        let list = get(&router, "/cgi-bin/values/list").await;
        assert!(list["list"].as_array().unwrap().is_empty());
        let recycle = get(&router, "/cgi-bin/values/recycle/list").await;
        assert_eq!(recycle["list"][0], "tok");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn single_vs_multiple_choice() {
        let (s, dir) = test_state("choice");
        let router = whistle_router(s.clone());
        post(&router, "/cgi-bin/rules/add", "name=a&value=x").await;
        post(&router, "/cgi-bin/rules/add", "name=b&value=y").await;
        // 单选：选 a 再选 b，只剩 b。
        post(&router, "/cgi-bin/rules/select", "name=a&value=x").await;
        let r = post(&router, "/cgi-bin/rules/select", "name=b&value=y").await;
        assert_eq!(r["list"].as_array().unwrap().len(), 1);
        assert_eq!(r["list"][0], "b");
        // 切换多选后两者都能选中。
        post(
            &router,
            "/cgi-bin/rules/allow-multiple-choice",
            "allowMultipleChoice=1",
        )
        .await;
        post(&router, "/cgi-bin/rules/select", "name=a&value=x").await;
        let r = post(&router, "/cgi-bin/rules/select", "name=b&value=y").await;
        assert_eq!(r["list"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_rules_builds_whistle_structure() {
        let mut t = Traffic::new(1, "http", "GET", "http://h.test/x", "h.test", "c");
        t.rules = vec![
            "reqHeaders://swimlane=abc".to_string(),
            "host://1.2.3.4".to_string(),
        ];
        let v = session_rules(&t);
        let obj = v.as_object().unwrap();
        // 按协议分组。
        assert!(obj.contains_key("reqHeaders"));
        assert!(obj.contains_key("host"));
        let rh = &obj["reqHeaders"];
        // 命中规则视图所需的最小字段（rawPattern/matcher/raw + list）。
        assert_eq!(rh["rawPattern"], "h.test");
        assert_eq!(rh["matcher"], "reqHeaders://swimlane=abc");
        assert_eq!(rh["raw"], "h.test reqHeaders://swimlane=abc");
        assert_eq!(rh["list"].as_array().unwrap().len(), 1);
        assert_eq!(rh["list"][0]["matcher"], "reqHeaders://swimlane=abc");
        // 空规则 → 空对象（不会被标记为命中）。
        let empty = Traffic::new(2, "http", "GET", "http://h/", "h", "c");
        assert!(session_rules(&empty).as_object().unwrap().is_empty());
    }
}

//! Line-oriented sidecar for the Go core: one JSON request per line in, one
//! JSON response per line out. It signs with the current official client
//! generation (MetaSec v5 + Medusa v5) and returns the raw `video_model`
//! so the Go core keeps its own codec/fallback logic.

use std::io::{BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use hongguo_api::client::ApiEnv;
use hongguo_api::play_url::fetch_play_model;
use serde_json::{json, Value};

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn make_env() -> Result<ApiEnv, String> {
    let cdid = hongguo_signer::fresh_cdid();
    let mut env = ApiEnv {
        proxy: Default::default(),
        device: hongguo_signer::anonymous_profile(&cdid),
        cookie: None,
        x_tt_token: None,
    };
    let material = hongguo_api::register::RegisterMaterial::mint();
    let reg = hongguo_api::register::register_material(
        &env,
        &material,
        hongguo_api::register::REGISTER_ORIGIN,
        now_millis(),
    )
    .await
    .map_err(|e| format!("register: {e:?}"))?;
    env.device.set("device_id", &reg.device_id);
    env.device.set("iid", &reg.install_id);
    if !reg.cdid.is_empty() {
        env.device.set("cdid", &reg.cdid);
    }
    if !reg.openudid.is_empty() {
        env.device.set("openudid", &reg.openudid);
    }
    eprintln!(
        "registered device={} iid={} cdid={}",
        reg.device_id, reg.install_id, reg.cdid
    );
    Ok(env)
}

fn comment_json(item: &hongguo_api::danmaku::CommentItem) -> Value {
    json!({
        "id": item.comment_id,
        "text": item.text,
        "user_id": item.user_id,
        "user_name": item.user_name,
        "avatar": item.avatar,
        "created": item.create_time,
        "likes": item.digg_count,
        "replies": item.reply_count,
        "liked": item.user_digg,
        "score": item.score,
        "score_suffix": item.score_suffix_text
    })
}

fn reply_json(item: &hongguo_api::danmaku::ReplyItem) -> Value {
    json!({
        "id": item.reply_id,
        "text": item.text,
        "user_id": item.user_id,
        "user_name": item.user_name,
        "avatar": item.avatar,
        "created": item.create_time,
        "likes": item.digg_count
    })
}

#[tokio::main]
async fn main() {
    let env = match make_env().await {
        Ok(env) => env,
        Err(e) => {
            println!("{}", json!({"ok": false, "error": e}));
            return;
        }
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("bad json: {e}")}));
                let _ = out.flush();
                continue;
            }
        };
        // The Go host owns the account session; pass its cookie through so
        // account-scoped fields (follow state, paywall) match the UI.
        let cookie = request
            .get("cookie")
            .and_then(Value::as_str)
            .unwrap_or("");
        let call_env = if cookie.is_empty() {
            env.clone()
        } else {
            ApiEnv {
                cookie: Some(cookie.to_string()),
                ..env.clone()
            }
        };

        // Two tasks share this process: "episodes" pulls the official episode
        // table (numerical vids), everything else resolves one stream model.
        // Keeping the episode table here means an id/endpoint change shipped
        // upstream reaches the TVs through sidecar.json, without a new APK.
        if request.get("task").and_then(Value::as_str) == Some("episodes") {
            let series_id = request
                .get("series_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if series_id.is_empty() {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": "missing series_id"}));
                let _ = out.flush();
                continue;
            }
            match hongguo_api::detail::fetch_episode_list(&series_id, &call_env).await {
                Ok(list) => {
                    let episodes: Vec<Value> = list
                        .episodes
                        .iter()
                        .map(|episode| {
                            // The reference stores the episode blurb in
                            // `title`; the TV labels episodes by index, so it
                            // is not shipped (it can be several KB per row).
                            json!({
                                "index": episode.vid_index,
                                "vid": episode.vid,
                                "duration": episode.duration
                            })
                        })
                        .collect();
                    let payload = json!({
                        "ok": true,
                        "task": "episodes",
                        "series_id": if list.series_id.is_empty() { series_id } else { list.series_id },
                        "title": list.title,
                        "cover": list.cover,
                        "content_type": list.content_type,
                        "episodes": episodes
                    });
                    let _ = writeln!(out, "{payload}");
                }
                Err(e) => {
                    let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("{e:?}")}));
                }
            }
            let _ = out.flush();
            continue;
        }
        // Ranking: the request parameters and the cell_view parsing live in the
        // reference crate; the whole page (items + selector tabs) is returned as
        // JSON for the Go host to shape into its list model.
        if request.get("task").and_then(Value::as_str) == Some("rank") {
            let req = hongguo_api::rank::RankReq {
                selected: request.get("selected").and_then(Value::as_str).unwrap_or(""),
                sub: request.get("sub").and_then(Value::as_str).unwrap_or(""),
                panel: request.get("panel").and_then(Value::as_str),
                background: request.get("background").and_then(Value::as_str),
                offset: request.get("offset").and_then(Value::as_i64).unwrap_or(0),
                session_id: request.get("session_id").and_then(Value::as_str).unwrap_or(""),
                rank_version: request.get("rank_version").and_then(Value::as_str).unwrap_or(""),
            };
            match hongguo_api::rank::fetch_rank_ex(req, &call_env).await {
                Ok(page) => {
                    let payload = json!({
                        "ok": true,
                        "task": "rank",
                        "page": serde_json::to_value(&page).unwrap_or(Value::Null)
                    });
                    let _ = writeln!(out, "{payload}");
                }
                Err(e) => {
                    let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("{e:?}")}));
                }
            }
            let _ = out.flush();
            continue;
        }

        // Comments (one page) and replies (one page) for the discussion panel.
        // Read-only and stateless, like danmaku.
        if matches!(
            request.get("task").and_then(Value::as_str),
            Some("comments") | Some("replies")
        ) {
            let task = request.get("task").and_then(Value::as_str).unwrap_or("comments");
            let group_id = request.get("vid").and_then(Value::as_str).unwrap_or("");
            let book_id = request.get("series_id").and_then(Value::as_str).unwrap_or("");
            let comment_id = request.get("comment_id").and_then(Value::as_str).unwrap_or("");
            let cursor = request.get("cursor").and_then(Value::as_str).unwrap_or("");
            if group_id.is_empty() || book_id.is_empty() {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": "missing vid/series_id"}));
                let _ = out.flush();
                continue;
            }
            let payload = if task == "comments" {
                match hongguo_api::danmaku::fetch_comments_page(group_id, book_id, cursor, &call_env).await {
                    Ok(page) => {
                        let items: Vec<Value> = page.items.iter().map(comment_json).collect();
                        json!({
                            "ok": true,
                            "task": "comments",
                            "items": items,
                            "total": page.total,
                            "has_more": page.has_more,
                            "cursor": page.next_cursor
                        })
                    }
                    Err(e) => json!({"ok": false, "error": format!("{e:?}")}),
                }
            } else {
                match hongguo_api::danmaku::fetch_comment_replies(group_id, book_id, comment_id, cursor, &call_env).await {
                    Ok(page) => {
                        let items: Vec<Value> = page.items.iter().map(reply_json).collect();
                        json!({
                            "ok": true,
                            "task": "replies",
                            "items": items,
                            "has_more": page.has_more,
                            "cursor": page.next_cursor
                        })
                    }
                    Err(e) => json!({"ok": false, "error": format!("{e:?}")}),
                }
            };
            let _ = writeln!(out, "{payload}");
            let _ = out.flush();
            continue;
        }

        // Danmaku windows: commentapi with comment_type=20, one window per call.
        // The parameter shape moves with the official client, so it lives here
        // (sidecar.json) instead of the Go core.
        if request.get("task").and_then(Value::as_str) == Some("danmaku") {
            let group_id = request.get("vid").and_then(Value::as_str).unwrap_or("");
            let book_id = request.get("series_id").and_then(Value::as_str).unwrap_or("");
            if group_id.is_empty() || book_id.is_empty() {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": "missing vid/series_id"}));
                let _ = out.flush();
                continue;
            }
            let start = request.get("start").and_then(Value::as_u64).unwrap_or(0);
            let duration = request.get("duration").and_then(Value::as_u64).unwrap_or(600_000);
            let cursor = request.get("cursor").and_then(Value::as_str).unwrap_or("");
            match hongguo_api::danmaku::fetch_danmaku_window_page(
                group_id, book_id, start, duration, cursor, &call_env,
            )
            .await
            {
                Ok(page) => {
                    let items: Vec<Value> = page
                        .items
                        .iter()
                        .map(|d| {
                            json!({
                                "id": d.comment_id,
                                "text": d.text,
                                "offset_ms": d.offset_ms,
                                "digg_count": d.digg_count
                            })
                        })
                        .collect();
                    let payload = json!({
                        "ok": true,
                        "task": "danmaku",
                        "items": items,
                        "has_more": page.has_more,
                        "cursor": page.next_cursor
                    });
                    let _ = writeln!(out, "{payload}");
                }
                Err(e) => {
                    let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("{e:?}")}));
                }
            }
            let _ = out.flush();
            continue;
        }

        // Generic signed passthrough for the reading domain (history, progress,
        // favorites, follows). The Go host keeps the flow and payloads; only
        // signing/transport live here, so a signature generation change is a
        // sidecar update, not an APK or core change.
        if request.get("task").and_then(Value::as_str) == Some("reading") {
            let path = request.get("path").and_then(Value::as_str).unwrap_or("");
            if path.is_empty() {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": "missing path"}));
                let _ = out.flush();
                continue;
            }
            let method = request.get("method").and_then(Value::as_str).unwrap_or("GET").to_uppercase();
            let query: Vec<(String, String)> = request
                .get("query")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .map(|(key, value)| {
                            let text = value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string());
                            (key.clone(), text)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let body = request.get("body").map(|value| value.to_string().into_bytes());
            match hongguo_api::client::api_call_reading(
                hongguo_api::client::SINFONLINEC_API_ORIGIN,
                path,
                body,
                &query,
                &call_env,
            )
            .await
            {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes).to_string();
                    let payload = json!({"ok": true, "status": 200, "body": text});
                    let _ = writeln!(out, "{payload}");
                }
                Err(e) => {
                    let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("{e:?}")}));
                }
            }
            let _ = out.flush();
            continue;
        }
        // Series detail: title/cover/intro/episodes/actors from the official
        // app endpoints. Keeping it here means the web page is no longer part
        // of any playback-critical path, and a change in these endpoints only
        // needs a new sidecar (sidecar.json), not a new core.
        if request.get("task").and_then(Value::as_str) == Some("detail") {
            let series_id = request
                .get("series_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if series_id.is_empty() {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": "missing series_id"}));
                let _ = out.flush();
                continue;
            }
            let list = hongguo_api::detail::fetch_episode_list(&series_id, &call_env).await;
            let meta = hongguo_api::detail::fetch_series_meta(&series_id, &call_env).await.ok();
            match list {
                Ok(list) => {
                    let episodes: Vec<Value> = list
                        .episodes
                        .iter()
                        .map(|episode| {
                            json!({
                                "index": episode.vid_index,
                                "vid": episode.vid,
                                "duration": episode.duration
                            })
                        })
                        .collect();
                    let resolved = if list.series_id.is_empty() { series_id.clone() } else { list.series_id.clone() };
                    let mut payload = json!({
                        "ok": true,
                        "task": "detail",
                        "series_id": resolved,
                        "title": list.title,
                        "cover": list.cover,
                        "content_type": list.content_type,
                        "follow_count": list.followed_cnt,
                        "episode_count": episodes.len(),
                        "episodes": episodes
                    });
                    if let Some(meta) = meta {
                        if !meta.title.is_empty() {
                            payload["title"] = json!(meta.title);
                        }
                        if payload["cover"].as_str().unwrap_or("").is_empty() && !meta.cover.is_empty() {
                            payload["cover"] = json!(meta.cover);
                        }
                        payload["intro"] = json!(meta.intro);
                        payload["tags"] = json!(meta.tags);
                        payload["play_count"] = json!(meta.play_cnt);
                        payload["hot_score"] = json!(meta.hot_score);
                        payload["update_frequency"] = json!(meta.update_frequency);
                        payload["series_status"] = json!(meta.series_status);
                        payload["episode_total_count"] = json!(meta.episode_total_cnt);
                        let actors: Vec<Value> = meta
                            .creators
                            .iter()
                            .map(|creator| {
                                json!({
                                    "id": creator.user_id,
                                    "name": creator.name,
                                    "avatar": creator.avatar
                                })
                            })
                            .collect();
                        payload["actors"] = json!(actors);
                    }
                    let _ = writeln!(out, "{payload}");
                }
                Err(e) => {
                    let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("{e:?}")}));
                }
            }
            let _ = out.flush();
            continue;
        }
        let vid = request
            .get("vid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if vid.is_empty() {
            let _ = writeln!(out, "{}", json!({"ok": false, "error": "missing vid"}));
            let _ = out.flush();
            continue;
        }
        let content_type = request
            .get("content_type")
            .and_then(Value::as_u64)
            .unwrap_or(1004) as i64;
        match fetch_play_model(&vid, content_type, &env).await {
            Ok(model) => {
                let payload = json!({"ok": true, "model": model.to_string()});
                let _ = writeln!(out, "{payload}");
            }
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("{e:?}")}));
            }
        }
        let _ = out.flush();
    }
}

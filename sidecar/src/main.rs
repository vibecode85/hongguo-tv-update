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
            match hongguo_api::detail::fetch_episode_list(&series_id, &env).await {
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

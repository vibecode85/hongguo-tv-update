//! Line-oriented sidecar for the Go core: one JSON request per line in,
//! one JSON response per line out. It signs with the current official client
//! generation (MetaSec v5 + Medusa v5) and returns the raw `video_model`
//! so the Go core keeps its own codec/fallback logic.

use std::io::{BufRead, Write};

use hongguo_api::client::ApiEnv;
use hongguo_api::play_url::fetch_play_model;
use serde_json::{json, Value};

async fn make_env() -> Result<ApiEnv, String> {
    let cdid = hongguo_signer::fresh_cdid();
    let mut env = ApiEnv::anonymous(Default::default());
    env.device = hongguo_signer::anonymous_profile(&cdid);
    let reg = hongguo_api::register::register_device(&env)
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
    eprintln!("registered device={} iid={} cdid={}", reg.device_id, reg.install_id, reg.cdid);
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
        let vid = request.get("vid").and_then(Value::as_str).unwrap_or("").to_string();
        if vid.is_empty() {
            let _ = writeln!(out, "{}", json!({"ok": false, "error": "missing vid"}));
            let _ = out.flush();
            continue;
        }
        let content_type = request.get("content_type").and_then(Value::as_u64).unwrap_or(1004) as i64;
        match fetch_play_model(&vid, content_type, &env).await {
            Ok(model) => {
                // Field order is irrelevant; serde_json sorts keys.
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

"""Expose the reference crate's playback helpers and add fetch_play_model."""
import pathlib

root = pathlib.Path(__file__).resolve().parent / "extern" / "hongguo-desktop"

p = root / "crates/hongguo-api/src/client.rs"
s = p.read_text()
s = s.replace("pub(crate) const SINFONLINEA_API_ORIGIN", "pub const SINFONLINEA_API_ORIGIN")
s = s.replace("pub(crate) async fn api_call_playback_v5", "pub async fn api_call_playback_v5")
p.write_text(s)

p = root / "crates/hongguo-api/src/metasec/request.rs"
s = p.read_text().replace("pub(crate) async fn sign_playback", "pub async fn sign_playback")
p.write_text(s)

p = root / "crates/hongguo-api/src/metasec.rs"
s = p.read_text().replace("mod request;", "pub mod request;").replace(
    "pub(crate) use request::sign_playback;", "pub use request::sign_playback;"
)
p.write_text(s)

p = root / "crates/hongguo-api/src/play_url.rs"
s = p.read_text()
if "pub async fn fetch_play_model" not in s:
    anchor = "/// 单集取流（`/novel/player/video_model/v1/`）。"
    if anchor not in s:
        anchor = ""
    add = '''/// 取单集的原始 `video_model`（含 `video_list` / `fallback_api`）。
pub async fn fetch_play_model(
    vid: &str,
    content_type: i64,
    env: &super::client::ApiEnv,
) -> AppResult<Value> {
    register_content_type(vid, content_type);
    let payload = serde_json::to_vec(&crate::params::single_model_payload(vid, content_type))
        .map_err(|error| AppError::Media(error.to_string()))?;
    let bytes = crate::client::api_call_playback_v5(
        crate::client::SINFONLINEA_API_ORIGIN,
        crate::params::SINGLE_MODEL_PATH,
        payload,
        env,
    )
    .await?;
    let value = parse_model_response(&bytes)?.ok_or_else(|| {
        AppError::EmptyResponse(format!("取流空响应 vid={vid}（ct={content_type}）"))
    })?;
    let raw = value
        .pointer(&format!("/data/{vid}/video_model"))
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/data/video_model").and_then(Value::as_str))
        .ok_or_else(|| AppError::StreamMiss(format!("响应没有 video_model vid={vid}")))?;
    serde_json::from_str(raw)
        .map_err(|e| AppError::Media(format!("video_model 解析失败: {e}")))
}

'''
    s = s.replace(anchor, add + anchor, 1) if anchor else s + "\n" + add
    p.write_text(s)
print("patched reference crate")

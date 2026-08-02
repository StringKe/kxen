//! Provider 转写引擎（OpenAI 兼容 /v1/audio/transcriptions）。API key 存 auth.json（id: voice:<provider>），无凭证明示未配置。

use super::EngineStatus;
use crate::auth::credential::{AuthStore, CredentialKind};

pub const PROVIDERS: &[(&str, &str, &str)] = &[
    ("openai", "OpenAI 转写", "https://api.openai.com/v1/audio/transcriptions"),
    ("xai", "xAI 转写", "https://api.x.ai/v1/audio/transcriptions"),
];

fn store_key(provider: &str) -> String {
    format!("voice:{provider}")
}

/// 写入 provider 转写 API key（设置页语音区）。
pub fn set_key(store: &mut AuthStore, provider: &str, key: &str, path: &std::path::Path) -> Result<(), String> {
    if !PROVIDERS.iter().any(|(id, _, _)| *id == provider) {
        return Err(format!("未知转写 provider: {provider}"));
    }
    store.insert(store_key(provider), CredentialKind::Api { key: key.to_string(), region: None });
    crate::auth::credential::write_auth_file(path, store).map_err(|e| e.to_string())
}

pub(crate) fn configured(store: &crate::auth::credential::AuthStore, provider: &str) -> bool {
    if provider.starts_with("custom:") {
        return matches!(store.get(provider), Some(CredentialKind::Api { .. }));
    }
    matches!(store.get(&store_key(provider)), Some(CredentialKind::Api { .. }))
}

pub fn statuses(config: &crate::core::config::Config, store: &AuthStore) -> Vec<EngineStatus> {
    let mut out: Vec<EngineStatus> = PROVIDERS
        .iter()
        .map(|(id, label, _)| {
            let has_key = configured(store, id);
            EngineStatus {
                id: id.to_string(),
                label: label.to_string(),
                status: if has_key { "ready" } else { "unconfigured" }.into(),
                detail: if has_key { "API key 已配置" } else { "未配置 API key（设置页语音区添加）" }.into(),
            }
        })
        .collect();
    // audio 标记的自定义提供商并入转写引擎
    for (name, def) in &config.custom_providers {
        if !def.capabilities.iter().any(|c| c == "audio") {
            continue;
        }
        let id = format!("custom:{name}");
        let has_key = configured(store, &id);
        out.push(EngineStatus {
            id: id.clone(),
            label: format!("{name} 转写（自定义）"),
            status: if has_key { "ready" } else { "unconfigured" }.into(),
            detail: if has_key { def.base_url.clone() } else { "未配置 API key".into() },
        });
    }
    out
}

/// multipart 上传整文件 -> 转写文本。
pub async fn transcribe_file(
    config: &crate::core::config::Config,
    store: &AuthStore,
    provider: &str,
    path: &str,
) -> Result<String, String> {
    // 自定义提供商（audio 标记）：端点 = base_url + /audio/transcriptions，key 直取 custom:<name>
    let (label, url, key) = if let Some(name) = provider.strip_prefix("custom:") {
        let def = config.custom_providers.get(name).ok_or_else(|| format!("自定义提供商未配置: {name}"))?;
        if !def.capabilities.iter().any(|c| c == "audio") {
            return Err(format!("自定义提供商 {name} 未标记 audio 能力"));
        }
        let Some(CredentialKind::Api { key, .. }) = store.get(provider) else {
            return Err(format!("{name} 未配置 API key"));
        };
        (name.to_string(), format!("{}/audio/transcriptions", def.base_url.trim_end_matches('/')), key.clone())
    } else {
        let (_, label, url) =
            PROVIDERS.iter().find(|(id, _, _)| *id == provider).ok_or_else(|| format!("未知转写 provider: {provider}"))?;
        let Some(CredentialKind::Api { key, .. }) = store.get(&store_key(provider)) else {
            return Err(format!("{label}未配置 API key"));
        };
        (label.to_string(), url.to_string(), key.clone())
    };
    let bytes = std::fs::read(path).map_err(|e| format!("读取音频失败: {e}"))?;
    let file_name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("audio.wav").to_string();
    let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
    let form = reqwest::multipart::Form::new().text("model", config.voice.transcribe_model.clone()).part("file", part);
    let resp = crate::llm::client::shared_http()
        .post(&url)
        .header("authorization", format!("Bearer {key}"))
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("转写请求失败: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(crate::llm::client::format_http_error(&label, status, &body));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| format!("转写响应解析失败: {e}"))?;
    v.get("text").and_then(|t| t.as_str()).map(String::from).ok_or_else(|| "转写响应缺少 text 字段".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_shows_explicitly() {
        let store = AuthStore::new();
        let statuses = statuses(&crate::core::config::Config::default(), &store);
        assert!(statuses.iter().all(|s| s.status == "unconfigured"));
        assert!(statuses.iter().all(|s| s.detail.contains("未配置")));
    }

    #[test]
    fn configured_is_ready() {
        let mut store = AuthStore::new();
        store.insert(store_key("xai"), CredentialKind::Api { key: "k".into(), region: None });
        let statuses = statuses(&crate::core::config::Config::default(), &store);
        let xai = statuses.iter().find(|s| s.id == "xai").unwrap();
        assert_eq!(xai.status, "ready");
    }
}

// ---------------- 麦克风录音会话（停止后整段上传转写） ----------------

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use std::sync::{Arc, Mutex};

pub struct RecordSession {
    engine: Retained<AnyObject>,
    /// tap block：stop/cancel 先 removeTap 再随结构体回收（mem::forget 会每次 PTT 泄漏一份）
    tap: super::objc::TapHandler,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
}

/// 启动录音（PTT 按下）。tap 回调把 PCM 累积进共享缓冲。
pub fn start_recording() -> Result<RecordSession, String> {
    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = samples.clone();
    let (engine, rate, tap) = super::objc::start_mic_capture(move |_input| {
        super::objc::TapHandler::new(move |buffer, _time| {
            let chunk = unsafe { super::objc::pcm_samples(buffer) };
            if !chunk.is_empty() {
                crate::core::shared::lock(&sink).extend_from_slice(&chunk);
            }
        })
    })?;
    Ok(RecordSession { engine, tap, samples, sample_rate: rate as u32 })
}

impl RecordSession {
    /// PTT 松开：停止 -> 写 WAV 到临时文件 -> 返回 (path, 时长秒)。
    pub fn stop(self) -> Result<(String, f32), String> {
        super::objc::stop_mic_engine(&self.engine);
        drop(self.tap); // removeTap 之后回收 tap block
        let samples = std::mem::take(&mut *crate::core::shared::lock(&self.samples));
        if samples.is_empty() {
            return Err("未录到音频".into());
        }
        let path = temp_wav_path();
        let path_str = path.to_string_lossy().into_owned();
        write_wav(&path, &samples, self.sample_rate).map_err(|e| format!("写 WAV 失败: {e}"))?;
        Ok((path_str, samples.len() as f32 / self.sample_rate as f32))
    }

    /// Session 被删除或同 Session 重启 PTT 时停止录音且丢弃未提交样本。
    pub fn cancel(self) {
        super::objc::stop_mic_engine(&self.engine);
        drop(self.tap);
    }
}

/// 云转写临时 WAV 路径：pid + 原子序号 + 纳秒时间戳。
/// 只按 pid 命名会让多会话并发 stop 写同一路径，互相覆盖、还互相误删。
pub(crate) fn temp_wav_path() -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    std::env::temp_dir().join(format!(
        "kxen-voice-{}-{}-{nanos}.wav",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// 16-bit PCM 单声道 WAV（f32 -> i16 截断）。
pub(crate) fn write_wav_pub(path: &std::path::Path, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    write_wav(path, samples, sample_rate)
}

/// 16-bit PCM 单声道 WAV（f32 -> i16 截断）。
fn write_wav(path: &std::path::Path, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 2) as u32;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&(sample_rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        f.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod wav_tests {
    #[test]
    fn wav_header_well_formed() {
        let dir = std::env::temp_dir().join(format!("kxen-wav-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        super::write_wav(&path, &[0.0, 0.5, -0.5], 16000).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(bytes.len(), 44 + 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn temp_wav_path_unique_per_call() {
        // 多会话并发 stop 各写各的临时文件：同名会互相覆盖 + 互相误删
        let a = super::temp_wav_path();
        let b = super::temp_wav_path();
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("kxen-voice-"));
        assert!(a.extension().is_some_and(|e| e == "wav"));
    }
}

//! Provider 转写引擎（OpenAI 兼容 /v1/audio/transcriptions）。API key 存 auth.json（id: voice:<provider>），无凭证明示未配置。

use super::EngineStatus;
use crate::auth::credential::{AuthStore, CredentialKind};

pub(crate) const MAX_AUDIO_SECONDS: usize = 300;
pub(crate) const MAX_CAPTURE_SAMPLE_RATE: usize = 192_000;
pub(crate) const MAX_PCM_SAMPLES: usize = MAX_CAPTURE_SAMPLE_RATE * MAX_AUDIO_SECONDS;
pub(crate) const MAX_WAV_BYTES: usize = 44 + MAX_PCM_SAMPLES * 2;

pub(crate) fn sample_limit(sample_rate: u32) -> usize {
    usize::try_from(sample_rate).unwrap_or(usize::MAX).saturating_mul(MAX_AUDIO_SECONDS).min(MAX_PCM_SAMPLES)
}

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
    let outcome = crate::auth::credential::update_auth_file_committed(path, |disk| {
        disk.insert(store_key(provider), CredentialKind::Api { key: key.to_string(), region: None });
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    let (persisted, warning) = outcome.into_snapshot_and_warning();
    *store = persisted;
    match warning {
        Some(error) => Err(error),
        None => Ok(()),
    }
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
            detail: if has_key { crate::core::net_security::safe_endpoint_label(&def.base_url) } else { "未配置 API key".into() },
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
    mrm: &crate::llm::mrm::ModelResourceManager,
    usage_reporter: &crate::agent::agent_loop::UsageReporter,
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
        let url = crate::core::net_security::join_base_endpoint(&def.base_url, "audio/transcriptions")?;
        (name.to_string(), url, key.clone())
    } else {
        let (_, label, url) =
            PROVIDERS.iter().find(|(id, _, _)| *id == provider).ok_or_else(|| format!("未知转写 provider: {provider}"))?;
        let Some(CredentialKind::Api { key, .. }) = store.get(&store_key(provider)) else {
            return Err(format!("{label}未配置 API key"));
        };
        (label.to_string(), url.to_string(), key.clone())
    };
    let bytes = read_bounded_wav(std::path::Path::new(path))?;
    let file_name = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("audio.wav").to_string();
    let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
    let form = reqwest::multipart::Form::new().text("model", config.voice.transcribe_model.clone()).part("file", part);
    let mut attempt = match usage_reporter.begin(None) {
        Ok(attempt) => attempt,
        Err(error) => return Err(format!("转写请求未发送：无法持久化用量声明: {error}")),
    };
    let permit = match mrm.begin_call(provider, None).await {
        Ok(permit) => permit,
        Err(error) => {
            usage_reporter
                .discard_unstarted(&attempt)
                .map_err(|cleanup| format!("转写 MRM admission 失败: {error}; 用量声明清理失败: {cleanup}"))?;
            return Err(format!("转写 MRM admission 失败，请求未发送: {error}"));
        }
    };
    if let Err(error) = usage_reporter.mark_started(&mut attempt) {
        drop(permit);
        return Err(format!("转写请求未发送：无法持久化 Started 边界: {error}"));
    }
    let slot = permit.start();
    let response = crate::llm::client::shared_http_for_url(&url)
        .post(&url)
        .header("authorization", format!("Bearer {key}"))
        .multipart(form)
        .send()
        .await;
    let result = match response {
        Err(error) => Err(format!("转写请求失败: {}", crate::core::net_security::sanitize_authenticated_error(&error, &[&key]))),
        Ok(response) if !response.status().is_success() => Err(crate::llm::client::bounded_http_error(&label, response, &[&key]).await),
        Ok(response) => {
            match crate::net_response::json::<serde_json::Value>(response, crate::net_response::JSON_BODY_LIMIT, "transcription response")
                .await
            {
                Ok(value) => {
                    value.get("text").and_then(|text| text.as_str()).map(String::from).ok_or_else(|| "转写响应缺少 text 字段".to_string())
                }
                Err(error) => Err(format!("转写响应解析失败: {error}")),
            }
        }
    };
    let outcome = if result.is_ok() { crate::llm::mrm::CallOutcome::Success } else { crate::llm::mrm::CallOutcome::Failure };
    mrm.record_call_outcome(provider, Some(&slot), outcome).await;
    settle_transcription(result, usage_reporter, &attempt)
}

fn settle_transcription(
    result: Result<String, String>,
    reporter: &crate::agent::agent_loop::UsageReporter,
    attempt: &crate::core::usage::ProviderAttempt,
) -> Result<String, String> {
    match reporter.settle(attempt) {
        Ok(outcome) => match (result, outcome.stop_message) {
            (Ok(_), Some(stop)) => Err(stop),
            (result, None) => result,
            (Err(error), Some(stop)) => Err(format!("{error}\n{stop}")),
        },
        Err(metering) => Err(match result {
            Ok(_) => format!("转写完成但用量持久化失败: {metering}"),
            Err(error) => format!("{error}\n转写用量持久化失败: {metering}"),
        }),
    }
}

fn read_bounded_wav(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| format!("读取音频元数据失败: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("音频路径不是普通文件".into());
    }
    if metadata.len() > MAX_WAV_BYTES as u64 {
        return Err(format!("音频文件超过 {} bytes 上限", MAX_WAV_BYTES));
    }
    let file = std::fs::File::open(path).map_err(|error| format!("读取音频失败: {error}"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_WAV_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|error| format!("读取音频失败: {error}"))?;
    if bytes.len() > MAX_WAV_BYTES {
        return Err(format!("音频文件超过 {} bytes 上限", MAX_WAV_BYTES));
    }
    Ok(bytes)
}

// ---------------- 麦克风录音会话（停止后整段上传转写） ----------------

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct SampleBuffer {
    pub(crate) samples: Vec<f32>,
    pub(crate) exceeded: bool,
}

pub(crate) fn append_samples(buffer: &mut SampleBuffer, chunk: &[f32], limit: usize) {
    append_samples_up_to(buffer, chunk, limit.min(MAX_PCM_SAMPLES));
}

fn append_samples_up_to(buffer: &mut SampleBuffer, chunk: &[f32], limit: usize) {
    if buffer.exceeded || chunk.is_empty() {
        return;
    }
    let remaining = limit.saturating_sub(buffer.samples.len());
    buffer.samples.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    buffer.exceeded = chunk.len() > remaining;
}

pub struct RecordSession {
    engine: Retained<AnyObject>,
    /// tap block：stop/cancel 先 removeTap 再随结构体回收（mem::forget 会每次 PTT 泄漏一份）
    tap: super::objc::TapHandler,
    samples: Arc<Mutex<SampleBuffer>>,
    sample_rate: u32,
}

/// 启动录音（PTT 按下）。tap 回调把 PCM 累积进共享缓冲。
pub fn start_recording() -> Result<RecordSession, String> {
    let samples: Arc<Mutex<SampleBuffer>> = Arc::new(Mutex::new(SampleBuffer::default()));
    let sink = samples.clone();
    let sample_limit = Arc::new(std::sync::atomic::AtomicUsize::new(MAX_PCM_SAMPLES));
    let callback_limit = sample_limit.clone();
    let (engine, rate, tap) = super::objc::start_mic_capture(move |_input| {
        super::objc::TapHandler::new(move |buffer, _time| {
            let chunk = unsafe { super::objc::pcm_samples(buffer) };
            if !chunk.is_empty() {
                append_samples(&mut crate::core::shared::lock(&sink), &chunk, callback_limit.load(std::sync::atomic::Ordering::Relaxed));
            }
        })
    })?;
    sample_limit.store(self::sample_limit(rate as u32), std::sync::atomic::Ordering::Relaxed);
    Ok(RecordSession { engine, tap, samples, sample_rate: rate as u32 })
}

impl RecordSession {
    /// PTT 松开：停止 -> 写 WAV 到临时文件 -> 返回 (path, 时长秒)。
    pub fn stop(self) -> Result<(String, f32), String> {
        super::objc::stop_mic_engine(&self.engine);
        drop(self.tap); // removeTap 之后回收 tap block
        let samples = std::mem::take(&mut *crate::core::shared::lock(&self.samples));
        if samples.exceeded {
            return Err(format!("录音超过 {MAX_AUDIO_SECONDS} 秒或 PCM 缓冲上限，已安全停止且不会上传"));
        }
        if samples.samples.is_empty() {
            return Err("未录到音频".into());
        }
        let path = temp_wav_path();
        let path_str = path.to_string_lossy().into_owned();
        write_wav(&path, &samples.samples, self.sample_rate).map_err(|e| format!("写 WAV 失败: {e}"))?;
        Ok((path_str, samples.samples.len() as f32 / self.sample_rate as f32))
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
    if sample_rate == 0 || samples.len() > MAX_PCM_SAMPLES || samples.len() as u64 > sample_rate as u64 * MAX_AUDIO_SECONDS as u64 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "audio exceeds PCM or duration limit"));
    }
    let data_len =
        u32::try_from(samples.len() * 2).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "WAV payload too large"))?;
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
#[path = "provider/tests.rs"]
mod tests;

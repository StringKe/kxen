//! 自写 SSE 解析（~120 行，pi_agent_rust 模式）。
//! 输入：任意字节流的增量；输出：完整的 SSE data 载荷帧。
//! 处理：行缓冲、跨块行拼接、`data:` 前缀、心跳注释（`:` 开头）、`[DONE]`。

const MAX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
pub struct SseParser {
    /// 未完成的行残片（跨 chunk）
    pending: Vec<u8>,
    failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseFrame {
    /// `data: ...` 载荷（不含前缀）
    Data(String),
    /// `data: [DONE]`
    Done,
    /// 完整 SSE 行不是合法 UTF-8。
    Invalid(String),
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一块字节流，返回本块解析出的完整帧。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        if self.failed {
            return Vec::new();
        }
        let mut frames = Vec::new();
        let mut remaining = chunk;
        while !remaining.is_empty() {
            let take = remaining.iter().position(|byte| *byte == b'\n').map_or(remaining.len(), |index| index + 1);
            if self.pending.len().saturating_add(take) > MAX_LINE_BYTES {
                self.pending.clear();
                self.failed = true;
                frames.push(SseFrame::Invalid(format!("sse line exceeded {MAX_LINE_BYTES} byte limit")));
                break;
            }
            self.pending.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.pending.last() == Some(&b'\n') {
                let mut line = std::mem::take(&mut self.pending);
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(frame) = parse_bytes(line) {
                    frames.push(frame);
                }
            }
        }
        frames
    }

    /// 流结束时冲刷（残片若构成完整 data 行也产出）。
    pub fn finish(&mut self) -> Vec<SseFrame> {
        if self.failed {
            return Vec::new();
        }
        let mut line = std::mem::take(&mut self.pending);
        while matches!(line.last(), Some(b'\r' | b'\n')) {
            line.pop();
        }
        parse_bytes(line).into_iter().collect()
    }
}

pub enum Projection {
    Ignore,
    Delta(crate::llm::types::Delta),
    Complete(Option<crate::llm::types::Delta>),
}

/// 各 provider 共用的流式管线。只有协议显式完成事件才能产生 Done；传输 EOF、
/// 非法 UTF-8 和读错误都产生 Error，不能把截断响应记成成功。
pub fn stream_deltas(
    resp: reqwest::Response,
    map: impl FnMut(SseFrame) -> Projection + Send + 'static,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = crate::llm::types::Delta> + Send>> {
    use futures::StreamExt;
    let bytes = Box::pin(resp.bytes_stream());
    let initial = (bytes, SseParser::new(), map, std::collections::VecDeque::new(), false);
    Box::pin(futures::stream::unfold(initial, |(mut bytes, mut parser, mut map, mut queued, mut finished)| async move {
        loop {
            if let Some(delta) = queued.pop_front() {
                return Some((delta, (bytes, parser, map, queued, finished)));
            }
            if finished {
                return None;
            }
            match bytes.next().await {
                Some(Ok(chunk)) => project(parser.feed(&chunk), &mut map, &mut queued, &mut finished),
                Some(Err(error)) => {
                    queued.push_back(crate::llm::types::Delta::Error(format!("sse read: {error}")));
                    finished = true;
                }
                None => {
                    project(parser.finish(), &mut map, &mut queued, &mut finished);
                    if !finished {
                        queued.push_back(crate::llm::types::Delta::Error("sse stream ended before protocol completion".into()));
                        finished = true;
                    }
                }
            }
        }
    }))
}

fn project(
    frames: Vec<SseFrame>,
    map: &mut impl FnMut(SseFrame) -> Projection,
    queued: &mut std::collections::VecDeque<crate::llm::types::Delta>,
    finished: &mut bool,
) {
    for frame in frames {
        if let SseFrame::Invalid(error) = frame {
            queued.push_back(crate::llm::types::Delta::Error(error));
            *finished = true;
            break;
        }
        match map(frame) {
            Projection::Ignore => {}
            Projection::Delta(delta) => queued.push_back(delta),
            Projection::Complete(delta) => {
                queued.extend(delta);
                queued.push_back(crate::llm::types::Delta::Done);
                *finished = true;
                break;
            }
        }
    }
}

fn parse_bytes(line: Vec<u8>) -> Option<SseFrame> {
    match String::from_utf8(line) {
        Ok(line) => parse_line(&line),
        Err(error) => Some(SseFrame::Invalid(format!("sse invalid utf-8: {error}"))),
    }
}

fn parse_line(line: &str) -> Option<SseFrame> {
    if line.is_empty() || line.starts_with(':') {
        return None; // 空行分隔 / 心跳注释
    }
    let data = line.strip_prefix("data:")?.trim_start();
    if data == "[DONE]" { Some(SseFrame::Done) } else { Some(SseFrame::Data(data.to_string())) }
}

pub fn payload_error(provider: &str, data: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let error = value.get("error").or_else(|| value.pointer("/response/error"))?;
    let kind = error.get("type").or_else(|| error.get("code")).and_then(serde_json::Value::as_str);
    let message = error.get("message").and_then(serde_json::Value::as_str).or_else(|| error.as_str());
    let detail = match (kind, message) {
        (Some(kind), Some(message)) => format!("{kind} - {message}"),
        (_, Some(message)) => message.to_string(),
        _ => error.to_string(),
    };
    Some(format!("{provider} stream error: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frames_across_chunks() {
        let mut p = SseParser::new();
        let mut frames = p.feed(b"data: {\"a\":1}\n\nda");
        frames.extend(p.feed(b"ta: {\"b\":2}\n"));
        frames.extend(p.feed(b"data: [DONE]\n"));
        let datas: Vec<_> = frames
            .iter()
            .filter_map(|f| match f {
                SseFrame::Data(d) => Some(d.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(datas, vec!["{\"a\":1}", "{\"b\":2}"]);
        assert!(frames.iter().any(|f| matches!(f, SseFrame::Done)));
    }

    #[test]
    fn skips_heartbeat_and_empty() {
        let mut p = SseParser::new();
        let frames = p.feed(b": ping\n\n\ndata: x\n");
        assert_eq!(frames, vec![SseFrame::Data("x".into())]);
    }

    #[test]
    fn preserves_multibyte_utf8_across_every_chunk_boundary() {
        let bytes = "data: 中文\n".as_bytes();
        for split in 1..bytes.len() {
            let mut parser = SseParser::new();
            let mut frames = parser.feed(&bytes[..split]);
            frames.extend(parser.feed(&bytes[split..]));
            assert_eq!(frames, vec![SseFrame::Data("中文".into())], "split={split}");
        }
    }

    #[test]
    fn invalid_utf8_is_not_lossily_replaced() {
        let mut parser = SseParser::new();
        let frames = parser.feed(b"data: \xff\n");
        assert!(matches!(frames.as_slice(), [SseFrame::Invalid(error)] if error.contains("invalid utf-8")));
    }

    #[test]
    fn oversized_line_fails_once_without_retaining_the_payload() {
        let mut parser = SseParser::new();
        let frames = parser.feed(&vec![b'x'; MAX_LINE_BYTES + 1]);
        assert!(matches!(frames.as_slice(), [SseFrame::Invalid(error)] if error.contains("exceeded")));
        assert!(parser.pending.is_empty());
        assert!(parser.feed(b"data: ignored\n").is_empty());
    }
}

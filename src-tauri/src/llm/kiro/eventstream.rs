//! AWS event stream 二进制帧解析（CodeWhisperer GenerateAssistantResponse 的响应协议）。
//! 帧布局：total_len u32BE | headers_len u32BE | prelude_crc u32 | headers | payload(JSON) | message_crc u32；
//! 两个 CRC 均为标准 CRC-32（IEEE，反射多项式 0xEDB88320）。帧契约对照 9router open-sse/executors/kiro.js
//! 的 parseEventFrame 翻译，含同样的边界与 CRC 校验（损坏帧无法重同步，直接报错终止）。

use serde_json::Value;

/// 单帧协议上限（同 9router）：防损坏长度字段触发巨量分配。
const MAX_MESSAGE_BYTES: usize = 24 * 1024 * 1024;
const MAX_HEADERS_BYTES: usize = 128 * 1024;
/// total_len + headers_len + prelude_crc。
const PRELUDE_BYTES: usize = 12;
const CRC_BYTES: usize = 4;

/// 一帧解码结果：只保留投影层关心的头（:message-type/:event-type/:error-code）与 JSON payload。
#[derive(Debug)]
pub(super) struct Event {
    pub message_type: String,
    pub event_type: String,
    pub error_code: String,
    pub payload: Option<Value>,
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

const CRC_TABLE: [u32; 256] = crc_table();

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc = CRC_TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn be_u32(frame: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(frame[offset..offset + 4].try_into().expect("u32 slice"))
}

/// 增量解码器：帧可跨 TCP 分片，buffer 攒够一帧才解析。
#[derive(Default)]
pub(super) struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub(super) fn feed(&mut self, chunk: &[u8]) -> Result<Vec<Event>, String> {
        if self.buffer.len() + chunk.len() > MAX_MESSAGE_BYTES {
            return Err("kiro eventstream buffered bytes exceed the protocol bound".into());
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while self.buffer.len() >= PRELUDE_BYTES {
            if be_u32(&self.buffer, 8) != crc32(&self.buffer[..8]) {
                return Err("kiro eventstream prelude CRC mismatch".into());
            }
            let total = be_u32(&self.buffer, 0) as usize;
            let headers_len = be_u32(&self.buffer, 4) as usize;
            if total < PRELUDE_BYTES + CRC_BYTES
                || total > MAX_MESSAGE_BYTES
                || headers_len > MAX_HEADERS_BYTES
                || headers_len > total - PRELUDE_BYTES - CRC_BYTES
            {
                return Err("kiro eventstream frame bounds are invalid".into());
            }
            if self.buffer.len() < total {
                break;
            }
            let frame: Vec<u8> = self.buffer.drain(..total).collect();
            events.push(parse_frame(&frame)?);
        }
        Ok(events)
    }

    /// 传输 EOF 时调用：残留字节即截断帧（协议未完成，必须报错）。
    pub(super) fn finish(&mut self) -> Result<(), String> {
        if self.buffer.is_empty() { Ok(()) } else { Err("kiro eventstream ended with a truncated frame".into()) }
    }
}

fn parse_frame(frame: &[u8]) -> Result<Event, String> {
    let total = frame.len();
    let headers_len = be_u32(frame, 4) as usize;
    if be_u32(frame, total - CRC_BYTES) != crc32(&frame[..total - CRC_BYTES]) {
        return Err("kiro eventstream message CRC mismatch".into());
    }
    let mut event = Event { message_type: String::new(), event_type: String::new(), error_code: String::new(), payload: None };
    let mut cursor = Cursor { frame, offset: PRELUDE_BYTES, end: PRELUDE_BYTES + headers_len };
    while cursor.offset < cursor.end {
        let name = cursor.read_name()?;
        let value = cursor.read_value()?;
        match name.as_str() {
            ":message-type" => event.message_type = value,
            ":event-type" => event.event_type = value,
            ":error-code" => event.error_code = value,
            _ => {}
        }
    }
    let payload = &frame[cursor.end..total - CRC_BYTES];
    let text = std::str::from_utf8(payload).map_err(|_| "kiro eventstream payload is not UTF-8".to_string())?;
    if !text.trim().is_empty() {
        event.payload = Some(serde_json::from_str(text).map_err(|error| format!("kiro eventstream payload is not valid JSON: {error}"))?);
    }
    Ok(event)
}

/// 头区游标：name_len u8 + name + type u8 + value（类型 0-9，全部按 AWS 契约跳过或读取）。
struct Cursor<'a> {
    frame: &'a [u8],
    offset: usize,
    end: usize,
}

impl Cursor<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], String> {
        if self.offset + count > self.end {
            return Err("kiro eventstream header exceeds its declared bounds".into());
        }
        let slice = &self.frame[self.offset..self.offset + count];
        self.offset += count;
        Ok(slice)
    }

    fn read_name(&mut self) -> Result<String, String> {
        let len = usize::from(self.take(1)?[0]);
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "kiro eventstream header name is not UTF-8".into())
    }

    /// 返回字符串值（type 7）；其余类型按长度跳过并返回空串。
    fn read_value(&mut self) -> Result<String, String> {
        let kind = self.take(1)?[0];
        let fixed = match kind {
            0 | 1 => 0,          // bool true/false
            2 => 1,              // byte
            3 => 2,              // short
            4 => 4,              // integer
            5 | 8 => 8,          // long / timestamp
            9 => 16,             // uuid
            6 | 7 => usize::MAX, // bytes / string：u16 长度前缀
            other => return Err(format!("kiro eventstream header has unknown type {other}")),
        };
        if fixed != usize::MAX {
            self.take(fixed)?;
            return Ok(String::new());
        }
        let len = u16::from_be_bytes(self.take(2)?.try_into().expect("u16 slice")) as usize;
        let bytes = self.take(len)?;
        if kind == 7 {
            String::from_utf8(bytes.to_vec()).map_err(|_| "kiro eventstream header value is not UTF-8".into())
        } else {
            Ok(String::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试帧构造：:message-type / :event-type 两个字符串头 + JSON payload。
    fn frame(message_type: &str, event_type: &str, payload: &str) -> Vec<u8> {
        let mut headers = Vec::new();
        for (name, value) in [(":message-type", message_type), (":event-type", event_type)] {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7);
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }
        let total = (PRELUDE_BYTES + headers.len() + payload.len() + CRC_BYTES) as u32;
        let mut frame = Vec::new();
        frame.extend_from_slice(&total.to_be_bytes());
        frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        frame.extend_from_slice(&crc32(&frame).to_be_bytes());
        frame.extend_from_slice(&headers);
        frame.extend_from_slice(payload.as_bytes());
        let crc = crc32(&frame);
        frame.extend_from_slice(&crc.to_be_bytes());
        frame
    }

    #[test]
    fn decodes_single_frame_with_headers_and_payload() {
        let bytes = frame("event", "assistantResponseEvent", r#"{"content":"你好"}"#);
        let events = FrameDecoder::default().feed(&bytes).expect("valid frame");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message_type, "event");
        assert_eq!(events[0].event_type, "assistantResponseEvent");
        assert_eq!(events[0].payload.as_ref().and_then(|p| p.get("content")).and_then(Value::as_str), Some("你好"));
    }

    #[test]
    fn frame_split_across_feeds_decodes_once_complete() {
        let bytes = frame("event", "assistantResponseEvent", r#"{"content":"ab"}"#);
        let mut decoder = FrameDecoder::default();
        let mid = bytes.len() / 2;
        assert!(decoder.feed(&bytes[..mid]).expect("prefix").is_empty(), "半帧不得产出事件");
        let events = decoder.feed(&bytes[mid..]).expect("suffix");
        assert_eq!(events.len(), 1);
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn multiple_frames_in_one_chunk_all_decode() {
        let one = frame("event", "assistantResponseEvent", r#"{"content":"a"}"#);
        let two = frame("event", "messageStopEvent", r#"{"stopReason":"end_turn"}"#);
        let mut joined = one;
        joined.extend_from_slice(&two);
        let events = FrameDecoder::default().feed(&joined).expect("two frames");
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].event_type, "messageStopEvent");
    }

    #[test]
    fn corrupt_prelude_crc_is_rejected() {
        let mut bytes = frame("event", "x", "{}");
        bytes[0] ^= 0xFF;
        let error = FrameDecoder::default().feed(&bytes).expect_err("corrupt prelude must fail");
        assert!(error.contains("prelude CRC"), "{error}");
    }

    #[test]
    fn corrupt_message_crc_is_rejected() {
        let mut bytes = frame("event", "x", "{}");
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let error = FrameDecoder::default().feed(&bytes).expect_err("corrupt message crc must fail");
        assert!(error.contains("message CRC"), "{error}");
    }

    #[test]
    fn truncated_frame_at_eof_is_an_error() {
        let bytes = frame("event", "x", "{}");
        let mut decoder = FrameDecoder::default();
        assert!(decoder.feed(&bytes[..bytes.len() - 2]).expect("prefix").is_empty());
        assert!(decoder.finish().expect_err("leftover bytes must fail").contains("truncated"));
    }

    #[test]
    fn error_frame_surfaces_message_type_and_error_code() {
        let bytes = frame("exception", "", r#"{"message":"throttled"}"#);
        let events = FrameDecoder::default().feed(&bytes).expect("error frame");
        assert_eq!(events[0].message_type, "exception");
        assert_eq!(events[0].payload.as_ref().and_then(|p| p.get("message")).and_then(Value::as_str), Some("throttled"));
    }
}

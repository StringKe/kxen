//! SSE（text/event-stream）增量帧解析：streamable http 与 legacy sse 两种 remote transport 共用。
//! 独立成纯函数模块：帧边界、CRLF、多行 data 拼接、多字节字符跨 chunk 都是易错点，必须可单测。

const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_EVENT_DATA_BYTES: usize = 1024 * 1024;

/// 一条完整 SSE 事件（空行 dispatch 时产出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// event: 字段；缺省 None（按 W3C 语义即 "message"）
    pub event: Option<String>,
    /// 多行 data: 以 \n 拼接
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    // 字节级缓冲：多字节 UTF-8 可能跨 TCP chunk 切开，按 \n 取整行后再解码才不会出替换符
    buf: Vec<u8>,
    event: Option<String>,
    data: String,
    has_data: bool,
    failed: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入任意字节块，返回本轮凑齐的全部事件。
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, String> {
        if self.failed {
            return Err("SSE parser is closed after a prior size or encoding violation".into());
        }
        let mut out = Vec::new();
        let mut remaining = chunk;
        while !remaining.is_empty() {
            let take = remaining.iter().position(|byte| *byte == b'\n').map_or(remaining.len(), |index| index + 1);
            if self.buf.len().saturating_add(take) > MAX_LINE_BYTES {
                return self.fail(format!("MCP SSE line exceeded {MAX_LINE_BYTES} byte limit"));
            }
            self.buf.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.buf.last() != Some(&b'\n') {
                continue;
            }
            let mut line = std::mem::take(&mut self.buf);
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(line, &mut out)?;
        }
        Ok(out)
    }

    fn process_line(&mut self, line: Vec<u8>, out: &mut Vec<SseEvent>) -> Result<(), String> {
        let line = String::from_utf8(line).map_err(|error| self.fail_message(format!("MCP SSE invalid UTF-8: {error}")))?;
        if line.is_empty() {
            self.dispatch(out);
            return Ok(());
        }
        // 注释行（含心跳 ":ping"）按规范忽略
        if line.starts_with(':') {
            return Ok(());
        }
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.strip_prefix(' ').unwrap_or(data);
            let separator = usize::from(self.has_data);
            if self.data.len().saturating_add(separator).saturating_add(data.len()) > MAX_EVENT_DATA_BYTES {
                return Err(self.fail_message(format!("MCP SSE event exceeded {MAX_EVENT_DATA_BYTES} byte data limit")));
            }
            if self.has_data {
                self.data.push('\n');
            }
            self.data.push_str(data);
            self.has_data = true;
        } else if let Some(event) = line.strip_prefix("event:") {
            self.event = Some(event.strip_prefix(' ').unwrap_or(event).to_string());
        }
        // id:/retry:/未知字段按规范忽略
        Ok(())
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        // 只有 event 没有 data 的块不是有效事件，丢弃但清状态防串帧
        if self.has_data {
            out.push(SseEvent { event: self.event.take(), data: std::mem::take(&mut self.data) });
            self.has_data = false;
        }
        self.event = None;
    }

    fn fail<T>(&mut self, message: String) -> Result<T, String> {
        Err(self.fail_message(message))
    }

    fn fail_message(&mut self, message: String) -> String {
        self.buf.clear();
        self.data.clear();
        self.event = None;
        self.has_data = false;
        self.failed = true;
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_and_multi_events() {
        let mut p = SseParser::new();
        let out = p.feed(b"data: {\"a\":1}\n\ndata: second\n\n").unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], SseEvent { event: None, data: "{\"a\":1}".into() });
        assert_eq!(out[1].data, "second");
    }

    #[test]
    fn joins_multi_line_data_and_captures_event() {
        let mut p = SseParser::new();
        let out = p.feed(b"event: message\ndata: line1\ndata: line2\n\n").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event.as_deref(), Some("message"));
        assert_eq!(out[0].data, "line1\nline2", "多行 data 以 \\n 拼接");
    }

    #[test]
    fn handles_crlf_and_comments() {
        let mut p = SseParser::new();
        let out = p.feed(b":ping\r\nevent: endpoint\r\ndata: /msg\r\n\r\n").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event.as_deref(), Some("endpoint"));
        assert_eq!(out[0].data, "/msg", "CRLF 不得残留 \\r");
    }

    #[test]
    fn tolerates_split_chunks_and_multibyte() {
        let mut p = SseParser::new();
        // "汉" 三字节跨两次 feed 切开：按行缓冲解码不得出替换符
        let frame = "data: 汉字\n\n";
        let bytes = frame.as_bytes();
        let cut = "data: 汉".len() - 1;
        assert!(p.feed(&bytes[..cut]).unwrap().is_empty(), "未凑齐整行不得产出");
        let out = p.feed(&bytes[cut..]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, "汉字");
    }

    #[test]
    fn drops_event_without_data() {
        let mut p = SseParser::new();
        assert!(p.feed(b"event: message\n\n").unwrap().is_empty());
        // 状态必须已清掉，下一帧不串
        let out = p.feed(b"data: ok\n\n").unwrap();
        assert_eq!(out[0].event, None);
        assert_eq!(out[0].data, "ok");
    }

    #[test]
    fn rejects_oversized_lines_and_multi_line_events() {
        let mut line = SseParser::new();
        assert!(line.feed(&vec![b'x'; MAX_LINE_BYTES + 1]).unwrap_err().contains("line exceeded"));
        assert!(line.buf.is_empty());

        let mut event = SseParser::new();
        // prefix 与换行也受单行上限约束；首行保持合法，再由第二行只触发事件总量上限。
        let first = format!("data: {}\n", "x".repeat(MAX_EVENT_DATA_BYTES - "data: ".len() - 1));
        event.feed(first.as_bytes()).unwrap();
        assert!(event.feed(b"data: 1234567\n").unwrap_err().contains("event exceeded"));
        assert!(event.data.is_empty());
    }
}

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub(super) const LIMIT: usize = 1024 * 1024;

/// Reads one newline-delimited JSON frame without allowing AsyncBufReadExt::lines
/// to grow an attacker-controlled String without bound.
pub(super) async fn next(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|error| format!("mcp stdio read: {error}"))?;
        if available.is_empty() {
            return if line.is_empty() { Ok(None) } else { Ok(Some(line)) };
        }
        let take = available.iter().position(|byte| *byte == b'\n').map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(take) > LIMIT {
            return Err(format!("mcp stdio frame exceeded {LIMIT} byte limit"));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_a_frame_without_newline_at_the_hard_limit() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, input) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move { writer.write_all(&vec![b'x'; LIMIT + 1]).await });
        let mut reader = tokio::io::BufReader::with_capacity(4096, input);
        let error = next(&mut reader).await.unwrap_err();
        assert!(error.contains("exceeded"));
        task.abort();
    }
}

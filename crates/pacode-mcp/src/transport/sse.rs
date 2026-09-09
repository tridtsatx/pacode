//! Minimal Server-Sent Events (SSE) parser for MCP Streamable HTTP.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

pub fn parse_sse_events(text: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut current_event = SseEvent::default();
    let mut current_data_lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !current_data_lines.is_empty() || current_event.event.is_some() {
                current_event.data = current_data_lines.join("\n");
                events.push(current_event);
                current_event = SseEvent::default();
                current_data_lines.clear();
            }
        } else if line.starts_with(':') {
            // SSE comment: ignored
            continue;
        } else if let Some(rest) = line.strip_prefix("data:") {
            let data = rest.strip_prefix(' ').unwrap_or(rest);
            current_data_lines.push(data);
        } else if let Some(rest) = line.strip_prefix("event:") {
            let ev = rest.strip_prefix(' ').unwrap_or(rest);
            current_event.event = Some(ev.to_string());
        } else if let Some(rest) = line.strip_prefix("id:") {
            let id = rest.strip_prefix(' ').unwrap_or(rest);
            current_event.id = Some(id.to_string());
        }
    }

    if !current_data_lines.is_empty() || current_event.event.is_some() {
        current_event.data = current_data_lines.join("\n");
        events.push(current_event);
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_event() {
        let raw = "event: message\ndata: {\"test\": 123}\n\n";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message"));
        assert_eq!(events[0].data, "{\"test\": 123}");
    }

    #[test]
    fn parse_multi_line_data() {
        let raw = "data: line 1\r\ndata: line 2\r\n\r\n";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line 1\nline 2");
    }

    #[test]
    fn parse_without_trailing_newline() {
        let raw = "data: {\"result\": 42}";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"result\": 42}");
    }

    #[test]
    fn parse_comments_ignored() {
        let raw = ": ping\n\ndata: hello\n\n";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }
}

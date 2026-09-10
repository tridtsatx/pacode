use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pacode_types::{AgentId, CallId, Mode, PermissionDecision, SessionId, WebConfig};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::Tool;
use crate::builtin::webfetch::extract_html_to_markdown;
use crate::host::{AgentSpec, PermissionDraft, ToolCtx, ToolHost, WaitOutcome};

#[derive(Default)]
struct DummyHost {
    schedule: crate::test_support::ScheduleStub,
}

#[async_trait]
impl ToolHost for DummyHost {
    async fn add_cron_job(
        &self,
        name: String,
        schedule: pacode_types::CronSchedule,
        prompt: String,
    ) -> Result<pacode_types::CronJob, ToolError> {
        self.schedule.add_cron_job(name, schedule, prompt)
    }

    fn list_cron_jobs(&self) -> Vec<pacode_types::CronJob> {
        self.schedule.list_cron_jobs()
    }

    async fn remove_cron_job(&self, id: &pacode_types::CronJobId) -> Result<(), ToolError> {
        self.schedule.remove_cron_job(id)
    }

    fn add_monitor(
        &self,
        label: String,
        condition: pacode_types::MonitorCondition,
        poll_interval_secs: Option<u64>,
    ) -> Result<pacode_types::MonitorInfo, ToolError> {
        self.schedule
            .add_monitor(label, condition, poll_interval_secs)
    }

    fn list_monitors(&self) -> Vec<pacode_types::MonitorInfo> {
        self.schedule.list_monitors()
    }

    fn stop_monitor(&self, id: &pacode_types::MonitorId) -> Result<(), ToolError> {
        self.schedule.stop_monitor(id)
    }

    async fn request_permission(&self, _draft: PermissionDraft) -> PermissionDecision {
        PermissionDecision::AllowOnce
    }
    async fn spawn_task(
        &self,
        _spec: pacode_exec::TaskSpec,
    ) -> Result<pacode_types::TaskId, ToolError> {
        Err(ToolError::failed("not implemented"))
    }
    fn task_info(&self, _task: &pacode_types::TaskId) -> Option<pacode_types::TaskInfo> {
        None
    }
    fn list_tasks(&self) -> Vec<pacode_types::TaskInfo> {
        Vec::new()
    }
    async fn wait_task(
        &self,
        _task: &pacode_types::TaskId,
        _timeout: Duration,
        _return_on_progress: bool,
    ) -> WaitOutcome {
        WaitOutcome::Finished
    }
    async fn kill_task(&self, _task: &pacode_types::TaskId) -> Result<(), ToolError> {
        Ok(())
    }
    async fn task_tail(
        &self,
        _task: &pacode_types::TaskId,
        _lines: usize,
    ) -> Result<Vec<String>, ToolError> {
        Ok(Vec::new())
    }
    fn report_task_progress(
        &self,
        _task: &pacode_types::TaskId,
        _progress: pacode_types::TaskProgress,
    ) -> Result<(), ToolError> {
        Ok(())
    }
    async fn spawn_agent(&self, _spec: AgentSpec) -> Result<AgentId, ToolError> {
        Err(ToolError::failed("not implemented"))
    }
    fn agent_info(&self, _agent: &AgentId) -> Option<pacode_types::AgentInfo> {
        None
    }
    fn list_agents(&self) -> Vec<pacode_types::AgentInfo> {
        Vec::new()
    }
    async fn wait_agent(&self, _agent: &AgentId, _timeout: Duration) -> WaitOutcome {
        WaitOutcome::Finished
    }
    fn request_agent_status(&self, _agent: &AgentId) -> Result<(), ToolError> {
        Ok(())
    }
    fn report_status(&self, _text: String) -> Result<(), ToolError> {
        Ok(())
    }
    async fn stop_agent(&self, _agent: &AgentId) -> Result<(), ToolError> {
        Ok(())
    }
    fn plan(&self) -> pacode_types::Plan {
        pacode_types::Plan::default()
    }
    fn set_plan(&self, _plan: pacode_types::Plan) {}
    fn emit_preview(&self, _call_id: &CallId, _preview: String) {}
    fn emit_notice(&self, _level: pacode_types::ToastLevel, _text: String) {}
}

fn dummy_ctx() -> ToolCtx {
    ToolCtx {
        session: SessionId::new("ses_test"),
        agent: AgentId::new("agent_test"),
        agent_name: "test_agent".to_string(),
        call_id: CallId::new("call_test"),
        cwd: std::env::current_dir().unwrap_or_default(),
        mode: Mode::Build,
        host: Arc::new(DummyHost::default()),
        cancel: CancellationToken::new(),
        output_cap_chars: 16_000,
        exec_yield_after: Duration::from_secs(10),
        exec_default_timeout: Duration::from_secs(30),
        tool_name: None,
        tool_kind: None,
    }
}

const DDG_RESULTS_FIXTURE: &str = r#"
<!DOCTYPE html>
<html>
<body>
<div class="results">
  <div class="result results_links results_links_deep web-result ">
    <h2 class="result__title">
      <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&rut=1">
        <b>Rust</b> Programming Language &amp; Tools
      </a>
    </h2>
    <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&rut=1">
      Empowering everyone to build <b>reliable</b> and &quot;efficient&quot; software. Fast &amp; memory-safe.
    </a>
  </div>

  <div class="result results_links results_links_deep web-result ">
    <h2 class="result__title">
      <a rel="nofollow" class="result__a" href="https://duckduckgo.com/about">
        DuckDuckGo About Page
      </a>
    </h2>
    <a class="result__snippet" href="https://duckduckgo.com/about">
      Privacy, simplified.
    </a>
  </div>

  <div class="result results_links results_links_deep web-result ">
    <h2 class="result__title">
      <a rel="nofollow" class="result__a" href="https://en.wikipedia.org/wiki/Rust_(programming_language)">
        Rust (programming language) &#8211; Wikipedia
      </a>
    </h2>
    <a class="result__snippet" href="https://en.wikipedia.org/wiki/Rust_(programming_language)">
      Rust is a multi-paradigm, general-purpose programming language. It emphasizes &#39;performance&#39; and safety.
    </a>
  </div>

  <div class="result results_links results_links_deep web-result ">
    <h2 class="result__title">
      <a rel="nofollow" class="result__a" href="https://doc.rust-lang.org/book/">
        The Rust Programming Language &lt;Book&gt;
      </a>
    </h2>
    <a class="result__snippet" href="https://doc.rust-lang.org/book/">
      Official book on Rust programming.
    </a>
  </div>
</div>
</body>
</html>
"#;

#[test]
fn test_parse_ddg_results_order_entities_tags_and_filter() {
    let results = parse_ddg_results(DDG_RESULTS_FIXTURE, 10).expect("parsing succeeds");

    // Total 3 non-DDG results (duckduckgo.com self-link dropped)
    assert_eq!(results.len(), 3);

    // First result: tags stripped, entities decoded, uddg URL unwrapped
    assert_eq!(results[0].title, "Rust Programming Language & Tools");
    assert_eq!(results[0].url, "https://www.rust-lang.org/");
    assert_eq!(
        results[0].snippet,
        "Empowering everyone to build reliable and \"efficient\" software. Fast & memory-safe."
    );

    // Second result (Wikipedia): &#8211; decoded to en-dash, &#39; decoded to '
    assert_eq!(results[1].title, "Rust (programming language) – Wikipedia");
    assert_eq!(
        results[1].url,
        "https://en.wikipedia.org/wiki/Rust_(programming_language)"
    );
    assert_eq!(
        results[1].snippet,
        "Rust is a multi-paradigm, general-purpose programming language. It emphasizes 'performance' and safety."
    );

    // Third result: <Book> tags encoded as &lt;Book&gt; decoded
    assert_eq!(results[2].title, "The Rust Programming Language <Book>");
    assert_eq!(results[2].url, "https://doc.rust-lang.org/book/");
    assert_eq!(results[2].snippet, "Official book on Rust programming.");
}

#[test]
fn test_parse_ddg_results_respects_num_results() {
    let results = parse_ddg_results(DDG_RESULTS_FIXTURE, 1).expect("parsing succeeds");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "Rust Programming Language & Tools");

    let results2 = parse_ddg_results(DDG_RESULTS_FIXTURE, 2).expect("parsing succeeds");
    assert_eq!(results2.len(), 2);
}

#[test]
fn test_anti_bot_detector_recognises_challenges() {
    let anomaly_html = r#"
        <!DOCTYPE html>
        <html>
        <head><script src="/anomaly.js"></script></head>
        <body>
            <div id="anomaly-modal">Please complete the challenge</div>
        </body>
        </html>
    "#;
    assert_eq!(
        detect_anti_bot_page(anomaly_html),
        Some("anomaly challenge")
    );

    let captcha_html = "<html><body><form><div class=\"g-recaptcha\"></div></form></body></html>";
    assert_eq!(detect_anti_bot_page(captcha_html), Some("recaptcha"));

    let cloudflare_html =
        "<html><body><div id=\"cf-challenge\">Checking browser</div></body></html>";
    assert_eq!(
        detect_anti_bot_page(cloudflare_html),
        Some("cloudflare challenge")
    );

    let robot_html = "<html><body>Please verify you are human</body></html>";
    assert_eq!(detect_anti_bot_page(robot_html), Some("human verification"));

    // Running parse_ddg_results on challenge page produces WebSearchError::Challenge
    let err = parse_ddg_results(anomaly_html, 10).unwrap_err();
    match err {
        WebSearchError::Challenge(ref reason) => {
            assert_eq!(reason, "anomaly challenge");
            let msg = err.to_string();
            assert!(msg.contains("TLS fingerprinting or IP reputation"));
        }
        WebSearchError::Network(ref msg) => panic!("expected Challenge, got Network: {msg}"),
        WebSearchError::Parse(ref msg) => panic!("expected Challenge, got Parse: {msg}"),
        WebSearchError::InvalidInput(ref msg) => {
            panic!("expected Challenge, got InvalidInput: {msg}")
        }
    }
}

#[test]
fn test_empty_results_not_reported_as_challenge() {
    let normal_empty_html = r#"
        <!DOCTYPE html>
        <html>
        <head><title>DuckDuckGo</title></head>
        <body>
          <div class="results">
            <div class="no-results">No results found for your query.</div>
          </div>
        </body>
        </html>
    "#;

    assert_eq!(detect_anti_bot_page(normal_empty_html), None);
    let results = parse_ddg_results(normal_empty_html, 10).expect("normal empty page succeeds");
    assert!(results.is_empty());
}

#[test]
fn test_decode_ddg_url() {
    let wrapped = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Ftokio%2F1.0%2F&rut=abcd";
    assert_eq!(decode_ddg_url(wrapped), "https://docs.rs/tokio/1.0/");

    let direct = "https://example.com/test?query=hello+world";
    assert_eq!(decode_ddg_url(direct), direct);
}

#[test]
fn test_percent_decode() {
    assert_eq!(percent_decode("hello+world"), "hello world");
    assert_eq!(
        percent_decode("https%3A%2F%2Fexample.com%2Ffoo%20bar"),
        "https://example.com/foo bar"
    );
    assert_eq!(percent_decode("plain-text"), "plain-text");
}

#[test]
fn test_html_decode() {
    assert_eq!(
        html_decode(
            "&lt;b&gt;Hello &amp; &quot;World&quot;&apos;s &nbsp;&#39;&#8211;&#x21;&lt;/b&gt;"
        ),
        "<b>Hello & \"World\"'s  '–!</b>"
    );
}

struct MockBackend {
    outcome: Result<Vec<SearchResult>, WebSearchError>,
}

#[async_trait]
impl SearchBackend for MockBackend {
    async fn search(
        &self,
        _query: &str,
        _num_results: usize,
        _timeout: Duration,
    ) -> Result<Vec<SearchResult>, WebSearchError> {
        self.outcome.clone()
    }
}

#[tokio::test]
async fn test_websearch_tool_success_and_formatting() {
    let backend = MockBackend {
        outcome: Ok(vec![
            SearchResult {
                title: "Rust Official".to_string(),
                url: "https://www.rust-lang.org/".to_string(),
                snippet: "Empowering everyone to build reliable software.".to_string(),
            },
            SearchResult {
                title: "Rust Wikipedia".to_string(),
                url: "https://en.wikipedia.org/wiki/Rust".to_string(),
                snippet: "A systems programming language.".to_string(),
            },
        ]),
    };

    let tool = WebSearchTool::with_backend_and_config(
        Arc::new(backend),
        WebConfig {
            default_num_results: 5,
            request_timeout_secs: 10,
        },
    );

    let ctx = dummy_ctx();
    let out = tool
        .call(json!({"query": "rust programming"}), &ctx)
        .await
        .expect("tool call succeeds");

    assert_eq!(out.title, "Search \"rust programming\"");
    assert_eq!(out.preview, "2 results");
    assert!(out.content.contains("Search results for: rust programming"));
    assert!(out.content.contains("1. **Rust Official**"));
    assert!(out.content.contains("https://www.rust-lang.org/"));
    assert!(out.content.contains("2. **Rust Wikipedia**"));
}

#[tokio::test]
async fn test_websearch_tool_validation_and_errors() {
    let tool = WebSearchTool::new();
    let ctx = dummy_ctx();

    // Empty query
    let err = tool.call(json!({"query": "   "}), &ctx).await;
    assert!(matches!(err, Err(ToolError::InvalidInput(_))));

    // Network error
    let fail_backend = MockBackend {
        outcome: Err(WebSearchError::Network("connection refused".to_string())),
    };
    let fail_tool = WebSearchTool::with_backend(Arc::new(fail_backend));
    let net_err = fail_tool
        .call(json!({"query": "test"}), &ctx)
        .await
        .unwrap_err();
    assert!(net_err.to_string().contains("network request failed"));

    // Challenge error
    let chal_backend = MockBackend {
        outcome: Err(WebSearchError::Challenge("anomaly challenge".to_string())),
    };
    let chal_tool = WebSearchTool::with_backend(Arc::new(chal_backend));
    let chal_err = chal_tool
        .call(json!({"query": "test"}), &ctx)
        .await
        .unwrap_err();
    assert!(chal_err.to_string().contains("anti-bot challenge"));
    assert!(
        chal_err
            .to_string()
            .contains("TLS fingerprinting or IP reputation")
    );

    // Parse error
    let parse_backend = MockBackend {
        outcome: Err(WebSearchError::Parse("malformed json/html".to_string())),
    };
    let parse_tool = WebSearchTool::with_backend(Arc::new(parse_backend));
    let parse_err = parse_tool
        .call(json!({"query": "test"}), &ctx)
        .await
        .unwrap_err();
    assert!(
        parse_err
            .to_string()
            .contains("failed to parse search results")
    );
}

#[test]
fn test_webfetch_extraction_strips_chrome() {
    let page_html = r#"
        <!DOCTYPE html>
        <html>
        <head><title>Article Test</title></head>
        <body>
          <header><nav><a href="/">Home</a><a href="/archive">Archive</a></nav></header>
          <aside class="sidebar">Sidebar ads and unrelated trending links</aside>
          <article>
            <h1>Understanding Async Rust</h1>
            <p>Asynchronous programming in Rust allows writing concurrent code with async/await syntax.</p>
            <p>The Future trait is the cornerstone of asynchronous computation in Rust.</p>
          </article>
          <footer>Footer copyright 2026. Cookie preferences. All rights reserved.</footer>
        </body>
        </html>
    "#;

    let md = extract_html_to_markdown(page_html, Some("https://example.com/article"));
    // Article title and body are preserved
    assert!(md.contains("Understanding Async Rust"));
    assert!(md.contains("Asynchronous programming in Rust"));
    // Chrome elements (sidebar, nav, footer) are stripped
    assert!(!md.contains("Sidebar ads"));
    assert!(!md.contains("Cookie preferences"));
    assert!(!md.contains("Archive"));
}

#[test]
fn test_webfetch_extraction_fallback() {
    // Shell page with no extractable article content (e.g. JS app shell)
    let app_shell_html = r#"
        <!DOCTYPE html>
        <html>
        <head><title>App Shell</title></head>
        <body>
          <div id="root"></div>
          <noscript>You need JavaScript to run this app.</noscript>
        </body>
        </html>
    "#;

    let md = extract_html_to_markdown(app_shell_html, Some("https://example.com/app"));
    // The fallback path must trigger and say so in the output
    assert!(
        md.contains("fell back to full document") || md.contains("fell back"),
        "expected fallback notice, got: {md}"
    );
    assert!(md.contains("You need JavaScript to run this app."));
}

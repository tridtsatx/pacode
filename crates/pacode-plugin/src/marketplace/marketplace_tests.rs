use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;

use super::*;

const NOW: u64 = 1_700_000_000_000;

/// A fetcher backed by a table, so no test touches the network.
#[derive(Default)]
struct FakeFetcher {
    responses: Mutex<HashMap<String, Result<Vec<u8>, String>>>,
    calls: Mutex<Vec<String>>,
}

impl FakeFetcher {
    fn with(url: &str, body: Vec<u8>) -> Self {
        let me = Self::default();
        me.set(url, Ok(body));
        me
    }

    fn set(&self, url: &str, response: Result<Vec<u8>, String>) {
        self.responses
            .lock()
            .expect("lock")
            .insert(url.to_string(), response);
    }

    fn call_count(&self) -> usize {
        self.calls.lock().expect("lock").len()
    }
}

#[async_trait::async_trait]
impl MarketplaceFetcher for FakeFetcher {
    async fn get(&self, url: &str, _limit: usize) -> Result<Vec<u8>, FetchError> {
        self.calls.lock().expect("lock").push(url.to_string());
        match self.responses.lock().expect("lock").get(url) {
            Some(Ok(bytes)) => Ok(bytes.clone()),
            Some(Err(message)) => Err(FetchError::Network(message.clone())),
            None => Err(FetchError::Status {
                url: url.to_string(),
                status: 404,
            }),
        }
    }
}

fn index_json() -> Vec<u8> {
    serde_json::json!({
        "name": "claude-plugins-official",
        "owner": {"name": "anthropics"},
        "plugins": [
            {
                "name": "context7",
                "source": "./context7",
                "description": "Upstash Context7 MCP server for documentation",
                "version": "1.2.0",
                "author": "upstash",
                "category": "mcp",
                "keywords": ["docs", "mcp"],
                "strict": true,
                "unknownFutureField": {"anything": 1}
            },
            {"description": "no name, must be skipped"},
            {"name": 42},
            {"name": "skill-creator", "description": "Create new skills", "version": "0.9.0"}
        ]
    })
    .to_string()
    .into_bytes()
}

fn marketplace(
    fetcher: std::sync::Arc<FakeFetcher>,
    dir: &std::path::Path,
    ttl_secs: u64,
) -> Marketplace {
    Marketplace::new(fetcher, dir.join("cache"), dir.join("plugins"), ttl_secs)
}

fn github() -> MarketplaceSource {
    MarketplaceSource::parse("anthropics/claude-plugins").expect("source")
}

#[test]
fn an_index_skips_entries_it_cannot_read_and_keeps_the_rest() {
    let index = MarketplaceIndex::parse(&index_json()).expect("parse");
    assert_eq!(index.name, "claude-plugins-official");
    assert_eq!(index.plugins.len(), 2);
    assert_eq!(index.plugins[0].name, "context7");
    assert_eq!(index.plugins[0].version, "1.2.0");
    assert_eq!(index.plugins[0].keywords, vec!["docs", "mcp"]);
    assert!(index.plugins[0].strict);
    // A missing source means the directory named after the plugin.
    assert_eq!(index.plugins[1].source, "./skill-creator");
}

#[test]
fn index_fields_are_capped() {
    let json = serde_json::json!({
        "plugins": [{
            "name": "n".repeat(manifest::NAME_MAX_CHARS + 40),
            "description": "d".repeat(manifest::DESCRIPTION_MAX_CHARS + 40),
            "keywords": vec!["k"; manifest::MAX_KEYWORDS + 10],
        }]
    })
    .to_string();
    let index = MarketplaceIndex::parse(json.as_bytes()).expect("parse");
    let entry = &index.plugins[0];
    assert_eq!(entry.name.chars().count(), manifest::NAME_MAX_CHARS);
    assert_eq!(
        entry.description.chars().count(),
        manifest::DESCRIPTION_MAX_CHARS
    );
    assert_eq!(entry.keywords.len(), manifest::MAX_KEYWORDS);
}

#[test]
fn a_source_spec_is_parsed_or_refused() {
    let plain = MarketplaceSource::parse("anthropics/claude-plugins").expect("plain");
    assert_eq!(
        plain,
        MarketplaceSource::GitHub {
            owner: "anthropics".into(),
            repo: "claude-plugins".into(),
            reference: None
        }
    );
    assert!(
        plain
            .index_url()
            .ends_with("/HEAD/.claude-plugin/marketplace.json")
    );

    let pinned = MarketplaceSource::parse("owner/repo@v1.2.0").expect("pinned");
    assert_eq!(pinned.display(), "owner/repo@v1.2.0");
    assert!(pinned.index_url().contains("/v1.2.0/"));
    assert!(
        pinned
            .archive_url()
            .expect("archive")
            .ends_with("/tar.gz/v1.2.0")
    );

    let url = MarketplaceSource::parse("https://example.com/marketplace.json").expect("url");
    assert_eq!(url.index_url(), "https://example.com/marketplace.json");
    assert!(url.archive_url().is_none());

    for bad in [
        "",
        "no-slash",
        "owner/repo@",
        "owner/../repo",
        "owner/repo/extra",
        "http://example.com/marketplace.json",
        "owner/repo\nrm -rf",
        "owner/repo@../../etc",
    ] {
        assert!(
            MarketplaceSource::parse(bad).is_err(),
            "{bad:?} must be refused"
        );
    }
}

#[test]
fn cache_keys_are_distinct_and_file_safe() {
    let a = MarketplaceSource::parse("owner/repo")
        .expect("a")
        .cache_key();
    let b = MarketplaceSource::parse("owner/repo@v2")
        .expect("b")
        .cache_key();
    let c = MarketplaceSource::parse("https://example.com/x.json")
        .expect("c")
        .cache_key();
    assert_ne!(a, b);
    assert_ne!(a, c);
    for key in [a, b, c] {
        assert!(
            key.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'),
            "{key} is not file-safe"
        );
    }
}

#[tokio::test]
async fn a_fresh_cache_is_served_without_fetching_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = github();
    let fetcher = std::sync::Arc::new(FakeFetcher::with(&source.index_url(), index_json()));
    let market = marketplace(fetcher.clone(), dir.path(), 3600);

    let first = market.index(&source, NOW).await.expect("first");
    assert!(!first.stale);
    assert_eq!(fetcher.call_count(), 1);

    let second = market.index(&source, NOW + 1000).await.expect("second");
    assert_eq!(second.index, first.index);
    assert_eq!(fetcher.call_count(), 1, "a fresh cache must not refetch");
}

#[tokio::test]
async fn a_stale_cache_is_refetched_and_kept_when_the_fetch_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = github();
    let fetcher = std::sync::Arc::new(FakeFetcher::with(&source.index_url(), index_json()));
    let market = marketplace(fetcher.clone(), dir.path(), 60);

    market.index(&source, NOW).await.expect("first");
    assert_eq!(fetcher.call_count(), 1);

    // Past the TTL and the network is down: the old copy comes back, marked stale.
    fetcher.set(&source.index_url(), Err("offline".to_string()));
    let stale = market
        .index(&source, NOW + 120_000)
        .await
        .expect("stale copy");
    assert!(stale.stale);
    assert_eq!(stale.index.plugins.len(), 2);
    assert_eq!(fetcher.call_count(), 2, "a stale cache must try a refetch");
}

#[tokio::test]
async fn a_cold_cache_with_a_failing_fetch_is_an_error_not_an_empty_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = github();
    let fetcher = std::sync::Arc::new(FakeFetcher::default());
    fetcher.set(&source.index_url(), Err("offline".to_string()));
    let market = marketplace(fetcher, dir.path(), 60);

    assert!(market.index(&source, NOW).await.is_err());
}

#[test]
fn a_corrupt_or_mismatched_cache_file_reads_as_a_miss() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = IndexCache::new(dir.path().to_path_buf(), 3600);
    let index = MarketplaceIndex::parse(&index_json()).expect("parse");
    cache.put("k", &index, NOW);
    assert!(cache.get("k", NOW).is_some());

    std::fs::write(dir.path().join("k.json"), b"{ not json").expect("write");
    assert!(cache.get("k", NOW).is_none(), "corrupt is a miss");

    let wrong_version = serde_json::json!({
        "version": cache::CACHE_FORMAT_VERSION + 1,
        "fetched_at_ms": NOW,
        "index": {"name": "x", "plugins": []}
    })
    .to_string();
    std::fs::write(dir.path().join("k.json"), wrong_version).expect("write");
    assert!(
        cache.get("k", NOW).is_none(),
        "a version mismatch is a miss"
    );
}

#[test]
fn the_cache_keeps_at_most_its_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = IndexCache::new(dir.path().to_path_buf(), 3600);
    let index = MarketplaceIndex::parse(&index_json()).expect("parse");
    for i in 0..(cache::MAX_ENTRIES + 5) {
        cache.put(&format!("k{i}"), &index, NOW + i as u64);
    }
    let count = std::fs::read_dir(dir.path())
        .expect("read dir")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count();
    assert!(count <= cache::MAX_ENTRIES, "kept {count} entries");
}

/// Build a repository tarball the way GitHub does: one wrapping directory.
fn tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("repo-main/{path}"), *content)
                .expect("append");
        }
        builder.finish().expect("finish");
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar_bytes).expect("gz");
    gz.finish().expect("gz finish")
}

fn entry(name: &str, source: &str) -> PluginEntry {
    PluginEntry {
        name: name.to_string(),
        source: source.to_string(),
        description: String::new(),
        version: "1.0.0".to_string(),
        author: manifest::Author::Unknown,
        category: String::new(),
        keywords: Vec::new(),
        strict: false,
    }
}

#[test]
fn only_the_plugins_own_directory_is_unpacked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = tarball(&[
        (
            "context7/.claude-plugin/plugin.json",
            br#"{"name":"context7"}"#,
        ),
        ("context7/skills/use.md", b"# use it"),
        ("other-plugin/skills/no.md", b"not this one"),
        ("README.md", b"repo readme"),
    ]);

    let dest = dir.path().join("context7");
    let files =
        install::unpack_plugin(&archive, &entry("context7", "./context7"), &dest).expect("unpack");

    assert!(dest.join(".claude-plugin/plugin.json").exists());
    assert!(dest.join("skills/use.md").exists());
    assert!(!dest.join("no.md").exists());
    assert!(files.iter().any(|f| f.contains("plugin.json")));
    assert_eq!(files.len(), 2);
}

/// The tar builder refuses to write `..` itself, so a hostile archive has to be
/// forged by writing the name straight into the header — which is exactly what a
/// hostile publisher would do.
fn forged_tarball(name: &str, content: &[u8]) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);

        let mut ok = tar::Header::new_gnu();
        ok.set_size(4);
        ok.set_mode(0o644);
        ok.set_cksum();
        builder
            .append_data(&mut ok, "repo-main/evil/ok.txt", &b"fine"[..])
            .expect("append");

        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        let full = format!("repo-main/{name}");
        let bytes = full.as_bytes();
        let name_field = &mut header.as_old_mut().name;
        name_field[..bytes.len()].copy_from_slice(bytes);
        header.set_cksum();
        builder.append(&header, content).expect("append forged");
        builder.finish().expect("finish");
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar_bytes).expect("gz");
    gz.finish().expect("gz finish")
}

#[test]
fn an_archive_that_reaches_outside_the_plugin_directory_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = forged_tarball("evil/../../etc/passwd", b"pwned");

    let dest = dir.path().join("evil");
    let err = install::unpack_plugin(&archive, &entry("evil", "./evil"), &dest)
        .expect_err("traversal must be refused");
    assert!(matches!(err, InstallError::PathEscape(_)), "{err}");
    assert!(!dir.path().join("etc").exists());
}

#[test]
fn a_missing_plugin_directory_is_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = tarball(&[("something-else/file.txt", b"x")]);
    let err = install::unpack_plugin(
        &archive,
        &entry("absent", "./absent"),
        &dir.path().join("a"),
    )
    .expect_err("must report");
    assert!(matches!(err, InstallError::NotInArchive(_)), "{err}");
}

#[tokio::test]
async fn installing_writes_a_record_and_uninstalling_removes_what_it_wrote() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = github();
    let fetcher = std::sync::Arc::new(FakeFetcher::with(&source.index_url(), index_json()));
    fetcher.set(
        &source.archive_url().expect("archive url"),
        Ok(tarball(&[
            (
                "context7/.claude-plugin/plugin.json",
                br#"{"name":"context7","hooks":{"x":1}}"#,
            ),
            ("context7/skills/use.md", b"# use it"),
        ])),
    );
    let market = marketplace(fetcher, dir.path(), 3600);

    let record = market
        .install(&source, "context7", NOW)
        .await
        .expect("install");
    assert_eq!(record.name, "context7");
    assert_eq!(record.version, "1.2.0");
    assert_eq!(record.source, "anthropics/claude-plugins");
    assert!(record.files.iter().any(|f| f.ends_with("use.md")));

    let plugin_dir = market.plugins_dir().join("context7");
    assert!(plugin_dir.join("skills/use.md").exists());
    assert_eq!(market.installed().len(), 1);

    // The manifest declares hooks, which pacode does not run: say so rather than
    // dropping them silently.
    assert_eq!(
        market.unsupported_components("context7"),
        vec![Component::Hooks]
    );

    market.uninstall("context7").expect("uninstall");
    assert!(!plugin_dir.exists());
    assert!(market.installed().is_empty());
}

#[tokio::test]
async fn a_failed_unpack_leaves_nothing_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = github();
    let fetcher = std::sync::Arc::new(FakeFetcher::with(&source.index_url(), index_json()));
    fetcher.set(
        &source.archive_url().expect("archive url"),
        Ok(tarball(&[("unrelated/file.txt", b"x")])),
    );
    let market = marketplace(fetcher, dir.path(), 3600);

    assert!(market.install(&source, "context7", NOW).await.is_err());
    assert!(!market.plugins_dir().join("context7").exists());
    let staging = std::fs::read_dir(market.plugins_dir())
        .expect("read dir")
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(".staging"));
    assert!(!staging, "a failed install must not leave staging behind");
}

#[tokio::test]
async fn uninstalling_something_pacode_did_not_install_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let market = marketplace(
        std::sync::Arc::new(FakeFetcher::default()),
        dir.path(),
        3600,
    );
    let hand_made = market.plugins_dir().join("mine");
    std::fs::create_dir_all(&hand_made).expect("mkdir");
    std::fs::write(hand_made.join("plugin.lua"), b"-- mine").expect("write");

    assert!(market.uninstall("mine").is_err());
    assert!(hand_made.join("plugin.lua").exists(), "hands off");
}

#[tokio::test]
async fn listing_filters_and_reports_installed_state_and_updates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = github();
    let fetcher = std::sync::Arc::new(FakeFetcher::with(&source.index_url(), index_json()));
    fetcher.set(
        &source.archive_url().expect("archive url"),
        Ok(tarball(&[(
            "context7/.claude-plugin/plugin.json",
            br#"{"name":"context7"}"#,
        )])),
    );
    let market = marketplace(fetcher, dir.path(), 3600);

    let all = market.list(&source, "", NOW).await.expect("list");
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|l| l.installed.is_none()));

    let filtered = market
        .list(&source, "documentation", NOW)
        .await
        .expect("list");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].entry.name, "context7");

    let by_keyword = market.list(&source, "MCP", NOW).await.expect("list");
    assert_eq!(by_keyword.len(), 1);

    market
        .install(&source, "context7", NOW)
        .await
        .expect("install");
    let after = market.list(&source, "context7", NOW).await.expect("list");
    assert!(after[0].installed.is_some());
    assert!(
        !after[0].update_available(),
        "the installed version matches the listed one"
    );

    // A newer version in the index is an available update.
    let mut listing = after[0].clone();
    listing.entry.version = "2.0.0".to_string();
    assert!(listing.update_available());
}

#[test]
fn a_manifest_reports_what_pacode_will_not_run() {
    let manifest = PluginManifest::parse(
        br#"{"name":"x","skills":"./skills","hooks":{"PreToolUse":[]},"agents":["./agents"]}"#,
    )
    .expect("parse");
    let ignored = manifest::unsupported_components(&manifest);
    assert!(ignored.contains(&Component::Hooks));
    assert!(ignored.contains(&Component::Agents));
    assert!(Component::Skills.supported());
    assert!(Component::McpServers.supported());
    // Markdown slash commands are declared by many plugins but pacode's own
    // commands come from its plugin runtime, so they are reported as ignored.
    assert!(!Component::Commands.supported());

    // A component path written either way reads back the same.
    assert_eq!(
        manifest.skills.as_ref().map(|s| s.paths()),
        Some(vec!["./skills"])
    );
    assert_eq!(
        manifest.agents.as_ref().map(|s| s.paths()),
        Some(vec!["./agents"])
    );
}

#[test]
fn a_plugin_name_that_is_not_a_plain_directory_name_is_dropped() {
    // The index comes from someone else's repository, so a name is hostile input.
    for bad in [
        "../../etc",
        "..",
        ".",
        "a/b",
        "a\\b",
        ".hidden",
        "with space",
        "nul\0",
    ] {
        assert!(
            !manifest::is_valid_plugin_name(bad),
            "{bad:?} must be refused"
        );
    }
    for good in ["context7", "skill-creator", "claude_md.management", "a1"] {
        assert!(manifest::is_valid_plugin_name(good), "{good:?} is fine");
    }

    let json = serde_json::json!({
        "plugins": [
            {"name": "../../../.bashrc", "description": "escape attempt"},
            {"name": "honest", "description": "fine"}
        ]
    })
    .to_string();
    let index = MarketplaceIndex::parse(json.as_bytes()).expect("parse");
    assert_eq!(index.plugins.len(), 1);
    assert_eq!(index.plugins[0].name, "honest");
}

#[tokio::test]
async fn a_hostile_name_never_reaches_a_path_join() {
    let dir = tempfile::tempdir().expect("tempdir");
    let market = marketplace(
        std::sync::Arc::new(FakeFetcher::default()),
        dir.path(),
        3600,
    );

    // Nothing outside the plugins directory may be removed, whatever the caller says.
    let outside = dir.path().join("precious");
    std::fs::create_dir_all(&outside).expect("mkdir");
    std::fs::write(outside.join("file"), b"keep me").expect("write");

    assert!(matches!(
        market.uninstall("../precious"),
        Err(MarketplaceError::UnsafeName(_))
    ));
    assert!(outside.join("file").exists(), "nothing outside was touched");
    assert!(market.unsupported_components("../../etc").is_empty());
}

#[test]
fn the_github_token_goes_only_to_github_hosts() {
    for url in [
        "https://raw.githubusercontent.com/o/r/HEAD/.claude-plugin/marketplace.json",
        "https://codeload.github.com/o/r/tar.gz/HEAD",
        "https://api.github.com/repos/o/r",
        "https://GitHub.com/o/r",
    ] {
        assert!(source::is_github_host(url), "{url} is GitHub");
    }
    for url in [
        // A substring test would have handed the token to every one of these.
        "https://evil.example/github/steal",
        "https://github.com.evil.example/x",
        "https://raw.githubusercontent.com.evil.example/x",
        "http://raw.githubusercontent.com/x",
        "not a url",
    ] {
        assert!(!source::is_github_host(url), "{url} is not GitHub");
    }
}

#[tokio::test]
async fn an_installed_plugins_skills_and_mcp_servers_are_exposed_to_the_host() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = github();
    let fetcher = std::sync::Arc::new(FakeFetcher::with(&source.index_url(), index_json()));
    let manifest = br#"{
        "name": "context7",
        "skills": "./skills",
        "commands": "./commands",
        "mcpServers": {
            "context7": {"command": "npx", "args": ["-y", "@upstash/context7-mcp"]},
            "remote": {"url": "https://mcp.example/sse"},
            "broken": {"description": "neither a command nor a url"}
        }
    }"#;
    fetcher.set(
        &source.archive_url().expect("archive url"),
        Ok(tarball(&[
            ("context7/.claude-plugin/plugin.json", manifest),
            ("context7/skills/lookup/SKILL.md", b"# lookup\n"),
        ])),
    );
    let market = marketplace(fetcher, dir.path(), 3600);
    market
        .install(&source, "context7", NOW)
        .await
        .expect("install");

    // Skills sit where pacode's own loader reads them.
    let skill_dirs = market.skill_dirs();
    assert_eq!(skill_dirs.len(), 1);
    assert!(skill_dirs[0].join("lookup/SKILL.md").exists());

    // MCP servers are namespaced by plugin, and one with nothing to start is left out.
    let servers = market.mcp_servers();
    assert_eq!(servers.len(), 2, "{servers:?}");
    let names: Vec<&str> = servers.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"context7/context7"));
    assert!(names.contains(&"context7/remote"));
    assert!(!names.contains(&"context7/broken"));

    // Commands are declared but pacode does not run markdown commands: say so.
    assert_eq!(
        market.unsupported_components("context7"),
        vec![Component::Commands]
    );
}

#[test]
fn a_plugin_source_names_this_repository_or_another_one() {
    use install::PluginSource;

    // In this repository.
    assert_eq!(
        PluginSource::parse("./context7"),
        PluginSource::InRepo("./context7".into())
    );
    assert_eq!(
        PluginSource::parse("./packs/context7"),
        PluginSource::InRepo("./packs/context7".into())
    );
    assert_eq!(
        PluginSource::parse("context7"),
        PluginSource::InRepo("context7".into())
    );
    // A bare `a/b` is `owner/repo`, which is why an in-repo path is written with
    // the leading `./` upstream uses.
    assert!(matches!(
        PluginSource::parse("packs/context7"),
        PluginSource::Repository { .. }
    ));

    // Somewhere else, whole repository.
    let PluginSource::Repository { source, subdir } = PluginSource::parse("obra/superpowers")
    else {
        panic!("owner/repo names another repository");
    };
    assert_eq!(source.display(), "obra/superpowers");
    assert_eq!(subdir, None);

    // Somewhere else, pinned, one directory of it.
    let PluginSource::Repository { source, subdir } =
        PluginSource::parse("owner/repo@v1.0#packs/thing")
    else {
        panic!("owner/repo@ref#dir names another repository");
    };
    assert_eq!(source.display(), "owner/repo@v1.0");
    assert_eq!(subdir.as_deref(), Some("packs/thing"));

    // A traversal attempt is not a repository spec, so it stays a path and the
    // unpack guards refuse it.
    assert_eq!(
        PluginSource::parse("../../etc"),
        PluginSource::InRepo("../../etc".into())
    );
}

#[tokio::test]
async fn a_plugin_maintained_elsewhere_installs_from_its_own_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = github();
    let index = serde_json::json!({
        "name": "pacode-plugins",
        "plugins": [{
            "name": "superpowers",
            "source": "obra/superpowers",
            "description": "Core skills library",
            "version": "6.3.0"
        }]
    })
    .to_string()
    .into_bytes();

    let fetcher = std::sync::Arc::new(FakeFetcher::with(&source.index_url(), index));
    // The archive comes from the other repository, not from the marketplace.
    let upstream = MarketplaceSource::parse("obra/superpowers").expect("upstream");
    fetcher.set(
        &upstream.archive_url().expect("archive url"),
        Ok(tarball(&[
            (".claude-plugin/plugin.json", br#"{"name":"superpowers"}"#),
            ("skills/brainstorming/SKILL.md", b"# brainstorming\n"),
        ])),
    );
    let market = marketplace(fetcher.clone(), dir.path(), 3600);

    let record = market
        .install(&source, "superpowers", NOW)
        .await
        .expect("install");
    assert_eq!(record.name, "superpowers");

    let plugin_dir = market.plugins_dir().join("superpowers");
    assert!(plugin_dir.join("skills/brainstorming/SKILL.md").exists());
    assert!(plugin_dir.join(".claude-plugin/plugin.json").exists());
    assert_eq!(market.skill_dirs().len(), 1);

    // The marketplace archive was never fetched: only the index and the upstream.
    let calls = fetcher.calls.lock().expect("lock").clone();
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("codeload.github.com/anthropics")),
        "{calls:?}"
    );
}

#[test]
fn a_whole_repository_unpacks_from_its_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = tarball(&[
        (".claude-plugin/plugin.json", br#"{"name":"x"}"#),
        ("skills/a/SKILL.md", b"# a"),
    ]);
    let dest = dir.path().join("x");
    let files = install::unpack_subdir(&archive, "", "x", &dest).expect("unpack");
    assert_eq!(files.len(), 2);
    assert!(dest.join("skills/a/SKILL.md").exists());
}

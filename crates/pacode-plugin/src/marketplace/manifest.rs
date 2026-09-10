//! The Claude Code marketplace layout, as pacode reads it.
//!
//! A marketplace repository carries `.claude-plugin/marketplace.json` at its
//! root listing its plugins; each plugin directory carries
//! `.claude-plugin/plugin.json`. Unknown fields are ignored rather than rejected,
//! so a newer upstream schema does not break the listing, and a malformed entry
//! is skipped with a warning rather than failing the whole index.

use serde::{Deserialize, Serialize};

/// Caps on anything that can reach the picker or the model.
pub const NAME_MAX_CHARS: usize = 80;
pub const DESCRIPTION_MAX_CHARS: usize = 400;
pub const VERSION_MAX_CHARS: usize = 40;
pub const KEYWORD_MAX_CHARS: usize = 40;
pub const MAX_KEYWORDS: usize = 12;
/// Most plugins one index may list. A larger index is truncated, not refused.
pub const MAX_PLUGINS: usize = 2000;

pub(crate) fn cap(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect()
}

fn cap_keywords(keywords: Vec<String>) -> Vec<String> {
    keywords
        .into_iter()
        .filter(|k| !k.trim().is_empty())
        .map(|k| cap(&k, KEYWORD_MAX_CHARS))
        .take(MAX_KEYWORDS)
        .collect()
}

/// An author, which upstream writes either as a string or as an object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Author {
    #[default]
    Unknown,
    Name(String),
    Detailed {
        #[serde(default)]
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        email: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
}

impl Author {
    pub fn display(&self) -> String {
        match self {
            Self::Unknown => String::new(),
            Self::Name(name) => cap(name, NAME_MAX_CHARS),
            Self::Detailed { name, .. } => cap(name, NAME_MAX_CHARS),
        }
    }
}

/// One plugin as the marketplace index lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEntry {
    pub name: String,
    /// Where the plugin's files live: a path inside the repository, or a source
    /// of its own. Kept as written; resolution happens at install time.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: Author,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Upstream's strict-validation flag. Carried through, not interpreted.
    #[serde(default)]
    pub strict: bool,
}

/// Whether a plugin name is usable as a single directory name.
///
/// The name comes from a fetched index, so it is attacker-controlled as soon as
/// a user adds someone else's marketplace: without this, a name of `../../..`
/// would send the installer's `remove_dir_all` and `rename` outside the plugin
/// directory entirely.
pub fn is_valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl PluginEntry {
    fn normalise(mut self) -> Option<Self> {
        self.name = cap(&self.name, NAME_MAX_CHARS);
        if !is_valid_plugin_name(&self.name) {
            return None;
        }
        self.description = cap(&self.description, DESCRIPTION_MAX_CHARS);
        self.version = cap(&self.version, VERSION_MAX_CHARS);
        self.category = cap(&self.category, NAME_MAX_CHARS);
        self.keywords = cap_keywords(std::mem::take(&mut self.keywords));
        // A source that is empty means "the directory named after the plugin",
        // which is what upstream does for a plugin living in its own folder.
        if self.source.trim().is_empty() {
            self.source = format!("./{}", self.name);
        }
        Some(self)
    }
}

/// The index at `.claude-plugin/marketplace.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceIndex {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owner: Author,
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
}

impl MarketplaceIndex {
    /// Parse an index, skipping entries that cannot be read instead of failing.
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        // Parsing entry by entry is what makes one bad plugin skippable: a single
        // `from_slice` into `Vec<PluginEntry>` would reject the whole file.
        let raw: RawIndex = serde_json::from_slice(bytes)?;
        let mut plugins = Vec::new();
        for (i, value) in raw.plugins.into_iter().enumerate() {
            match serde_json::from_value::<PluginEntry>(value) {
                Ok(entry) => match entry.normalise() {
                    Some(entry) => plugins.push(entry),
                    None => log::warn!("marketplace entry {i} has no name, skipping it"),
                },
                Err(e) => log::warn!("marketplace entry {i} is unreadable, skipping it: {e}"),
            }
            if plugins.len() >= MAX_PLUGINS {
                log::warn!("marketplace index is longer than {MAX_PLUGINS} plugins, truncating");
                break;
            }
        }
        Ok(Self {
            name: cap(&raw.name, NAME_MAX_CHARS),
            owner: raw.owner,
            plugins,
        })
    }

    pub fn find(&self, name: &str) -> Option<&PluginEntry> {
        self.plugins.iter().find(|p| p.name == name)
    }
}

#[derive(Deserialize)]
struct RawIndex {
    #[serde(default)]
    name: String,
    #[serde(default)]
    owner: Author,
    #[serde(default)]
    plugins: Vec<serde_json::Value>,
}

/// A plugin's own manifest, at `<plugin>/.claude-plugin/plugin.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: Author,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Component paths as upstream declares them. Absent means the conventional
    /// directory, which is how upstream plugins are usually laid out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<PathList>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<PathList>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<PathList>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<serde_json::Value>,
    /// Upstream spells this `mcpServers`; the snake_case spelling is accepted too.
    #[serde(
        default,
        rename = "mcpServers",
        alias = "mcp_servers",
        skip_serializing_if = "Option::is_none"
    )]
    pub mcp_servers: Option<serde_json::Value>,
}

/// Upstream writes a component path either as one string or as a list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PathList {
    One(String),
    Many(Vec<String>),
}

impl PathList {
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Self::One(p) => vec![p.as_str()],
            Self::Many(ps) => ps.iter().map(String::as_str).collect(),
        }
    }
}

impl PluginManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let mut manifest: Self = serde_json::from_slice(bytes)?;
        manifest.name = cap(&manifest.name, NAME_MAX_CHARS);
        manifest.description = cap(&manifest.description, DESCRIPTION_MAX_CHARS);
        manifest.version = cap(&manifest.version, VERSION_MAX_CHARS);
        manifest.homepage = cap(&manifest.homepage, DESCRIPTION_MAX_CHARS);
        manifest.license = cap(&manifest.license, NAME_MAX_CHARS);
        manifest.keywords = cap_keywords(std::mem::take(&mut manifest.keywords));
        Ok(manifest)
    }
}

/// One MCP server as a plugin declares it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerDecl {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// An http server instead of a spawned command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

impl PluginManifest {
    /// MCP servers the manifest declares, skipping any that name neither a
    /// command nor a URL — there would be nothing to start.
    pub fn mcp_server_decls(&self) -> Vec<(String, McpServerDecl)> {
        let Some(value) = &self.mcp_servers else {
            return Vec::new();
        };
        let Some(map) = value.as_object() else {
            return Vec::new();
        };
        map.iter()
            .filter_map(|(name, decl)| {
                match serde_json::from_value::<McpServerDecl>(decl.clone()) {
                    Ok(decl) if !decl.command.is_empty() || decl.url.is_some() => {
                        Some((cap(name, NAME_MAX_CHARS), decl))
                    }
                    Ok(_) => {
                        log::warn!("mcp server {name:?} declares neither a command nor a url");
                        None
                    }
                    Err(e) => {
                        log::warn!("mcp server {name:?} is unreadable: {e}");
                        None
                    }
                }
            })
            .collect()
    }
}

/// What pacode can and cannot honour from a Claude Code plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    Skills,
    Commands,
    McpServers,
    Hooks,
    Agents,
}

impl Component {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Commands => "commands",
            Self::McpServers => "mcpServers",
            Self::Hooks => "hooks",
            Self::Agents => "agents",
        }
    }

    /// Whether pacode runs this component today. An unsupported one is reported
    /// to the caller as a warning rather than dropped in silence.
    pub fn supported(self) -> bool {
        match self {
            // Skills are read from `<plugin>/skills/*/SKILL.md`, which is the
            // layout pacode's own skill loader already reads; MCP servers are
            // merged into the pool under a per-plugin name.
            Self::Skills | Self::McpServers => true,
            // pacode has its own subagent model, no hook runner, and its slash
            // commands come from its own plugin runtime rather than from markdown.
            Self::Commands | Self::Hooks | Self::Agents => false,
        }
    }
}

/// Components a manifest declares that pacode will ignore.
pub fn unsupported_components(manifest: &PluginManifest) -> Vec<Component> {
    let mut out = Vec::new();
    if manifest.hooks.is_some() {
        out.push(Component::Hooks);
    }
    if manifest.agents.is_some() {
        out.push(Component::Agents);
    }
    if manifest.commands.is_some() {
        out.push(Component::Commands);
    }
    out.retain(|c| !c.supported());
    out
}

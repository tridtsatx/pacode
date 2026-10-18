//! Shape of the per-language tables (`crate::tables`): which node kinds play which
//! role in an outline. Pure data; the walk (`crate::walk`) is the same for every language.

use tree_sitter::Node;

/// What a node kind means for the outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// Class / impl / trait / module / namespace / interface / enum: header and closing
    /// line kept, members outlined one by one.
    Container,
    /// Function / method: signature, the first `body_lines` body lines and the closing
    /// line kept, the rest of the body hidden.
    Callable,
    /// Field / const / static / variable: first `body_lines` lines and the last line kept.
    Binding,
    /// Imports / uses: a run of them is shown when short, else hidden behind one marker.
    Import,
    /// Comments: a run directly above a declaration keeps its first line.
    Comment,
    /// Attributes / decorators directly above a declaration: kept (up to `body_lines` lines each).
    Attribute,
    /// Transparent wrapper (`export`, decorated definition, template): its children are
    /// treated as members of the enclosing container.
    Wrapper,
}

/// Where a container's / callable's body (the part that can be hidden) is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Body {
    /// The `body` field, else the first descendant (3 levels) whose kind is in
    /// [`LangSpec::body_kinds`].
    Auto,
    /// The node itself: its first line is the header, its children the members.
    SelfNode,
    /// A named field (e.g. a JSON pair's `value`).
    Field(&'static str),
}

/// One node kind and its role.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rule {
    pub kind: &'static str,
    pub role: Role,
    /// Short word for markers and symbols (`fn`, `impl`, `class`). Empty = the node's
    /// first keyword token (`const`/`let`, Swift `struct`/`extension`).
    pub label: &'static str,
    pub body: Body,
}

impl Rule {
    pub(crate) const fn new(
        kind: &'static str,
        role: Role,
        label: &'static str,
        body: Body,
    ) -> Self {
        Self {
            kind,
            role,
            label,
            body,
        }
    }
}

/// Everything the walk needs to know about one language.
#[derive(Debug)]
pub(crate) struct LangSpec {
    pub rules: &'static [Rule],
    /// Body kinds searched when a node has no `body` field ([`Body::Auto`]).
    pub body_kinds: &'static [&'static str],
    /// Child kinds that carry a declaration's name when it has no name field
    /// (e.g. `variable_declarator` in `const x = 1`).
    pub name_kinds: &'static [&'static str],
    /// Containers nested this deep are collapsed like callables.
    pub max_depth: usize,
    /// Data format: containers collapsed at `max_depth` keep only their first and last line.
    pub data: bool,
    /// Containers keep the first `body_lines` lines after their header (Markdown sections).
    pub lead: bool,
    /// Unclassified members keep their first line (statements at module level).
    pub keep_first_row: bool,
}

impl LangSpec {
    /// The rule for a named node, if its kind is in the table.
    pub(crate) fn rule(&self, node: Node<'_>) -> Option<&'static Rule> {
        if !node.is_named() {
            return None;
        }
        let rules: &'static [Rule] = self.rules;
        let kind = node.kind();
        rules.iter().find(|r| r.kind == kind)
    }

    pub(crate) fn role(&self, node: Node<'_>) -> Option<Role> {
        self.rule(node).map(|r| r.role)
    }
}

/// Fields tried, in order, for a declaration's name.
pub(crate) const NAME_FIELDS: &[&str] = &["name", "declarator", "type", "key"];

/// Fields followed from a name candidate to the identifier itself (C declarators,
/// Python assignment targets, Markdown heading text).
pub(crate) const DESCEND_FIELDS: &[&str] = &["declarator", "name", "left", "heading_content"];

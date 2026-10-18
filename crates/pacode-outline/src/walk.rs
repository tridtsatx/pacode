//! The language-agnostic walk: visits declarations with the per-language table and
//! marks which rows of the file stay visible. Hidden regions are derived from the
//! unmarked rows afterwards (`crate::build`).
//!
//! Recursion depth is bounded: containers nest at most `LangSpec::max_depth` levels
//! (deeper ones are collapsed like callables), wrappers are flattened at most
//! [`MAX_WRAPPER_DEPTH`] levels, and searches inside other statements use an explicit
//! stack. Every visited node costs one unit of `OutlineOpts::node_budget`.

use std::ops::Range;

use tree_sitter::Node;

use crate::names::{label_of, name_of};
use crate::nodes::{closer_row, header_end, last_row, line_starts, named_children, start_row};
use crate::spec::{Body, LangSpec, Role, Rule};
use crate::{OutlineError, OutlineOpts, Symbol};

/// Wrappers nested deeper than this are treated as ordinary statements.
const MAX_WRAPPER_DEPTH: usize = 16;
/// Levels searched below a node for its body kind / for a value that is a declaration.
const BODY_SEARCH_LEVELS: usize = 3;
const INNER_DECL_LEVELS: usize = 4;
/// Import runs shorter than this many lines are shown as they are.
const IMPORT_COLLAPSE_LINES: usize = 6;
/// Member cap for data formats (huge arrays), whatever `OutlineOpts::max_members` says.
const DATA_MAX_MEMBERS: usize = 100;

type Walked<T> = Result<T, OutlineError>;

/// A labelled row range that is not a declaration (an import run).
pub(crate) struct Hint {
    pub lines: Range<usize>,
    pub label: &'static str,
}

/// Comments / attributes seen since the last declaration.
struct Prefix {
    start: usize,
    end: usize,
    first_comment: Option<usize>,
    attrs: Vec<(usize, usize)>,
}

pub(crate) struct Walker<'s> {
    src: &'s [u8],
    spec: &'static LangSpec,
    opts: &'s OutlineOpts,
    starts: Vec<usize>,
    keep: Vec<bool>,
    hints: Vec<Hint>,
    visits: usize,
    /// > 0 while walking members past `OutlineOpts::max_members`: symbols are still
    /// > recorded, rows are not kept.
    suppress: usize,
}

impl<'s> Walker<'s> {
    pub(crate) fn new(
        src: &'s str,
        rows: usize,
        spec: &'static LangSpec,
        opts: &'s OutlineOpts,
    ) -> Self {
        Self {
            src: src.as_bytes(),
            spec,
            opts,
            starts: line_starts(src.as_bytes()),
            keep: vec![false; rows],
            hints: Vec::new(),
            visits: 0,
            suppress: 0,
        }
    }

    /// Kept-row flags and import hints, once the walk is done.
    pub(crate) fn into_parts(self) -> (Vec<bool>, Vec<Hint>) {
        (self.keep, self.hints)
    }

    fn tick(&mut self) -> Walked<()> {
        self.visits += 1;
        if self.visits > self.opts.node_budget {
            return Err(OutlineError::Budget {
                budget: self.opts.node_budget,
            });
        }
        Ok(())
    }

    /// Keep rows `a..b` (clamped to the file).
    fn mark(&mut self, a: usize, b: usize) {
        if self.suppress > 0 {
            return;
        }
        let end = b.min(self.keep.len());
        if let Some(rows) = self.keep.get_mut(a.min(end)..end) {
            rows.fill(true);
        }
    }

    /// Named children with wrappers replaced by their own children.
    fn flatten<'t>(&mut self, node: Node<'t>, depth: usize) -> Walked<Vec<Node<'t>>> {
        let mut out = Vec::new();
        for child in named_children(node) {
            self.tick()?;
            if depth < MAX_WRAPPER_DEPTH && self.spec.role(child) == Some(Role::Wrapper) {
                out.extend(self.flatten(child, depth + 1)?);
            } else {
                out.push(child);
            }
        }
        Ok(out)
    }

    /// Outline the members of `parent` (the root, or a container's body).
    pub(crate) fn members(&mut self, parent: Node<'_>, depth: usize) -> Walked<Vec<Symbol>> {
        let mut symbols = Vec::new();
        let mut prefix: Option<Prefix> = None;
        let mut imports: Option<(usize, usize)> = None;
        let mut count = 0usize;
        let max_members = if self.spec.data {
            self.opts.max_members.min(DATA_MAX_MEMBERS)
        } else {
            self.opts.max_members
        };
        for child in self.flatten(parent, 0)? {
            self.tick()?;
            let rule = self.spec.rule(child);
            let role = rule.map(|r| r.role);
            let (s, e) = (start_row(child), last_row(child));
            if !matches!(role, Some(Role::Import | Role::Comment))
                && let Some(run) = imports.take()
            {
                self.flush_imports(run);
            }
            match role {
                Some(Role::Comment | Role::Attribute) => {
                    let adjacent = prefix.as_ref().is_some_and(|p| s <= p.end + 1);
                    if !adjacent {
                        self.drop_prefix(prefix.take());
                    }
                    let p = prefix.get_or_insert_with(|| Prefix {
                        start: s,
                        end: e,
                        first_comment: None,
                        attrs: Vec::new(),
                    });
                    p.end = p.end.max(e);
                    if role == Some(Role::Comment) {
                        p.first_comment.get_or_insert(s);
                    } else {
                        p.attrs.push((s, e));
                    }
                    continue;
                }
                Some(Role::Import) => {
                    imports = Some(match imports {
                        Some((a, b)) => (a, b.max(e)),
                        None => match prefix.take() {
                            Some(p) if s <= p.end + 1 => (p.start, e),
                            other => {
                                self.drop_prefix(other);
                                (s, e)
                            }
                        },
                    });
                    self.drop_prefix(prefix.take());
                    continue;
                }
                Some(Role::Container | Role::Callable | Role::Binding | Role::Wrapper) | None => {}
            }
            count += 1;
            let suppressed = count > max_members;
            if suppressed {
                self.suppress += 1;
            }
            let mut start = s;
            match prefix.take() {
                Some(p) if s <= p.end + 1 => {
                    if let Some(first) = p.first_comment {
                        self.mark(first, first + 1);
                    }
                    for (a, b) in &p.attrs {
                        let last = (*b).min(*a + self.opts.body_lines.saturating_sub(1));
                        self.mark(*a, last + 1);
                    }
                    start = p.start;
                }
                other => self.drop_prefix(other),
            }
            let result = self.decl(child, rule, depth, start, None);
            if suppressed {
                self.suppress -= 1;
            }
            symbols.extend(result?);
        }
        if let Some(run) = imports.take() {
            self.flush_imports(run);
        }
        self.drop_prefix(prefix.take());
        Ok(symbols)
    }

    /// A comment run not followed by a declaration is hidden, except a file header
    /// (a run starting on the first line), whose first line is kept.
    fn drop_prefix(&mut self, prefix: Option<Prefix>) {
        if let Some(Prefix {
            start: 0,
            first_comment: Some(first),
            ..
        }) = prefix
        {
            self.mark(first, first + 1);
        }
    }

    fn flush_imports(&mut self, (a, b): (usize, usize)) {
        if b + 1 - a < IMPORT_COLLAPSE_LINES {
            self.mark(a, b + 1);
        } else {
            self.hints.push(Hint {
                lines: a..b + 1,
                label: "imports",
            });
        }
    }

    /// Outline one member. `start` is its first row including attached doc comments
    /// and attributes; `name` overrides the node's own name (a declaration bound to a variable).
    fn decl(
        &mut self,
        node: Node<'_>,
        rule: Option<&'static Rule>,
        depth: usize,
        start: usize,
        name: Option<String>,
    ) -> Walked<Vec<Symbol>> {
        let Some(rule) = rule else {
            return self.unclassified(node, depth);
        };
        match rule.role {
            Role::Container => Ok(vec![self.container(node, rule, depth, start, name)?]),
            Role::Callable => Ok(vec![self.callable(node, rule, start, name, false)?]),
            Role::Binding => self.binding(node, rule, depth, start),
            Role::Import | Role::Comment | Role::Attribute | Role::Wrapper => {
                self.unclassified(node, depth)
            }
        }
    }

    fn container(
        &mut self,
        node: Node<'_>,
        rule: &'static Rule,
        depth: usize,
        start: usize,
        name: Option<String>,
    ) -> Walked<Symbol> {
        if depth >= self.spec.max_depth {
            return self.callable(node, rule, start, name, true);
        }
        let Some(body) = self.find_body(node, rule)? else {
            return Ok(self.leaf(node, rule, start, name, false));
        };
        let header_end = header_end(self.src, &self.starts, node, body);
        self.mark(start_row(node), header_end + 1);
        let closer = closer_row(node, body, header_end);
        if self.spec.lead {
            let first_member = named_children(body)
                .into_iter()
                .find(|c| self.spec.role(*c) == Some(Role::Container))
                .map(start_row);
            let limit = first_member.unwrap_or(last_row(node) + 1);
            self.mark(
                header_end + 1,
                (header_end + 1 + self.opts.body_lines).min(limit),
            );
        }
        let children = self.members(body, depth + 1)?;
        if let Some(row) = closer {
            self.mark(row, row + 1);
        }
        Ok(self.symbol(node, rule, start, name, children))
    }

    /// `collapsed` = a container past `max_depth` (data formats then keep no body lines).
    fn callable(
        &mut self,
        node: Node<'_>,
        rule: &'static Rule,
        start: usize,
        name: Option<String>,
        collapsed: bool,
    ) -> Walked<Symbol> {
        let Some(body) = self.find_body(node, rule)? else {
            return Ok(self.leaf(node, rule, start, name, true));
        };
        let keep_lines = if collapsed && self.spec.data {
            0
        } else {
            self.opts.body_lines
        };
        let header_end = header_end(self.src, &self.starts, node, body);
        self.mark(start_row(node), header_end + 1);
        let closer = closer_row(node, body, header_end);
        let content_end = closer.unwrap_or(last_row(node) + 1);
        self.mark(
            header_end + 1,
            (header_end + 1 + keep_lines).min(content_end),
        );
        if let Some(row) = closer {
            self.mark(row, row + 1);
        }
        Ok(self.symbol(node, rule, start, name, Vec::new()))
    }

    /// A declaration without a body: its first lines (signature cap for callables,
    /// `body_lines` otherwise) and its last line.
    fn leaf(
        &mut self,
        node: Node<'_>,
        rule: &'static Rule,
        start: usize,
        name: Option<String>,
        signature: bool,
    ) -> Symbol {
        let (s, e) = (start_row(node), last_row(node));
        let cap = if signature {
            self.opts.max_signature_lines
        } else {
            self.opts.body_lines
        };
        self.mark(s, (e + 1).min(s + cap.max(1)));
        self.mark(e, e + 1);
        self.symbol(node, rule, start, name, Vec::new())
    }

    fn binding(
        &mut self,
        node: Node<'_>,
        rule: &'static Rule,
        depth: usize,
        start: usize,
    ) -> Walked<Vec<Symbol>> {
        if let Some(inner) = self.inner_decl(node)? {
            let name = name_of(self.src, self.spec, node);
            let inner_rule = self.spec.rule(inner);
            let name = (!name.is_empty()).then_some(name);
            let mut out = self.decl(inner, inner_rule, depth, start, name)?;
            let e = last_row(node);
            self.mark(e, e + 1);
            for symbol in &mut out {
                symbol.lines = start..e + 1;
            }
            return Ok(out);
        }
        let symbol = self.leaf(node, rule, start, None, false);
        Ok(if symbol.name.is_empty() {
            Vec::new()
        } else {
            vec![symbol]
        })
    }

    /// A multi-line container/callable that starts on the binding's first row
    /// (`const f = () => {`, `static X: S = S {` is not one: struct literals are values).
    fn inner_decl<'t>(&mut self, node: Node<'t>) -> Walked<Option<Node<'t>>> {
        let row = start_row(node);
        let mut frontier = named_children(node);
        for _ in 0..INNER_DECL_LEVELS {
            let mut next = Vec::new();
            for child in frontier {
                self.tick()?;
                let decl = matches!(
                    self.spec.role(child),
                    Some(Role::Container | Role::Callable)
                );
                if decl && start_row(child) == row && last_row(child) > row {
                    return Ok(Some(child));
                }
                next.extend(named_children(child));
            }
            frontier = next;
        }
        Ok(None)
    }

    /// A statement the table does not know: its first line (when the language keeps
    /// them), plus any container/callable found inside it (`describe(() => {...})`,
    /// declarations under `if __name__ == "__main__":`).
    fn unclassified(&mut self, node: Node<'_>, depth: usize) -> Walked<Vec<Symbol>> {
        let (s, e) = (start_row(node), last_row(node));
        if self.spec.keep_first_row {
            self.mark(s, s + 1);
        }
        let mut out = Vec::new();
        if e == s {
            return Ok(out);
        }
        let mut stack: Vec<Node<'_>> = named_children(node).into_iter().rev().collect();
        while let Some(child) = stack.pop() {
            self.tick()?;
            let rule = self.spec.rule(child);
            if matches!(rule.map(|r| r.role), Some(Role::Container | Role::Callable)) {
                out.extend(self.decl(child, rule, depth + 1, start_row(child), None)?);
            } else {
                stack.extend(named_children(child).into_iter().rev());
            }
        }
        Ok(out)
    }

    fn find_body<'t>(&mut self, node: Node<'t>, rule: &Rule) -> Walked<Option<Node<'t>>> {
        match rule.body {
            Body::SelfNode => Ok(Some(node)),
            Body::Field(field) => Ok(node.child_by_field_name(field)),
            Body::Auto => {
                if let Some(body) = node.child_by_field_name("body") {
                    return Ok(Some(body));
                }
                if self.spec.body_kinds.is_empty() {
                    return Ok(None);
                }
                let mut frontier = named_children(node);
                for _ in 0..BODY_SEARCH_LEVELS {
                    let mut next = Vec::new();
                    for child in frontier {
                        self.tick()?;
                        if self.spec.body_kinds.contains(&child.kind()) {
                            return Ok(Some(child));
                        }
                        next.extend(named_children(child));
                    }
                    frontier = next;
                }
                Ok(None)
            }
        }
    }

    fn symbol(
        &self,
        node: Node<'_>,
        rule: &'static Rule,
        start: usize,
        name: Option<String>,
        children: Vec<Symbol>,
    ) -> Symbol {
        let end = (last_row(node) + 1).min(self.keep.len()).max(start);
        Symbol {
            kind: label_of(node, rule),
            name: name.unwrap_or_else(|| name_of(self.src, self.spec, node)),
            lines: start..end,
            children,
        }
    }
}

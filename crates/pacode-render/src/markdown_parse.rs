use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::RenderOptions;

pub enum Block {
    Paragraph(Line<'static>),
    Heading {
        line: Line<'static>,
    },
    Code {
        lines: Vec<String>,
    },
    ListItem {
        ordered: bool,
        number: u64,
        depth: usize,
        line: Line<'static>,
    },
    Quote(Vec<Line<'static>>),
    ThematicBreak,
    Table {
        rows: Vec<Vec<String>>,
    },
}

#[derive(Clone, Copy, Default)]
struct StyleStack {
    bold: bool,
    italic: bool,
    strikethrough: bool,
}

impl StyleStack {
    fn apply(self, mut base: Style) -> Style {
        if self.bold {
            base = base.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            base = base.add_modifier(Modifier::ITALIC);
        }
        if self.strikethrough {
            base = base.add_modifier(Modifier::CROSSED_OUT);
        }
        base
    }
}

struct ListFrame {
    ordered: bool,
    next_number: u64,
    depth: usize,
}

pub fn parse_to_blocks(source: &str, opts: &RenderOptions) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(source, options);
    let mut blocks: Vec<Block> = Vec::new();

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut style_stack = StyleStack::default();

    let mut in_heading = false;
    let mut in_code_block = false;
    let mut code_buf = String::new();

    let mut list_stack: Vec<ListFrame> = Vec::new();
    let mut current_item_info: Option<(bool, u64, usize)> = None;

    let mut in_blockquote = 0usize;
    let mut bq_lines: Vec<Line<'static>> = Vec::new();

    let mut in_link = false;
    let mut link_url = String::new();
    let mut link_text = String::new();

    let mut in_table = false;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut table_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();

    let flush_inline = |spans: &mut Vec<Span<'static>>,
                        blocks: &mut Vec<Block>,
                        in_heading: bool,
                        current_item_info: &mut Option<(bool, u64, usize)>,
                        in_blockquote: usize,
                        bq_lines: &mut Vec<Line<'static>>| {
        if spans.is_empty() {
            return;
        }
        let line = Line::from(std::mem::take(spans));
        if in_blockquote > 0 {
            bq_lines.push(line);
        } else if in_heading {
            blocks.push(Block::Heading { line });
        } else if let Some((ordered, number, depth)) = current_item_info.take() {
            blocks.push(Block::ListItem {
                ordered,
                number,
                depth,
                line,
            });
        } else {
            blocks.push(Block::Paragraph(line));
        }
    };

    for event in parser {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
                in_heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                let mut heading_spans = std::mem::take(&mut spans);
                let heading_style = opts.theme.accent.add_modifier(Modifier::BOLD);
                for s in &mut heading_spans {
                    s.style = heading_style;
                }
                blocks.push(Block::Heading {
                    line: Line::from(heading_spans),
                });
                in_heading = false;
            }

            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
            }

            Event::Start(Tag::List(first)) => {
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
                let depth = list_stack.len();
                list_stack.push(ListFrame {
                    ordered: first.is_some(),
                    next_number: first.unwrap_or(1),
                    depth,
                });
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
            }

            Event::Start(Tag::Item) => {
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
                if let Some(frame) = list_stack.last_mut() {
                    let num = frame.next_number;
                    frame.next_number += 1;
                    current_item_info = Some((frame.ordered, num, frame.depth));
                }
            }
            Event::End(TagEnd::Item) => {
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
                current_item_info = None;
            }

            Event::Start(Tag::BlockQuote(_)) => {
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
                in_blockquote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
                in_blockquote = in_blockquote.saturating_sub(1);
                if in_blockquote == 0 && !bq_lines.is_empty() {
                    blocks.push(Block::Quote(std::mem::take(&mut bq_lines)));
                }
            }

            Event::Start(Tag::CodeBlock(kind)) => {
                let _ = kind;
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
                in_code_block = true;
                code_buf.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                let trimmed = code_buf.strip_suffix('\n').unwrap_or(&code_buf);
                let lines = trimmed.split('\n').map(|s| s.to_string()).collect();
                blocks.push(Block::Code { lines });
                code_buf.clear();
            }

            Event::Start(Tag::Table(_)) => {
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
                in_table = true;
                table_rows.clear();
            }
            Event::End(TagEnd::Table) => {
                in_table = false;
                if !table_rows.is_empty() {
                    blocks.push(Block::Table {
                        rows: std::mem::take(&mut table_rows),
                    });
                }
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                table_row.clear();
            }
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                if !table_row.is_empty() {
                    table_rows.push(std::mem::take(&mut table_row));
                }
            }
            Event::Start(Tag::TableCell) => {
                current_cell.clear();
            }
            Event::End(TagEnd::TableCell) => {
                table_row.push(current_cell.trim().to_string());
                current_cell.clear();
            }

            Event::Start(Tag::Emphasis) => style_stack.italic = true,
            Event::End(TagEnd::Emphasis) => style_stack.italic = false,
            Event::Start(Tag::Strong) => style_stack.bold = true,
            Event::End(TagEnd::Strong) => style_stack.bold = false,
            Event::Start(Tag::Strikethrough) => style_stack.strikethrough = true,
            Event::End(TagEnd::Strikethrough) => style_stack.strikethrough = false,

            Event::Start(Tag::Link { dest_url, .. }) => {
                in_link = true;
                link_url = dest_url.to_string();
                link_text.clear();
            }
            Event::End(TagEnd::Link) => {
                in_link = false;
                if link_url != link_text && !link_url.is_empty() {
                    spans.push(Span::styled(format!(" ({link_url})"), opts.theme.dim));
                }
            }

            Event::Text(t) => {
                if in_code_block {
                    code_buf.push_str(&t);
                } else if in_table {
                    current_cell.push_str(&t);
                } else {
                    if in_link {
                        link_text.push_str(&t);
                    }
                    let base_style = if in_link {
                        opts.theme.cyan
                    } else {
                        opts.theme.fg
                    };
                    spans.push(Span::styled(t.to_string(), style_stack.apply(base_style)));
                }
            }

            Event::Code(t) => {
                if in_table {
                    current_cell.push_str(&t);
                } else {
                    if in_link {
                        link_text.push_str(&t);
                    }
                    spans.push(Span::styled(
                        t.to_string(),
                        style_stack.apply(opts.theme.accent),
                    ));
                }
            }

            Event::Rule => {
                flush_inline(
                    &mut spans,
                    &mut blocks,
                    in_heading,
                    &mut current_item_info,
                    in_blockquote,
                    &mut bq_lines,
                );
                blocks.push(Block::ThematicBreak);
            }

            Event::SoftBreak | Event::HardBreak => {
                if in_table {
                    current_cell.push(' ');
                } else {
                    spans.push(Span::raw(" "));
                }
            }

            _ => {}
        }
    }

    flush_inline(
        &mut spans,
        &mut blocks,
        in_heading,
        &mut current_item_info,
        in_blockquote,
        &mut bq_lines,
    );

    blocks
}

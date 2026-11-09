//! GFM rendering with safe remote images and readable tables in a narrow inspector.
use std::ops::Range;

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use html5gum::{StartTag, Token, Tokenizer};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

#[derive(Debug)]
enum Block {
    Markdown(String),
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

#[derive(Default)]
pub struct Document {
    source: String,
    blocks: Vec<Block>,
    cache: CommonMarkCache,
}

impl Document {
    pub fn show(&mut self, ui: &mut egui::Ui, source: &str) {
        if self.source != source {
            self.source = source.to_owned();
            self.blocks = prepare(source);
            self.cache = CommonMarkCache::default();
        }
        for (index, block) in self.blocks.iter().enumerate() {
            ui.push_id(index, |ui| match block {
                Block::Markdown(markdown) => render(ui, &mut self.cache, markdown),
                Block::Table { header, rows } => {
                    let weights: Vec<f32> = (0..header.len())
                        .map(|column| {
                            let cells = rows.iter().filter_map(|row| row.get(column));
                            let max_len =
                                cells.clone().map(|s| s.chars().count()).max().unwrap_or(0);
                            if !rows.is_empty()
                                && cells.clone().all(|s| s.trim().starts_with("<img "))
                            {
                                0.0
                            } else if max_len > 150 {
                                2.0
                            } else {
                                1.0
                            }
                        })
                        .collect();
                    let gaps = header.len().saturating_sub(1) as f32 * 12.0;
                    let table_width = ui.available_width().max(header.len() as f32 * 90.0 + gaps);
                    let image_width = weights.iter().filter(|w| **w == 0.0).count() as f32 * 64.0;
                    let text_width = (table_width - gaps - image_width).max(40.0);
                    let weight_total = weights.iter().sum::<f32>().max(1.0);
                    let widths: Vec<f32> = weights
                        .iter()
                        .map(|weight| {
                            if *weight == 0.0 {
                                64.0
                            } else {
                                text_width * weight / weight_total
                            }
                        })
                        .collect();
                    // A bounded table viewport keeps its horizontal scrollbar reachable
                    // even when an author's table contains many tall rows.
                    egui::ScrollArea::both()
                        .id_salt("table_scroll")
                        .max_height(ui.available_height().clamp(220.0, 420.0))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (row_index, row) in std::iter::once(header).chain(rows).enumerate()
                            {
                                // Measure actual child UIs before advancing the row. Grid's
                                // cached cell height can lag behind asynchronously loaded images
                                // and wrapped CommonMark paragraphs, causing text to overlap.
                                let top_left = ui.cursor().min;
                                let background = ui.painter().add(egui::Shape::Noop);
                                let mut x = top_left.x;
                                let mut row_height: f32 = 0.0;
                                for (column, width) in widths.iter().enumerate() {
                                    let cell_rect = egui::Rect::from_min_size(
                                        egui::pos2(x, top_left.y),
                                        egui::vec2(*width, 100_000.0),
                                    );
                                    let mut cell_ui = ui.new_child(
                                        egui::UiBuilder::new()
                                            .id_salt((row_index, column))
                                            .max_rect(cell_rect)
                                            .layout(egui::Layout::top_down(egui::Align::Min)),
                                    );
                                    cell_ui.set_clip_rect(ui.clip_rect().intersect(cell_rect));
                                    cell_ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                                    render(
                                        &mut cell_ui,
                                        &mut self.cache,
                                        row.get(column).map(String::as_str).unwrap_or(""),
                                    );
                                    row_height = row_height.max(cell_ui.min_rect().height());
                                    x += width + 12.0;
                                }
                                let row_rect = egui::Rect::from_min_size(
                                    top_left,
                                    egui::vec2(table_width, row_height + 10.0),
                                );
                                if row_index % 2 == 0 {
                                    ui.painter().set(
                                        background,
                                        egui::Shape::rect_filled(
                                            row_rect,
                                            0.0,
                                            ui.visuals().faint_bg_color,
                                        ),
                                    );
                                }
                                ui.allocate_space(row_rect.size());
                            }
                        });
                    ui.add_space(8.0);
                }
            });
        }
    }
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_DEFINITION_LIST
}

fn render(ui: &mut egui::Ui, cache: &mut CommonMarkCache, markdown: &str) {
    let width = ui.available_width().max(32.0);
    CommonMarkViewer::new()
        .explicit_image_uri_scheme(true)
        .max_image_width(Some(width as usize))
        .default_width(Some(width as usize))
        .render_html_fn(Some(&render_html_images))
        .show(ui, cache, markdown);
}

fn prepare(source: &str) -> Vec<Block> {
    // Keep HTML images inline until the GFM parser has located table cells.
    let source = normalize_html(source, false);
    let parser = Parser::new_ext(&source, options());
    // Reference definitions belong to the complete README, not to an individual
    // table cell or the paragraph fragment rendered by CommonMarkViewer.
    let definitions = parser
        .reference_definitions()
        .iter()
        .map(|(_, definition)| &source[definition.span.clone()])
        .collect::<Vec<_>>()
        .join("\n");
    let fragment = |text: &str| {
        if definitions.is_empty() {
            sanitize(text)
        } else {
            sanitize(&format!("{text}\n\n{definitions}\n"))
        }
    };
    let mut blocks = Vec::new();
    let mut previous = 0;
    let mut table_start = 0;
    let mut in_table = false;
    let mut in_head = false;
    let mut header = Vec::new();
    let mut rows = Vec::new();
    let mut row = Vec::new();
    for (event, span) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Table(_)) => {
                if span.start > previous {
                    blocks.push(Block::Markdown(fragment(&source[previous..span.start])));
                }
                in_table = true;
                table_start = span.start;
                header.clear();
                rows.clear();
            }
            Event::Start(Tag::TableHead) if in_table => in_head = true,
            Event::End(TagEnd::TableHead) if in_table => in_head = false,
            Event::Start(Tag::TableRow) if in_table => row.clear(),
            Event::Start(Tag::TableCell) if in_table => {
                let cell = source[span].trim();
                if in_head {
                    header.push(fragment(&format!("**{cell}**")));
                } else {
                    row.push(fragment(cell));
                }
            }
            Event::End(TagEnd::TableRow) if in_table => rows.push(std::mem::take(&mut row)),
            Event::End(TagEnd::Table) if in_table => {
                blocks.push(Block::Table {
                    header: std::mem::take(&mut header),
                    rows: std::mem::take(&mut rows),
                });
                previous = span.end.max(table_start);
                in_table = false;
            }
            _ => {}
        }
    }
    if previous < source.len() {
        blocks.push(Block::Markdown(fragment(&source[previous..])));
    }
    blocks
}

fn sanitize(source: &str) -> String {
    let source = normalize_html(source, true);
    let events: Vec<_> = Parser::new_ext(&source, options())
        .into_offset_iter()
        .collect();
    let mut edits = Vec::new();
    for (index, (event, span)) in events.iter().enumerate() {
        let (destination, image) = match event {
            Event::Start(Tag::Image { dest_url, .. }) => (dest_url.as_ref(), true),
            Event::Start(Tag::Link { dest_url, .. }) => (dest_url.as_ref(), false),
            _ => continue,
        };
        let url = web_url(destination, image);
        if url.is_some() && !image {
            continue;
        }
        let mut label = String::new();
        let mut end = span.end;
        for (child, child_span) in &events[index + 1..] {
            if matches!(child, Event::End(TagEnd::Image)) && image
                || matches!(child, Event::End(TagEnd::Link)) && !image
            {
                end = child_span.end;
                break;
            }
            if let Event::Text(text) | Event::Code(text) = child {
                label.push_str(text);
            }
        }
        let replacement = match url {
            Some(url) => format!("![{}](<{url}>)", escape_markdown(&label)),
            None => escape_markdown(&label),
        };
        edits.push((span.start..end, replacement));
    }
    apply_edits(&source, edits)
}

fn apply_edits(source: &str, mut edits: Vec<(Range<usize>, String)>) -> String {
    edits.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(b.0.end.cmp(&a.0.end)));
    let mut output = String::with_capacity(source.len());
    let mut offset = 0;
    for (span, replacement) in edits {
        if span.start < offset {
            continue;
        }
        output.push_str(&source[offset..span.start]);
        output.push_str(&replacement);
        offset = span.end;
    }
    output.push_str(&source[offset..]);
    output
}

fn web_url(value: &str, image: bool) -> Option<String> {
    let mut url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        return None;
    }
    if image && url.host_str() == Some("github.com") {
        let path: Vec<_> = url.path().split('/').collect();
        if path.len() >= 6 && path[3] == "blob" {
            let raw_path = format!("/{}/{}/{}", path[1], path[2], path[4..].join("/"));
            url.set_host(Some("raw.githubusercontent.com")).ok()?;
            url.set_path(&raw_path);
            url.set_query(None);
            url.set_fragment(None);
        }
    }
    Some(url.into())
}

#[derive(Debug)]
struct HtmlImage {
    url: String,
    alt: String,
    width: Option<f32>,
    height: Option<f32>,
}

fn attribute(tag: &StartTag, key: &[u8]) -> Option<String> {
    tag.attributes
        .iter()
        .find(|(name, _)| name.as_ref() == key)
        .map(|(_, value)| String::from_utf8_lossy(value.as_ref()).into_owned())
}

fn html_image(tag: &StartTag) -> Option<HtmlImage> {
    if tag.name.as_ref() != b"img" {
        return None;
    }
    let dimension = |key| {
        attribute(tag, key)
            .and_then(|s| s.trim_end_matches("px").parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| v.min(2048.0))
    };
    Some(HtmlImage {
        url: web_url(&attribute(tag, b"src")?, true)?,
        alt: attribute(tag, b"alt").unwrap_or_default(),
        width: dimension(b"width"),
        height: dimension(b"height"),
    })
}

fn render_html_images(ui: &mut egui::Ui, html: &str) {
    for token in Tokenizer::new(html).flatten() {
        if let Token::StartTag(tag) = token {
            if let Some(image) = html_image(&tag) {
                let mut widget =
                    egui::Image::from_uri(image.url).max_width(ui.available_width().max(16.0));
                if let Some(width) = image.width {
                    // Reserve the requested width even while the asynchronous image is loading.
                    let height = image.height.unwrap_or_else(|| {
                        widget
                            .load_for_size(ui.ctx(), egui::vec2(width, 2048.0))
                            .ok()
                            .and_then(|loaded| loaded.size())
                            .filter(|size| size.x > 0.0)
                            .map(|size| width * size.y / size.x)
                            .unwrap_or(width)
                    });
                    widget = widget
                        .fit_to_exact_size(egui::vec2(width, height))
                        .maintain_aspect_ratio(true);
                } else {
                    widget = widget.fit_to_original_size(1.0);
                }
                ui.add(widget).on_hover_text(image.alt);
            }
        }
    }
}

fn normalize_html(source: &str, block_images: bool) -> String {
    let mut edits = Vec::new();
    let mut hidden = false;
    for (event, span) in Parser::new_ext(source, options()).into_offset_iter() {
        let (Event::Html(html) | Event::InlineHtml(html)) = event else {
            continue;
        };
        let mut output = String::new();
        for token in Tokenizer::new(html.as_ref()).flatten() {
            match token {
                Token::StartTag(tag) => {
                    let name = String::from_utf8_lossy(tag.name.as_ref());
                    if matches!(name.as_ref(), "script" | "style" | "iframe") {
                        hidden = true;
                        continue;
                    }
                    if hidden {
                        continue;
                    }
                    if name == "img" {
                        if let Some(image) = html_image(&tag) {
                            if block_images {
                                output.push_str("\n\n");
                            }
                            output.push_str(&format!(
                                "<img src=\"{}\" alt=\"{}\"",
                                escape_html(&image.url),
                                escape_html(&image.alt)
                            ));
                            if let Some(width) = image.width {
                                output.push_str(&format!(" width=\"{width}\""));
                            }
                            if let Some(height) = image.height {
                                output.push_str(&format!(" height=\"{height}\""));
                            }
                            output.push('>');
                            if block_images {
                                output.push_str("\n\n");
                            }
                        }
                    } else {
                        output.push_str(match name.as_ref() {
                            "strong" | "b" | "summary" => "**",
                            "em" | "i" => "*",
                            "del" | "s" => "~~",
                            "br" if block_images => "  \n",
                            "br" => "<br>",
                            "p" | "div" | "details" => "\n\n",
                            "h1" => "\n\n# ",
                            "h2" => "\n\n## ",
                            "h3" => "\n\n### ",
                            "hr" => "\n\n---\n\n",
                            _ => "",
                        });
                    }
                }
                Token::EndTag(tag) => {
                    let name = String::from_utf8_lossy(tag.name.as_ref());
                    if matches!(name.as_ref(), "script" | "style" | "iframe") {
                        hidden = false;
                        continue;
                    }
                    if !hidden {
                        output.push_str(match name.as_ref() {
                            "strong" | "b" => "**",
                            "summary" => "**\n\n",
                            "em" | "i" => "*",
                            "del" | "s" => "~~",
                            "p" | "div" | "details" | "h1" | "h2" | "h3" => "\n\n",
                            _ => "",
                        });
                    }
                }
                Token::String(text) if !hidden => {
                    output.push_str(&String::from_utf8_lossy(text.as_ref()))
                }
                _ => {}
            }
        }
        edits.push((span, output));
    }
    apply_edits(source, edits)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_markdown(value: &str) -> String {
    let mut result = String::new();
    for c in value.chars() {
        if "\\`*_{}[]<>()!#|".contains(c) {
            result.push('\\');
        }
        result.push(c);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gfm_table_preserves_formatted_cells_and_sized_html_images() {
        let markdown = "# Items\n\n| Image | Name | Description | Type |\n|---|---|---|---|\n|<img src=\"https://github.com/owner/repo/blob/main/item.png?raw=true\" width=\"64\" alt=\"Item\">|**Example**|A [link](https://example.com) and ~~old~~ text.|Item|\n\nAfter table.";
        let blocks = prepare(markdown);
        let Block::Table { header, rows } = &blocks[1] else {
            panic!("expected GFM table: {blocks:?}");
        };
        assert_eq!(header.len(), 4);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 4);
        assert!(rows[0][0].contains("width=\"64\""));
        assert!(rows[0][0].contains("https://raw.githubusercontent.com/owner/repo/main/item.png"));
        assert_eq!(rows[0][1], "**Example**");
        assert!(rows[0][2].contains("[link](https://example.com)"));
        assert!(
            matches!(blocks.last(), Some(Block::Markdown(text)) if text.contains("After table."))
        );
    }

    #[test]
    fn remote_markup_cannot_load_files_or_open_custom_schemes() {
        let input = "[File](file:///etc/passwd) [Run](javascript:alert) ![secret](file:///tmp/secret.png) ![relative](private.png) <img src='file:///tmp/image.png' width='64'>\n\n[Web](https://example.com)";
        let result = sanitize(input);
        assert!(!result.contains("file:"));
        assert!(!result.contains("javascript:"));
        assert!(!result.contains("private.png"));
        assert!(result.contains("[Web](https://example.com)"));
        assert!(result.contains("secret"));
    }

    #[test]
    fn code_examples_remain_literal_and_details_keep_content() {
        let source = "```html\n<img src=\"file:///demo\">\n```\n\n<details><summary>More</summary>\n\n- **First**\n- Second\n\n</details>";
        let normalized = normalize_html(source, false);
        assert!(normalized.contains("```html\n<img src=\"file:///demo\">\n```"));
        assert!(normalized.contains("**More**"));
        assert!(normalized.contains("- **First**"));
        assert!(!normalized.contains("<details>"));
    }

    #[test]
    fn html_attribute_entities_and_dimensions_are_parsed() {
        let token = Tokenizer::new("<IMG src='https://example.com/a.png?x=1&amp;y=2' width=40 height='20' alt='A &amp; B'>").flatten().next().unwrap();
        let Token::StartTag(tag) = token else {
            panic!();
        };
        let image = html_image(&tag).unwrap();
        assert_eq!(image.width, Some(40.0));
        assert_eq!(image.height, Some(20.0));
        assert_eq!(image.alt, "A & B");
        assert!(image.url.ends_with("?x=1&y=2"));
    }

    #[test]
    fn line_breaks_stay_inside_their_table_cells() {
        let blocks = prepare(
            "|Name|Description|\n|-|-|\n|First|Line one<br/>Line two|\n|Second|Another<br>line|\n",
        );
        let Block::Table { rows, .. } = &blocks[0] else {
            panic!("expected table");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
        assert!(rows[0][1].contains("Line one  \nLine two"));
        assert!(rows[1][1].contains("Another  \nline"));
    }

    #[test]
    fn document_references_work_before_inside_and_after_tables() {
        let blocks = prepare(
            "Before [docs][ref].\n\n|Name|Link|\n|-|-|\n|Item|[manual][ref]|\n\nAfter [docs][ref].\n\n[ref]: https://example.com/manual \"Manual\"\n",
        );
        let mut links = 0;
        for block in blocks {
            let fragments = match block {
                Block::Markdown(text) => vec![text],
                Block::Table { header, rows } => header
                    .into_iter()
                    .chain(rows.into_iter().flatten())
                    .collect(),
            };
            for text in fragments {
                for event in Parser::new_ext(&text, options()) {
                    if let Event::Start(Tag::Link { dest_url, .. }) = event {
                        assert_eq!(dest_url.as_ref(), "https://example.com/manual");
                        links += 1;
                    }
                }
            }
        }
        assert_eq!(links, 3);
    }
}

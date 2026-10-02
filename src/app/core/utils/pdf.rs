use std::{
    collections::HashMap,
    io::Cursor,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use base64::{Engine as _, engine::general_purpose};
use cosmic::widget::image::Handle;
use image::{ImageBuffer, Rgba};

// We do this so that we don't have to recompile the regex every time
static MD_IMG_RE: OnceLock<regex::Regex> = OnceLock::new();
static HTML_IMG_RE: OnceLock<regex::Regex> = OnceLock::new();

fn md_img_re() -> &'static regex::Regex {
    MD_IMG_RE.get_or_init(|| regex::Regex::new(r"!\[([^\]]*)\]\((\.\/[^)]+)\)").unwrap())
}

fn html_img_re() -> &'static regex::Regex {
    HTML_IMG_RE
        .get_or_init(|| regex::Regex::new(r#"<img([^>]*?)src="(\./[^"]+)"([^>]*?)/?>"#).unwrap())
}

async fn embed_local_images(content: &str, base_dir: &Path) -> String {
    let md_re = md_img_re();
    let html_re = html_img_re();

    let mut cache: HashMap<String, Option<String>> = HashMap::new();

    for cap in md_re.captures_iter(content) {
        cache.entry(cap[2].to_string()).or_insert(None);
    }
    for cap in html_re.captures_iter(content) {
        cache.entry(cap[2].to_string()).or_insert(None);
    }

    // read all images
    for (path, slot) in cache.iter_mut() {
        *slot = read_image_as_data_uri(base_dir, path).await;
    }

    // apply markdown replacements
    let result = md_re.replace_all(content, |cap: &regex::Captures| {
        let alt = &cap[1];
        let path = &cap[2];
        match cache.get(path).and_then(|v| v.as_ref()) {
            Some(data_uri) => format!("![{}]({})", alt, data_uri),
            None => cap[0].to_string(),
        }
    });

    // apply HTML replacements
    let result = html_re.replace_all(&result, |cap: &regex::Captures| {
        let before = &cap[1];
        let path = &cap[2];
        let after = &cap[3];
        match cache.get(path).and_then(|v| v.as_ref()) {
            Some(data_uri) => format!("<img{}src=\"{}\"{}/>", before, data_uri, after),
            None => cap[0].to_string(),
        }
    });

    result.into_owned()
}

async fn read_image_as_data_uri(base_dir: &Path, path: &str) -> Option<String> {
    let image_path = base_dir.join(path.trim_start_matches("./"));
    if !image_path.exists() {
        return None;
    }

    let mime = match image_path.extension().and_then(|e| e.to_str()) {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        _ => return None,
    };

    let bytes = tokio::fs::read(&image_path).await.ok()?;
    let b64 = general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{};base64,{}", mime, b64))
}

enum Special {
    Typst,
    Mermaid,
}

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Converts the markdown to HTML, replacing typst and mermaid code blocks
/// with embedded images (or an escaped code block if they can't be rendered).
fn replace_special_blocks(content: &str, typst_cache: &HashMap<String, Handle>) -> String {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd, html};

    let mut current: Option<Special> = None;
    let mut source_buf = String::new();
    let mut replacements: Vec<String> = Vec::new();

    let events: Vec<Event> = Parser::new_ext(content, Options::all())
        .filter_map(|event| match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref lang)))
                if matches!(lang.as_ref(), "typst" | "typ" | "mermaid" | "mmd") =>
            {
                current = Some(if matches!(lang.as_ref(), "typst" | "typ") {
                    Special::Typst
                } else {
                    Special::Mermaid
                });
                source_buf.clear();
                None
            }
            Event::Text(ref text) if current.is_some() => {
                source_buf.push_str(text);
                None
            }
            Event::End(TagEnd::CodeBlock) if current.is_some() => {
                let source = source_buf.trim();
                let fallback = || format!("<pre><code>{}</code></pre>\n", escape_html(source));

                let replacement = match current.take() {
                    Some(Special::Typst) => typst_cache
                        .get(source)
                        .and_then(handle_to_base64_png)
                        .map(|b64| {
                            format!(
                                "<img src=\"data:image/png;base64,{b64}\" style=\"max-width:100%;height:auto\" />\n"
                            )
                        })
                        .unwrap_or_else(fallback),
                    _ => match frostmark::render_mermaid_png(source, false, 3.0) {
                        Ok((bytes, width)) => format!(
                            "<img src=\"data:image/png;base64,{}\" width=\"{}\" style=\"display:block;margin:1em auto;max-width:100%;height:auto\" />\n",
                            general_purpose::STANDARD.encode(&bytes),
                            width.round()
                        ),
                        Err(_) => fallback(),
                    },
                };

                let placeholder = format!("SPECIAL_BLOCK_PLACEHOLDER_{}_", replacements.len());
                replacements.push(replacement);
                Some(Event::Html(placeholder.into()))
            }
            _ => Some(event),
        })
        .collect();

    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());

    for (index, replacement) in replacements.iter().enumerate() {
        out = out.replace(&format!("SPECIAL_BLOCK_PLACEHOLDER_{index}_"), replacement);
    }

    out
}

fn handle_to_base64_png(handle: &Handle) -> Option<String> {
    let Handle::Rgba {
        width,
        height,
        pixels,
        ..
    } = handle
    else {
        return None;
    };

    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(*width, *height, pixels.to_vec())?;

    let mut png_bytes: Vec<u8> = Vec::new();
    img.write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .ok()?;

    Some(general_purpose::STANDARD.encode(&png_bytes))
}

pub async fn export_pdf(
    client: gotenberg_pdf::Client,
    file_path: Option<PathBuf>,
    file_content: String,
    file_destination_path: String,
    typst_cache: HashMap<String, Handle>,
) -> Result<(), anywho::Error> {
    let (title, base_dir) = match &file_path {
        Some(path) => {
            let title = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Document")
                .to_string();
            (title, path.parent().unwrap_or(Path::new(".")).to_path_buf())
        }
        None => ("Document".to_string(), PathBuf::from(".")),
    };

    let md_html = replace_special_blocks(&file_content, &typst_cache);
    let md_html = embed_local_images(&md_html, &base_dir).await;

    let full_html = format!(
        r#"<!doctype html>
            <html lang="en">
            <head>
                <meta charset="utf-8">
                <title>{title}</title>
                <link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.11.1/styles/default.min.css">
                <script src="https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.11.1/highlight.min.js"></script>
                <script>hljs.highlightAll();</script>
                <style>
                    img {{
                        max-width: 100%;
                        height: auto;
                    }}
                </style>
            </head>
            <body>
                {md_html}
            </body>
        </html>"#
    );

    let options = gotenberg_pdf::WebOptions {
        skip_network_idle_events: Some(false),
        ..Default::default()
    };

    let pdf_bytes = client
        .pdf_from_html(&full_html, options)
        .await
        .map_err(|e| anywho::anywho!("{e}"))?;

    tokio::fs::write(file_destination_path, pdf_bytes)
        .await
        .map_err(|e| anywho::anywho!("{e}"))?;

    Ok(())
}

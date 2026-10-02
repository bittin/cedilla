use std::{cell::RefCell, collections::HashMap};

use cosmic::{
    iced::{Alignment, Length, widget::image::Handle},
    widget,
};
use typst_layout::PagedDocument;

use crate::{
    MarkWidget,
    renderer::ValidTheme,
    structs::{RenderedSpan, TypstResult},
    typst_world::MinimalWorld,
};

/// (packed text color, text size bits, trimmed source)
type Key = (u32, u32, String);

thread_local! {
    // Survives state rebuilds
    static CACHE: RefCell<HashMap<Key, Option<Handle>>> = RefCell::default();
    // Last successful render per block position, shown while the source is invalid.
    static LAST_GOOD: RefCell<HashMap<usize, Handle>> = RefCell::default();
}

impl<'a, M: Clone + 'static, T: ValidTheme + 'a> MarkWidget<'a, M, T> {
    pub fn draw_typst(&mut self, source: &str) -> RenderedSpan<'a, M, T> {
        let id = self.current_typst_id;
        self.current_typst_id += 1;

        let source = source.trim();

        let text_color = self.style.and_then(|s| s.text_color).unwrap_or_else(|| {
            let c = cosmic::theme::active().cosmic().on_bg_color();
            cosmic::iced::Color::from_rgb(c.red, c.green, c.blue)
        });
        let [r, g, b, _] = text_color.into_rgba8();
        let typst_size = self.text_size * 0.6;

        let key: Key = (
            u32::from_be_bytes([0, r, g, b]),
            typst_size.to_bits(),
            source.to_owned(),
        );

        let entry = CACHE
            .with_borrow(|c| c.get(&key).cloned())
            .unwrap_or_else(|| {
                let wrapped = format!(
                    "#set page(width: auto, height: auto, margin: 4pt, fill: none)\n\
                     #set text(fill: rgb({r}, {g}, {b}), size: {typst_size}pt)\n\
                     {source}"
                );
                let rendered = render_typst(&wrapped, 2.0).ok().map(|r| r.handle);
                CACHE.with_borrow_mut(|c| {
                    if c.len() > 64 {
                        c.clear(); // editing a block creates one key per keystroke
                    }
                    c.insert(key, rendered.clone());
                });
                rendered
            });

        let shown = match entry {
            Some(handle) => {
                LAST_GOOD.with_borrow_mut(|m| m.insert(id, handle.clone()));
                Some(handle)
            }
            None => LAST_GOOD.with_borrow(|m| m.get(&id).cloned()),
        };

        match shown {
            Some(handle) => {
                cosmic::iced::widget::column![widget::image(handle).width(Length::Shrink)]
                    .width(Length::Fill)
                    .align_x(Alignment::Center)
                    .into()
            }
            None => self.codeblock(source.to_string(), self.text_size, false),
        }
    }
}

pub fn render_typst_png(source: &str, text_pt: f32, pixel_per_pt: f32) -> Option<(Vec<u8>, f32)> {
    let wrapped = format!(
        "#set page(width: auto, height: auto, margin: 4pt, fill: none)\n\
         #set text(fill: black, size: {text_pt}pt)\n\
         {}",
        source.trim()
    );

    let world = MinimalWorld::new(&wrapped);
    let document: PagedDocument = typst::compile(&world).output.ok()?;
    let page = document.pages().first()?;
    let render_opts = typst_render::RenderOptions {
        pixel_per_pt: (pixel_per_pt as f64).into(),
        ..Default::default()
    };
    let pixmap = typst_render::render(page, &render_opts);

    let width_pt = pixmap.width() as f32 / pixel_per_pt;
    Some((pixmap.encode_png().ok()?, width_pt))
}

pub fn render_typst(source: &str, pixel_per_pt: f64) -> Result<TypstResult, ()> {
    let world = MinimalWorld::new(source);
    let document: PagedDocument = typst::compile(&world).output.map_err(|_| ())?;
    let page = &document.pages()[0];
    let render_opts = typst_render::RenderOptions {
        pixel_per_pt: pixel_per_pt.into(),
        ..Default::default()
    };
    let pixmap = typst_render::render(page, &render_opts);

    let width = pixmap.width();
    let height = pixmap.height();
    let rgba = unpremultiply(pixmap.take());

    Ok(TypstResult {
        handle: Handle::from_rgba(width, height, rgba),
    })
}

fn unpremultiply(data: Vec<u8>) -> Vec<u8> {
    data.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| {
            let [r, g, b, a] = [px[0], px[1], px[2], px[3]];
            if a == 0 {
                return [0u8, 0, 0, 0];
            }
            let a_f = a as f32 / 255.0;
            [
                (r as f32 / a_f).round() as u8,
                (g as f32 / a_f).round() as u8,
                (b as f32 / a_f).round() as u8,
                a,
            ]
        })
        .collect()
}

use std::{cell::RefCell, collections::HashMap};

use cosmic::{
    iced::{Alignment, Length, widget::image::Handle},
    widget,
};
use merman::svg::{CssOverridePolicy, SvgOutputPolicy, SvgPipelinePreset, export::RasterOptions};
use merman::svg::{HostTheme, HostThemeAppearance, Presentation, ThemeRole};
use merman::{OperationControl, PngRequest, RenderOutput, RenderRequest, Renderer, SvgRequest};

use crate::{MarkWidget, renderer::ValidTheme, structs::RenderedSpan};

const SCALE: f32 = 2.0;
type Rendered = (Handle, f32);

thread_local! {
    // Survives state rebuilds. None = failed, don't retry every frame.
    static CACHE: RefCell<HashMap<(bool, String), Option<Rendered>>> = RefCell::default();
    // Last successful render per block position, shown while the source is invalid.
    static LAST_GOOD: RefCell<HashMap<usize, Rendered>> = RefCell::default();
}

impl<'a, M: Clone + 'static, T: ValidTheme + 'a> MarkWidget<'a, M, T> {
    pub fn draw_mermaid(&mut self, source: &str) -> RenderedSpan<'a, M, T> {
        let id = self.current_mermaid_id;
        self.current_mermaid_id += 1;

        let source = source.trim();
        let is_dark = cosmic::theme::active().cosmic().is_dark;
        let key = (is_dark, source.to_owned());

        let entry = CACHE
            .with_borrow(|c| c.get(&key).cloned())
            .unwrap_or_else(|| {
               let r = render_mermaid(source, is_dark).ok();
                CACHE.with_borrow_mut(|c| {
                    if c.len() > 64 {
                        c.clear(); // editing a diagram creates one key per keystroke
                    }
                    c.insert(key, r.clone());
                });
                r
            });

        let shown = match entry {
            Some(r) => {
                LAST_GOOD.with_borrow_mut(|m| m.insert(id, r.clone()));
                Some(r)
            }
            None => LAST_GOOD.with_borrow(|m| m.get(&id).cloned()),
        };

        match shown {
            Some((handle, width)) => {
                cosmic::iced::widget::column![widget::image(handle).width(Length::Fixed(width))]
                    .width(Length::Fill)
                    .align_x(Alignment::Center)
                    .into()
            }
            None => self.codeblock(source.to_string(), self.text_size, false),
        }
    }
}

struct Look {
    canvas: &'static str,
    surface: &'static str,
    surface_alt: &'static str,
    text: &'static str,
    subtle: &'static str,
    border: &'static str,
    line: &'static str,
    cluster_bg: &'static str,
    cluster_border: &'static str,
    note_bg: &'static str,
    note_border: &'static str,
    note_text: &'static str,
    series: [&'static str; 6],
}

const DARK: Look = Look {
    canvas: "#0f172a",
    surface: "#1e293b",
    surface_alt: "#273549",
    text: "#e2e8f0",
    subtle: "#94a3b8",
    border: "#60a5fa",
    line: "#94a3b8",
    cluster_bg: "#162033",
    cluster_border: "#334155",
    note_bg: "#422006",
    note_border: "#f59e0b",
    note_text: "#fef3c7",
    series: ["#60a5fa", "#34d399", "#fbbf24", "#f472b6", "#a78bfa", "#22d3ee"],
};

const LIGHT: Look = Look {
    canvas: "#ffffff",
    surface: "#eff6ff",
    surface_alt: "#f1f5f9",
    text: "#0f172a",
    subtle: "#475569",
    border: "#3b82f6",
    line: "#64748b",
    cluster_bg: "#f8fafc",
    cluster_border: "#cbd5e1",
    note_bg: "#fef9c3",
    note_border: "#eab308",
    note_text: "#713f12",
    series: ["#3b82f6", "#10b981", "#f59e0b", "#ec4899", "#8b5cf6", "#06b6d4"],
};

pub fn render_mermaid_png(
    source: &str,
    is_dark: bool,
    scale: f32,
) -> Result<(Vec<u8>, f32), Box<dyn std::error::Error>> {
    let l = if is_dark { &DARK } else { &LIGHT };

    let theme = HostTheme::new()
        .with_appearance(if is_dark {
            HostThemeAppearance::Dark
        } else {
            HostThemeAppearance::Light
        })
        .try_with_role(ThemeRole::Canvas, l.canvas)?
        .try_with_role(ThemeRole::Surface, l.surface)?
        .try_with_role(ThemeRole::SurfaceAlt, l.surface_alt)?
        .try_with_role(ThemeRole::SurfaceMuted, l.surface_alt)?
        .try_with_role(ThemeRole::Text, l.text)?
        .try_with_role(ThemeRole::SubtleText, l.subtle)?
        .try_with_role(ThemeRole::Border, l.border)?
        .try_with_role(ThemeRole::Line, l.line)?
        .try_with_role(ThemeRole::EdgeLabelBackground, l.canvas)?
        .try_with_role(ThemeRole::ClusterBackground, l.cluster_bg)?
        .try_with_role(ThemeRole::ClusterBorder, l.cluster_border)?
        .try_with_role(ThemeRole::NoteBackground, l.note_bg)?
        .try_with_role(ThemeRole::NoteBorder, l.note_border)?
        .try_with_role(ThemeRole::NoteText, l.note_text)?
        .try_with_role(ThemeRole::ActorBackground, l.surface)?
        .try_with_role(ThemeRole::ActorBorder, l.border)?
        .try_with_role(ThemeRole::ActorText, l.text)?
        .try_with_role(ThemeRole::ActivationBackground, l.surface_alt)?
        .try_with_role(ThemeRole::ActivationBorder, l.border)?
        .try_with_series_palette(l.series)?;

    let policy = SvgOutputPolicy {
        preset: SvgPipelinePreset::ResvgSafe,
        css_override_policy: CssOverridePolicy::StripExistingImportant,
        root_background_color: Some(l.canvas.to_string()),
        ..SvgOutputPolicy::default()
    };

    let resolved = Presentation::new().with_theme(theme).resolve();
    let renderer =
        Renderer::new().with_engine(resolved.materialize_engine(merman::Engine::new()));

    let output = renderer.render(RenderRequest::png(
        source,
        OperationControl::new(),
        PngRequest {
            svg: SvgRequest {
                pipeline: Some(policy.pipeline()),
                presentation: resolved.render_policy(),
                ..Default::default()
            },
            options: RasterOptions::default().with_scale(scale),
        },
    ))?;

    let RenderOutput::Png(Some(png)) = output else {
        return Err("no mermaid diagram detected".into());
    };

    let width = png.plan.width_px as f32 / png.plan.effective_scale as f32;
    Ok((png.bytes, width))
}

fn render_mermaid(source: &str, is_dark: bool) -> Result<Rendered, Box<dyn std::error::Error>> {
    let (bytes, width) = render_mermaid_png(source, is_dark, SCALE)?;

    let rgba = ::image::load_from_memory_with_format(&bytes, ::image::ImageFormat::Png)?
        .into_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((Handle::from_rgba(w, h, rgba.into_raw()), width))
}

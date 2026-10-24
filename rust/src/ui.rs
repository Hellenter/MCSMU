//! Оформление и самописные виджеты: тема, карточки, графики.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};

pub const GREEN: Color32 = Color32::from_rgb(76, 175, 80);
pub const AMBER: Color32 = Color32::from_rgb(230, 162, 20);
pub const RED: Color32 = Color32::from_rgb(229, 83, 75);
pub const BLUE: Color32 = Color32::from_rgb(66, 135, 245);
pub const GRAY: Color32 = Color32::from_rgb(130, 130, 130);

pub fn apply_theme(ctx: &egui::Context, dark: bool) {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let radius = CornerRadius::same(4);
    v.window_corner_radius = CornerRadius::same(10);
    v.widgets.noninteractive.corner_radius = radius;
    v.widgets.inactive.corner_radius = radius;
    v.widgets.hovered.corner_radius = radius;
    v.widgets.active.corner_radius = radius;
    v.widgets.open.corner_radius = radius;
    v.selection.bg_fill = if dark { Color32::from_rgb(46, 110, 60) } else { Color32::from_rgb(170, 220, 175) };
    v.selection.stroke = Stroke::new(1.0_f32, if dark { Color32::from_rgb(140, 230, 150) } else { Color32::from_rgb(30, 90, 40) });
    v.hyperlink_color = BLUE;
    if dark {
        v.panel_fill = Color32::from_rgb(27, 29, 33);
        v.window_fill = Color32::from_rgb(34, 37, 42);
        v.extreme_bg_color = Color32::from_rgb(18, 19, 22);
        v.faint_bg_color = Color32::from_rgb(33, 36, 41);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(45, 49, 56);
        v.widgets.inactive.bg_fill = Color32::from_rgb(45, 49, 56);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(58, 63, 72);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(72, 78, 88));
    }
    // Тема задаётся явно: иначе egui следует системной теме Windows и затирает наши цвета при старте
    let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(theme);
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        use egui::{FontFamily, FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::new(14.5, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(14.5, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Small, FontId::new(12.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(22.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(13.5, FontFamily::Monospace));
    });
}

pub fn card_frame(ui: &Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(8)
        .inner_margin(egui::Margin::same(10))
}

pub fn status_dot(ui: &mut Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size * 0.4, color);
}

/// Карточка "заголовок / значение" фиксированного размера.
pub fn stat_card(ui: &mut Ui, title: &str, value: &str, accent: Option<Color32>) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(168.0, 60.0), Sense::hover());
    let vis = ui.visuals();
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 8.0, vis.faint_bg_color);
    p.rect_stroke(rect, 8.0, vis.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
    p.text(rect.left_top() + Vec2::new(10.0, 8.0), egui::Align2::LEFT_TOP, title, egui::FontId::proportional(12.0), vis.weak_text_color());
    let galley = ui.painter().layout(value.to_string(), egui::FontId::proportional(18.0),
                                     accent.unwrap_or(vis.strong_text_color()), f32::INFINITY);
    let fits = galley.size().x <= rect.width() - 20.0;
    p.galley(rect.left_top() + Vec2::new(10.0, 28.0), galley, Color32::WHITE);
    if !fits {
        resp.on_hover_text(value);
    }
}

/// Линейный график с заливкой и подсказкой по наведению.
pub fn chart(ui: &mut Ui, title: &str, data: &[Option<f32>], min_top: f32, color: Color32, unit: &str, decimals: usize, height: f32) {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let p = ui.painter_at(rect);
    let vis = ui.visuals();
    p.rect_filled(rect, 8.0, vis.faint_bg_color);
    p.rect_stroke(rect, 8.0, vis.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
    let vals: Vec<f32> = data.iter().flatten().copied().collect();
    let top = vals.iter().copied().fold(min_top.max(1.0), |a, b| a.max(b * 1.15));
    let cur = vals.last().map(|v| format!("{:.*} {}", decimals, v, unit)).unwrap_or("—".into());
    let text_color = vis.text_color();
    p.text(rect.left_top() + Vec2::new(10.0, 7.0), egui::Align2::LEFT_TOP, format!("{}  ", title),
           egui::FontId::proportional(13.0), vis.weak_text_color());
    let title_w = 10.0 + 7.5 * title.chars().count() as f32 + 8.0;
    p.text(rect.left_top() + Vec2::new(title_w, 5.0), egui::Align2::LEFT_TOP, cur,
           egui::FontId::proportional(15.0), text_color);
    p.text(rect.right_top() + Vec2::new(-10.0, 7.0), egui::Align2::RIGHT_TOP,
           format!("шкала {:.0} {}", top, unit), egui::FontId::proportional(11.0), vis.weak_text_color());
    let plot = Rect::from_min_max(rect.left_top() + Vec2::new(8.0, 28.0), rect.right_bottom() - Vec2::new(8.0, 8.0));
    for i in 1..4 {
        let y = plot.top() + plot.height() * i as f32 / 4.0;
        p.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                       Stroke::new(1.0_f32, vis.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.5)));
    }
    if data.len() < 2 {
        p.text(plot.center(), egui::Align2::CENTER_CENTER, "нет данных", egui::FontId::proportional(12.0), vis.weak_text_color());
        return;
    }
    let n = data.len();
    let step = plot.width() / (n - 1).max(1) as f32;
    let pt = |i: usize, v: f32| Pos2::new(plot.left() + i as f32 * step, plot.bottom() - plot.height() * (v / top).min(1.0));
    let fill = color.gamma_multiply(0.18);
    let mut line = vec![];
    for i in 0..n {
        if let Some(v) = data[i] {
            let a = pt(i, v);
            line.push(a);
            if i + 1 < n {
                if let Some(v2) = data[i + 1] {
                    let b = pt(i + 1, v2);
                    p.add(Shape::convex_polygon(
                        vec![a, b, Pos2::new(b.x, plot.bottom()), Pos2::new(a.x, plot.bottom())],
                        fill,
                        Stroke::NONE,
                    ));
                }
            }
        } else if !line.is_empty() {
            p.add(Shape::line(std::mem::take(&mut line), Stroke::new(2.0_f32, color)));
        }
    }
    if line.len() >= 2 {
        p.add(Shape::line(line, Stroke::new(2.0_f32, color)));
    }
    if let Some(pos) = resp.hover_pos() {
        let i = (((pos.x - plot.left()) / step).round().max(0.0) as usize).min(n - 1);
        if let Some(v) = data[i] {
            let at = pt(i, v);
            p.line_segment([Pos2::new(at.x, plot.top()), Pos2::new(at.x, plot.bottom())], Stroke::new(1.0_f32, vis.weak_text_color()));
            p.circle_filled(at, 4.0, color);
            let secs_ago = (n - 1 - i) * 2;
            p.text(at + Vec2::new(8.0, -8.0), egui::Align2::LEFT_BOTTOM,
                   format!("{:.*} {} · {} с назад", decimals, v, unit, secs_ago),
                   egui::FontId::proportional(12.0), text_color);
        }
    }
}

/// Значок окна: блок травы 32×32.
pub fn app_icon() -> egui::IconData {
    let size = 32u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let noise = ((x * 7 + y * 13) ^ (x * y)) % 5;
            let (r, g, b) = if y < 10 || (y < 13 && (x * 5 + 3) % 7 < 3) {
                (70 + noise as u8 * 6, 160 + noise as u8 * 8, 60)
            } else {
                (130 + noise as u8 * 8, 90 + noise as u8 * 5, 55)
            };
            let border = x == 0 || y == 0 || x == size - 1 || y == size - 1;
            let (r, g, b) = if border { (r / 2, g / 2, b / 2) } else { (r, g, b) };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    egui::IconData { rgba, width: size, height: size }
}

pub fn fmt_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (days, h, m, sec) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60, s % 60);
    if days > 0 {
        format!("{}д {:02}:{:02}:{:02}", days, h, m, sec)
    } else {
        format!("{:02}:{:02}:{:02}", h, m, sec)
    }
}

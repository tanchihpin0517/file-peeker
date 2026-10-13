//! Icons drawn with the painter rather than loaded as assets.
//!
//! Two reasons: no image decoder or asset pipeline in a demo, and hand-drawn
//! shapes sit on the pixel grid at the one size the list uses, which emoji and
//! font glyphs do not.

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2, epaint::PathShape};

/// A closed or open folder, 16x14 within `rect`.
pub(crate) fn folder(painter: &Painter, rect: Rect, tint: Color32, open: bool) {
    let body = Rect::from_min_max(
        Pos2::new(rect.min.x, rect.min.y + 2.5),
        Pos2::new(rect.max.x, rect.max.y),
    );
    let tab = Rect::from_min_max(
        Pos2::new(rect.min.x, rect.min.y),
        Pos2::new(rect.min.x + rect.width() * 0.46, rect.min.y + 4.0),
    );
    painter.rect_filled(tab, 1.0, tint.gamma_multiply(0.75));
    if open {
        // The open state leans the body forward and lightens its lid, which
        // reads at 16px where a rotated chevron does not.
        painter.rect_filled(body, 2.0, tint.gamma_multiply(0.55));
        let lid = Rect::from_min_max(body.min, Pos2::new(body.max.x, body.min.y + 3.0));
        painter.rect_filled(lid, 2.0, tint);
    } else {
        painter.rect_filled(body, 2.0, tint);
    }
}

/// A sheet with a folded corner.
pub(crate) fn file(painter: &Painter, rect: Rect, tint: Color32) {
    let fold = 4.5;
    let points = vec![
        rect.min,
        Pos2::new(rect.max.x - fold, rect.min.y),
        Pos2::new(rect.max.x, rect.min.y + fold),
        Pos2::new(rect.max.x, rect.max.y),
        Pos2::new(rect.min.x, rect.max.y),
    ];
    painter.add(Shape::Path(PathShape {
        points,
        closed: true,
        fill: tint.gamma_multiply(0.85),
        stroke: Stroke::NONE.into(),
    }));
    painter.add(Shape::Path(PathShape {
        points: vec![
            Pos2::new(rect.max.x - fold, rect.min.y),
            Pos2::new(rect.max.x - fold, rect.min.y + fold),
            Pos2::new(rect.max.x, rect.min.y + fold),
        ],
        closed: true,
        fill: tint.gamma_multiply(0.45),
        stroke: Stroke::NONE.into(),
    }));
}

/// The expander: `>` when closed, rotated a quarter turn when open.
pub(crate) fn chevron(painter: &Painter, center: Pos2, colour: Color32, open: bool) {
    let arm = 3.2;
    let (a, b, c) = if open {
        (
            Pos2::new(center.x - arm, center.y - arm * 0.55),
            Pos2::new(center.x, center.y + arm * 0.65),
            Pos2::new(center.x + arm, center.y - arm * 0.55),
        )
    } else {
        (
            Pos2::new(center.x - arm * 0.55, center.y - arm),
            Pos2::new(center.x + arm * 0.65, center.y),
            Pos2::new(center.x - arm * 0.55, center.y + arm),
        )
    };
    let stroke = Stroke::new(1.5, colour);
    painter.line_segment([a, b], stroke);
    painter.line_segment([b, c], stroke);
}

/// A status dot with a soft halo, for connection rows.
pub(crate) fn status_dot(painter: &Painter, center: Pos2, colour: Color32) {
    painter.circle_filled(center, 5.0, colour.gamma_multiply(0.22));
    painter.circle_filled(center, 2.6, colour);
}

/// The macOS window buttons, drawn by us because the native ones are hidden.
pub(crate) fn traffic_lights(painter: &Painter, left: Pos2) {
    for (colour, index) in [
        Color32::from_rgb(0xff, 0x5f, 0x57),
        Color32::from_rgb(0xfe, 0xbc, 0x2e),
        Color32::from_rgb(0x28, 0xc8, 0x40),
    ]
    .into_iter()
    .zip(0_u8..)
    {
        painter.circle_filled(
            Pos2::new(left.x + f32::from(index) * 20.0, left.y),
            6.0,
            colour,
        );
    }
}

/// A thin indeterminate bar: a bright segment sliding along a track.
pub(crate) fn indeterminate(
    painter: &Painter,
    track: Rect,
    time: f64,
    track_colour: Color32,
    colour: Color32,
) {
    painter.rect_filled(track, 1.0, track_colour);
    let span = track.width() * 0.28;
    let cycle = (time * 0.55).fract();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a phase in 0..1 loses nothing that a 3px bar can show"
    )]
    let phase = (cycle as f32) * (track.width() + span) - span;
    let bar = Rect::from_min_size(
        Pos2::new(track.min.x + phase.max(0.0), track.min.y),
        Vec2::new(
            span.min(track.max.x - (track.min.x + phase.max(0.0)))
                .max(0.0),
            track.height(),
        ),
    );
    painter.rect_filled(bar, 1.0, colour);
}

/// A hairline on one edge of a rect — the app's only kind of border.
pub(crate) fn edge(painter: &Painter, rect: Rect, colour: Color32, side: Side) {
    let (a, b) = match side {
        Side::Top => (rect.left_top(), rect.right_top()),
        Side::Bottom => (rect.left_bottom(), rect.right_bottom()),
        Side::Right => (rect.right_top(), rect.right_bottom()),
    };
    painter.line_segment([a, b], Stroke::new(1.0, colour));
}

#[derive(Clone, Copy)]
pub(crate) enum Side {
    Top,
    Bottom,
    Right,
}

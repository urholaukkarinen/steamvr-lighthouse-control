use egui::{lerp, vec2, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Widget};

/// Progress spinner extracted from egui::ProgressBar
#[derive(Default)]
pub struct Spinner;

impl Widget for Spinner {
    fn ui(self, ui: &mut Ui) -> Response {
        let height = ui.spacing().interact_size.y;
        let desired_width = height;
        let (outer_rect, response) =
            ui.allocate_exact_size(vec2(desired_width, height), Sense::hover());

        let visuals = ui.style().visuals.clone();
        let corner_radius = outer_rect.height() / 2.0;
        let inner_rect = Rect::from_min_size(
            outer_rect.min,
            vec2(outer_rect.height(), outer_rect.height()),
        );

        let n_points = 20;
        let start_angle = ui.input().time as f64 * 360f64.to_radians();
        let end_angle = start_angle + 240f64.to_radians() * ui.input().time.sin();
        let circle_radius = corner_radius - 2.0;
        let points: Vec<Pos2> = (0..n_points)
            .map(|i| {
                let angle = lerp(start_angle..=end_angle, i as f64 / n_points as f64);
                let (sin, cos) = angle.sin_cos();
                inner_rect.right_center()
                    + circle_radius * vec2(cos as f32, sin as f32)
                    + vec2(-corner_radius, 0.0)
            })
            .collect();

        ui.painter()
            .add(Shape::line(points, Stroke::new(2.0, visuals.text_color())));

        response
    }
}

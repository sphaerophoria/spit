use eframe::{
    egui::{
        pos2, vec2, NumExt, Response, Sense, Shape, TextStyle, Ui, Widget, WidgetInfo, WidgetText,
        WidgetType,
    },
    epaint,
};

/// Blatantly copy pasted from egui::Checkbox and modified to add a desired state
/// Uses desired_state as the WidgetInfo output, as that's the item that we actually care about
#[must_use = "You should put this widget in an ui with `ui.add(widget);`"]
pub struct TristateCheckbox<'a> {
    checked: &'a bool,
    desired_state: &'a mut bool,
    text: WidgetText,
}

impl<'a> TristateCheckbox<'a> {
    pub fn new(
        checked: &'a bool,
        desired_state: &'a mut bool,
        text: impl Into<WidgetText>,
    ) -> Self {
        TristateCheckbox {
            checked,
            desired_state,
            text: text.into(),
        }
    }
}

impl<'a> Widget for TristateCheckbox<'a> {
    fn ui(self, ui: &mut Ui) -> Response {
        let TristateCheckbox {
            checked,
            desired_state,
            text,
        } = self;

        let spacing = &ui.spacing();
        let icon_width = spacing.icon_width;
        let icon_spacing = ui.spacing().icon_spacing;
        let button_padding = spacing.button_padding;
        let total_extra = button_padding + vec2(icon_width + icon_spacing, 0.0) + button_padding;

        let wrap_width = ui.available_width() - total_extra.x;
        let text = text.into_galley(ui, None, wrap_width, TextStyle::Button);

        let mut desired_size = total_extra + text.size();
        desired_size = desired_size.at_least(spacing.interact_size);
        desired_size.y = desired_size.y.max(icon_width);
        let (rect, mut response) = ui.allocate_exact_size(desired_size, Sense::click());

        if response.clicked() {
            *desired_state = !*desired_state;
            response.mark_changed();
        }
        response.widget_info(|| {
            WidgetInfo::selected(WidgetType::Checkbox, *desired_state, text.text())
        });

        if ui.is_rect_visible(rect) {
            // let visuals = ui.style().interact_selectable(&response, *checked); // too colorful
            let visuals = ui.style().interact(&response);
            let text_pos = pos2(
                rect.min.x + button_padding.x + icon_width + icon_spacing,
                rect.center().y - 0.5 * text.size().y,
            );
            let (small_icon_rect, big_icon_rect) = ui.spacing().icon_rectangles(rect);
            ui.painter().add(epaint::RectShape {
                rect: big_icon_rect.expand(visuals.expansion),
                rounding: visuals.rounding,
                fill: visuals.bg_fill,
                fill_texture_id: Default::default(),
                uv: epaint::Rect::ZERO,
                stroke: visuals.bg_stroke,
            });

            if *desired_state != *checked {
                ui.painter().add(Shape::line(
                    vec![
                        pos2(small_icon_rect.left(), small_icon_rect.center().y),
                        pos2(small_icon_rect.right(), small_icon_rect.center().y),
                    ],
                    visuals.fg_stroke,
                ));
            } else if *checked {
                // Check mark:
                ui.painter().add(Shape::line(
                    vec![
                        pos2(small_icon_rect.left(), small_icon_rect.center().y),
                        pos2(small_icon_rect.center().x, small_icon_rect.bottom()),
                        pos2(small_icon_rect.right(), small_icon_rect.top()),
                    ],
                    visuals.fg_stroke,
                ));
            }

            ui.painter().galley(text_pos, text, visuals.text_color());
        }

        response
    }
}

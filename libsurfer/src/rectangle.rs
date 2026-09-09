use crate::{
    annotation::{Annotatable, AnnotationData},
    annotation_list::DEFAULT_GROUP_NAME,
    comment::Comment,
    config::SurferTheme,
    displayed_item::DisplayedItemRef,
    graphics::{Anchor, GraphicsY},
    message::Message,
    time::TimeFormatter,
    view::DrawingContext,
    viewport::Viewport,
    wave_data::WaveData,
};
use egui::{Id, Pos2, Rect, Response, Sense, Stroke, Ui, Widget};
use emath::RectTransform;
use num::BigInt;

const DEFAULT_TYPE: &str = "Rectangle";
const SELECTED_GAMMA_FACTOR: f32 = 1.1;
const SELECTED_WIDTH_FACTOR: f32 = 1.3;
const HITBOX_SIZE_FACTOR: f32 = 3.;

#[derive(Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct AnchorPoint {
    pub wave: Option<GraphicsY>,
    pub time: BigInt,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RectAnnotation {
    pub annotation_data: AnnotationData,
    pub from: AnchorPoint,
    pub to: AnchorPoint,
    pub rect: Rect,
}

impl RectAnnotation {
    pub(crate) fn new(
        id: Id,
        time_at_start: BigInt,
        time_at_end: BigInt,
        wave_from: Option<GraphicsY>,
        wave_to: Option<GraphicsY>,
        rect: Rect,
        num: i32,
    ) -> Self {
        let name = format!("{DEFAULT_TYPE} {num}");
        let annotation_data = AnnotationData::new(id, name, num);
        Self {
            annotation_data,
            from: AnchorPoint {
                wave: wave_from,
                time: time_at_start,
            },
            to: AnchorPoint {
                wave: wave_to,
                time: time_at_end,
            },
            rect,
        }
    }
    #[must_use]
    pub fn get_id(&self) -> Id {
        self.annotation_data.id
    }

    #[must_use]
    pub fn get_pos(
        &self,
        waves: &WaveData,
        viewport: &Viewport,
        ctx: &DrawingContext,
        y_offset: f32,
    ) -> Option<Pos2> {
        let range = waves.time_range();

        let x = viewport.pixel_from_time(&self.from.time, ctx.cfg.canvas_size.x, range);

        let from_y = self.from.wave.as_ref().and_then(|f| waves.get_item_y(f))?;
        let to_y = self.to.wave.as_ref().and_then(|to| waves.get_item_y(to))?;

        let min_y = (from_y + y_offset).min(to_y + y_offset);

        Some((ctx.to_screen)(x, min_y))
    }

    //Find the correct y_positions for the rectangle. If the "p" value is none, it means we have a snapped value
    //and make sure to anchor them correctly.
    fn resolve_y_positions(&mut self, waves: &WaveData) -> (Option<f32>, Option<f32>) {
        let mut from_y = calculate_y(self.from.wave.as_ref(), waves);
        let mut to_y = calculate_y(self.to.wave.as_ref(), waves);

        if from_y >= to_y {
            if let Some(wave_from) = self.from.wave.as_mut()
                && matches!(wave_from.anchor, Anchor::Top)
            {
                wave_from.anchor = Anchor::Bottom;
                from_y = calculate_y(Some(wave_from), waves);
            }

            if let Some(wave_to) = self.to.wave.as_mut()
                && matches!(wave_to.anchor, Anchor::Bottom)
            {
                wave_to.anchor = Anchor::Top;
                to_y = calculate_y(Some(wave_to), waves);
            }
        }
        (from_y, to_y)
    }

    /// Calculate the correct position of the rectangle on to the canvas.
    #[allow(clippy::too_many_arguments)]
    fn compute_rect(
        &mut self,
        from_y: f32,
        to_y: f32,
        waves: &WaveData,
        ctx: &DrawingContext,
        viewport_idx: usize,
        theme: &SurferTheme,
        y_offset: f32,
    ) {
        let viewport = waves.viewports[viewport_idx];
        let range = waves.time_range();

        //Update size and coloring from theme and whether it selected or not
        self.annotation_data.stroke = Stroke::new(
            theme.annotation_rectangle.width,
            theme.annotation_rectangle.color,
        );
        // y_offset adjusts positioning whether the default timeline is shown or not.
        let min_y = from_y.min(to_y) + y_offset;
        let max_y = from_y.max(to_y) + y_offset;

        let min_x = viewport.pixel_from_time(&self.from.time, ctx.cfg.canvas_size.x, range);
        let max_x = viewport.pixel_from_time(&self.to.time, ctx.cfg.canvas_size.x, range);

        self.rect = Rect {
            min: (ctx.to_screen)(min_x, min_y),
            max: (ctx.to_screen)(max_x, max_y),
        }
    }
}

pub(crate) fn calculate_y(wave: Option<&GraphicsY>, waves: &WaveData) -> Option<f32> {
    wave.and_then(|from| waves.get_item_y(from))
}

impl Annotatable for RectAnnotation {
    fn get_id(&self) -> Id {
        self.annotation_data.id
    }

    fn get_type(&self) -> &str {
        DEFAULT_TYPE
    }

    fn set_name(&mut self, name: &str) {
        self.annotation_data.name = name.to_string();
    }

    fn get_name(&self) -> String {
        self.annotation_data.name.clone()
    }

    fn is_selected(&mut self) {
        self.annotation_data.stroke.width *= SELECTED_WIDTH_FACTOR;
        self.annotation_data
            .stroke
            .color
            .gamma_multiply(SELECTED_GAMMA_FACTOR);
    }

    fn set_visibility(&mut self, visible: bool) {
        self.annotation_data.visible = visible;
    }

    fn show_comments(&self) -> bool {
        self.annotation_data.show_comments
    }

    fn show_comment_box(&self) -> bool {
        self.annotation_data.comment_box.visible
    }

    fn set_show_comments(&mut self, show: bool) {
        self.annotation_data.show_comments = show;
    }

    fn get_comment_box(&self) -> Comment {
        self.annotation_data.comment_box.clone()
    }

    fn get_comment_box_mut(&mut self) -> &mut Comment {
        &mut self.annotation_data.comment_box
    }

    fn get_messages(&self) -> Vec<crate::comment::CommentMessage> {
        self.annotation_data.comment_box.message_chain.clone()
    }

    fn is_visible(&self) -> bool {
        self.annotation_data.visible
    }

    fn get_center_time(&self) -> BigInt {
        (&self.from.time + &self.to.time) / 2
    }

    fn get_start_time(&self) -> BigInt {
        self.from.time.clone()
    }

    fn get_end_time(&self) -> BigInt {
        self.to.time.clone()
    }

    fn is_attached(&self, removed_ref: &DisplayedItemRef) -> bool {
        self.from
            .wave
            .as_ref()
            .is_some_and(|wave| &wave.item == removed_ref)
            || self
                .to
                .wave
                .as_ref()
                .is_some_and(|wave| &wave.item == removed_ref)
    }

    fn get_from_wave(&self) -> Option<GraphicsY> {
        self.from.wave.clone()
    }

    fn get_to_wave(&self) -> Option<GraphicsY> {
        self.to.wave.clone()
    }

    fn draw(
        &self,
        ui: &mut Ui,
        waves: &WaveData,
        viewport_idx: usize,
        ctx: &mut DrawingContext,
        theme: &SurferTheme,
        msgs: &mut Vec<Message>,
        y_offset: f32,
        to_screen: RectTransform,
        time_formatter: &TimeFormatter,
    ) {
        let mut rectangle_annotation = self.clone();

        rectangle_annotation.annotation_data.id =
            egui::Id::new(("rectangle", self.annotation_data.id, viewport_idx));

        if let (Some(from_y), Some(to_y)) = rectangle_annotation.resolve_y_positions(waves) {
            rectangle_annotation.compute_rect(
                from_y,
                to_y,
                waves,
                ctx,
                viewport_idx,
                theme,
                y_offset,
            );

            if waves.selected_annotation == Some(self.get_id()) {
                rectangle_annotation.is_selected();
            }

            let hover_start_time = time_formatter.format(&self.from.time);
            let hover_end_time = time_formatter.format(&self.to.time);

            let group_name = waves
                .annotation_groups
                .iter()
                .find(|group| group.annotations.contains(&self.get_id()))
                .map_or(DEFAULT_GROUP_NAME, |group| &group.name);
            let res = ui.add(rectangle_annotation).on_hover_ui(|ui| {
                self.draw_hover_info(group_name, ui, (&hover_start_time, &hover_end_time));
            });

            if res.clicked_by(egui::PointerButton::Primary) {
                msgs.push(Message::SetActiveViewport(viewport_idx));
                msgs.push(Message::AnnotationClicked(
                    Some(self.annotation_data.id),
                    res.interact_pointer_pos(),
                    Some(viewport_idx),
                    Some(to_screen),
                    Some(ctx.cfg.canvas_size.x),
                ));
                msgs.push(Message::ClickHandled());
            }
        }
    }

    fn get_comment_position(
        &self,
        viewport: &Viewport,
        ctx: &DrawingContext,
        waves: &WaveData,
        offset: f32,
    ) -> Option<Pos2> {
        let range = waves.time_range();
        let x = viewport.pixel_from_time(&self.to.time, ctx.cfg.canvas_size.x, range);
        let y = calculate_y(self.to.wave.as_ref(), waves)? + offset;
        Some((ctx.to_screen)(x, y))
    }

    fn get_time_info(&self, time_formatter: &TimeFormatter) -> String {
        format!(
            "from: {}, to: {}",
            time_formatter.format(&self.from.time),
            time_formatter.format(&self.to.time)
        )
    }
}

/// Creates an outer and inner rectangle, used to identify whether the annotation was clicked on.
fn point_on_rect_border(p: emath::Pos2, rect: Rect, width: f32) -> (bool, Rect) {
    let half_width: f32 = width * HITBOX_SIZE_FACTOR;
    let outer_rect = Rect {
        min: emath::Pos2 {
            x: rect.min.x - half_width,
            y: rect.min.y - half_width,
        },
        max: emath::Pos2 {
            x: rect.max.x + half_width,
            y: rect.max.y + half_width,
        },
    };
    let inner_rect = Rect {
        min: emath::Pos2 {
            x: rect.min.x + half_width,
            y: rect.min.y + half_width,
        },
        max: emath::Pos2 {
            x: rect.max.x - half_width,
            y: rect.max.y - half_width,
        },
    };
    (
        outer_rect.contains(p) && !inner_rect.contains(p),
        outer_rect,
    )
}

impl Widget for RectAnnotation {
    fn ui(self, ui: &mut Ui) -> Response {
        if self.is_visible() {
            ui.painter().rect_stroke(
                self.rect,
                0.0,
                self.annotation_data.stroke,
                egui::StrokeKind::Middle,
            );
            // Always draw the rectangle but if we are on border we should also register clicks.
            // This allows the click to be transferred unto the underlying panel so the rectangle is hollow
            let (on_border, hitbox) = ui
                .ctx()
                .pointer_hover_pos()
                .map_or((false, Rect::ZERO), |p| {
                    point_on_rect_border(p, self.rect, self.annotation_data.stroke.width)
                });

            if on_border {
                ui.interact(hitbox, self.annotation_data.id, Sense::click_and_drag())
            } else {
                ui.allocate_response(egui::Vec2::ZERO, Sense::empty())
            }
        } else {
            let rect = self.hide_annotation(ui, self.annotation_data.stroke, self.rect.min);
            ui.interact(rect, self.annotation_data.id, Sense::click_and_drag())
        }
    }
}

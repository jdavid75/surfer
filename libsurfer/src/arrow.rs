use crate::annotation::{Annotatable, AnnotationData};
use crate::annotation_list::DEFAULT_GROUP_NAME;
use crate::comment::Comment;
use crate::config::SurferTheme;
use crate::displayed_item::DisplayedItemRef;
use crate::graphics::GraphicsY;
use crate::message::Message;
use crate::time::TimeFormatter;
use crate::{Viewport, view::DrawingContext, wave_data::WaveData};

use chrono::{DateTime, Local};
use egui::{Id, Pos2, Response, Stroke, Ui, Vec2, Widget};
use emath::RectTransform;
use num::BigInt;
use serde::{Deserialize, Serialize};

const DEFAULT_TYPE: &str = "Arrow";
const SELECTED_GAMMA_FACTOR: f32 = 1.1;
const SELECTED_WIDTH_FACTOR: f32 = 1.2;
const HITBOX_SIZE: f32 = 4.0;
const HEAD_LEN_FACTOR: f32 = 5.0;
const HEAD_WIDTH_FACTOR: f32 = 3.0;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum ArrowHeadMode {
    /// One-headed arrow, with the head at the target/end point.
    End,
    /// Double-headed arrow, with heads at both the start and end points.
    Double,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct WavePoint {
    pub time: BigInt,
    pub attached_item: Option<DisplayedItemRef>,
    pub screen_pos: Pos2,
}

#[derive(Clone, Copy, Debug)]
struct ArrowSegments {
    shaft_start: Pos2,
    shaft_end: Pos2,
    end_tip: Pos2,
    end_left: Pos2,
    end_right: Pos2,
    start_tip: Option<Pos2>,
    start_left: Option<Pos2>,
    start_right: Option<Pos2>,
}

/// Returns the shortest distance between point `p` and the line segment `a -> b`.
fn distance_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let ap = p - a;

    let ab_len_sq = ab.length_sq();
    if ab_len_sq <= 0.0001 {
        return ap.length();
    }

    let t = (ap.dot(ab) / ab_len_sq).clamp(0.0, 1.0);
    let closest = a + ab * t;
    (p - closest).length()
}

/// Calculates the base, left, and right points of an arrow head ending at `to`.
fn arrow_geometry(from: Pos2, to: Pos2, width: f32) -> Option<(Pos2, Pos2, Pos2)> {
    let v = to - from;
    let len = v.length();

    if len <= 0.1 {
        return None;
    }

    let dir = v / len;
    let perp = Vec2::new(-dir.y, dir.x);

    let head_len = width * HEAD_LEN_FACTOR;
    let head_half_width = width * HEAD_WIDTH_FACTOR;

    let base = to - dir * head_len;
    let left = base + perp * head_half_width;
    let right = base - perp * head_half_width;

    Some((base, left, right))
}
/// Returns the vertical center of a displayed waveform item with the drawing offset applied.
fn item_center_y(waves: &WaveData, item_ref: &DisplayedItemRef, offset: f32) -> Option<f32> {
    match waves.get_displayed_item_index(item_ref) {
        Some(vidx) => {
            let info = waves.drawing_infos.get(vidx.0)?;
            Some(info.center() + offset)
        }
        None => None,
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ArrowAnnotation {
    pub from: WavePoint,
    pub to: WavePoint,
    pub created_at: DateTime<Local>,
    pub length: f32,
    pub head_mode: ArrowHeadMode,
    pub annotation_data: AnnotationData,
}

impl Annotatable for ArrowAnnotation {
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

    fn set_show_comments(&mut self, show: bool) {
        self.annotation_data.show_comments = show;
    }

    fn show_comment_box(&self) -> bool {
        self.annotation_data.comment_box.visible
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
        self.to.attached_item.as_ref() == Some(removed_ref)
    }

    fn get_from_wave(&self) -> Option<GraphicsY> {
        //this REALLY should be changed, arrow should likely just use a GraphicsY instead of WavePoint
        if let Some(item) = self.from.attached_item {
            let temp_graphics = GraphicsY {
                item,
                anchor: crate::graphics::Anchor::Center,
            };

            return Some(temp_graphics);
        }

        None
    }

    fn get_to_wave(&self) -> Option<GraphicsY> {
        if let Some(item) = self.to.attached_item {
            let temp_graphics = GraphicsY {
                item,
                anchor: crate::graphics::Anchor::Center,
            };

            return Some(temp_graphics);
        }

        None
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
        let mut arrow_annotation = self.clone();
        arrow_annotation.annotation_data.stroke =
            Stroke::new(theme.annotation_arrow.width, theme.annotation_arrow.color);

        if waves.selected_annotation == Some(self.annotation_data.id) {
            arrow_annotation.is_selected();
        }

        let range = waves.time_range();
        let viewport = waves.viewports[viewport_idx];
        let frame_width = ctx.cfg.canvas_size.x;

        arrow_annotation.annotation_data.id =
            egui::Id::new(("arrow", self.annotation_data.id, viewport_idx));

        // `item_center_y` returns a canvas-local y-coordinate, so it must be converted
        // through `ctx.to_screen` before use as a final screen position.
        let to_y = match self.to.attached_item.as_ref() {
            Some(item_ref) => match item_center_y(waves, item_ref, y_offset) {
                Some(y) => y,
                None => return,
            },
            None => return,
        };

        // A one-headed arrow keeps its original vertical length. A double-headed arrow
        // follows the vertical centers of both attached items.
        let from_y = match self.head_mode {
            ArrowHeadMode::End => to_y - self.length,
            ArrowHeadMode::Double => match self.from.attached_item.as_ref() {
                Some(item_ref) => match item_center_y(waves, item_ref, y_offset) {
                    Some(y) => y,
                    None => return,
                },
                None => return,
            },
        };

        // Convert annotation times into viewport-local x pixel positions.
        let new_to_x = viewport.pixel_from_time(&arrow_annotation.to.time, frame_width, range);

        let new_from_x = viewport.pixel_from_time(&arrow_annotation.from.time, frame_width, range);

        let new_to: Pos2 = (ctx.to_screen)(new_to_x, to_y);
        let new_from = (ctx.to_screen)(new_from_x, from_y);

        arrow_annotation.to.screen_pos = new_to;
        arrow_annotation.from.screen_pos = new_from;

        // Get hover/click position for hit detection
        let pointer_hover_pos = ui.input(|i| i.pointer.hover_pos());
        let pointer_click_pos = ui.input(|i| i.pointer.interact_pos());
        let primary_clicked = ui.input(|i| i.pointer.primary_clicked());

        let exact_hovered = pointer_hover_pos
            .and_then(|p| arrow_annotation.hit_distance_screen(p))
            .is_some();

        let exact_clicked = primary_clicked
            && pointer_click_pos
                .and_then(|p| arrow_annotation.hit_distance_screen(p))
                .is_some();

        ui.add(arrow_annotation);

        if exact_clicked {
            // Notify the application that this annotation was clicked and that the
            // current viewport should become active

            msgs.push(Message::SetActiveViewport(viewport_idx));
            msgs.push(Message::AnnotationClicked(
                Some(self.annotation_data.id),
                pointer_click_pos,
                Some(viewport_idx),
                Some(to_screen),
                Some(ctx.cfg.canvas_size.x),
            ));
            msgs.push(Message::ClickHandled());
        }

        if exact_hovered && let Some(pointer_pos) = pointer_hover_pos {
            // Use a tiny hover rectangle at the pointer position to attach egui's
            // tooltip UI to the actual arrow hit location.
            let hover_rect = egui::Rect::from_center_size(pointer_pos, egui::vec2(1.0, 1.0));

            let hover_response = ui.interact(
                hover_rect,
                egui::Id::new(("arrow_hover_info", self.annotation_data.id, viewport_idx)),
                egui::Sense::hover(),
            );

            let hover_start_time = time_formatter.format(&self.from.time.clone());
            let hover_end_time = time_formatter.format(&self.to.time.clone());

            let group_name = waves
                .annotation_groups
                .iter()
                .find(|group| group.annotations.contains(&self.get_id()))
                .map_or(DEFAULT_GROUP_NAME, |group| &group.name);
            hover_response.on_hover_ui(|ui| {
                self.draw_hover_info(group_name, ui, (&hover_start_time, &hover_end_time));
            });
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
        let to_y = self
            .to
            .attached_item
            .as_ref()
            .and_then(|item_ref| item_center_y(waves, item_ref, offset))?;
        let (x, y) = match self.head_mode {
            ArrowHeadMode::End => (
                viewport.pixel_from_time(&self.to.time, ctx.cfg.canvas_size.x, range),
                to_y,
            ),
            ArrowHeadMode::Double => {
                // For double-headed arrows, place comments near the visual midpoint.
                let from_y = self
                    .from
                    .attached_item
                    .as_ref()
                    .and_then(|item_ref| item_center_y(waves, item_ref, offset))?;
                let from_x =
                    viewport.pixel_from_time(&self.from.time, ctx.cfg.canvas_size.x, range);
                let to_x = viewport.pixel_from_time(&self.to.time, ctx.cfg.canvas_size.x, range);
                (f32::midpoint(from_x, to_x), f32::midpoint(to_y, from_y))
            }
        };
        Some((ctx.to_screen)(x, y))
    }

    fn get_time_info(&self, time_formatter: &TimeFormatter) -> String {
        match self.head_mode {
            ArrowHeadMode::End => format!("Pointing at {}", time_formatter.format(&self.to.time)),
            ArrowHeadMode::Double => format!(
                "from: {}, to: {}",
                time_formatter.format(&self.from.time),
                time_formatter.format(&self.to.time)
            ),
        }
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
}

impl ArrowAnnotation {
    pub(crate) fn new(
        id: Id,
        from: WavePoint,
        to: WavePoint,
        head_mode: ArrowHeadMode,
        num: i32,
    ) -> Self {
        let name = format!("{DEFAULT_TYPE} {num}");
        let annotation_data = AnnotationData::new(id, name, num);

        ArrowAnnotation {
            from: from.clone(),
            to: to.clone(),
            created_at: Local::now(),
            length: to.screen_pos.y - from.screen_pos.y,
            head_mode,
            annotation_data,
        }
    }

    #[must_use]
    pub fn created_at_string(&self) -> String {
        self.created_at.format("%Y-%m-%d %H:%M").to_string()
    }
    pub fn toggle_arrow_visibility(&mut self) {
        self.annotation_data.visible = !self.annotation_data.visible;
    }

    fn hit_radius(&self) -> f32 {
        self.annotation_data.stroke.width + HITBOX_SIZE
    }

    // Builds all drawable and hit-testable arrow segments from the current screen positions.
    fn segments(&self) -> Option<ArrowSegments> {
        let end_head = arrow_geometry(
            self.from.screen_pos,
            self.to.screen_pos,
            self.annotation_data.stroke.width,
        )?;
        let (end_base, end_left, end_right) = end_head;

        let start_head: Option<(Pos2, Pos2, Pos2)> = match self.head_mode {
            ArrowHeadMode::End => None,
            ArrowHeadMode::Double => arrow_geometry(
                self.to.screen_pos,
                self.from.screen_pos,
                self.annotation_data.stroke.width,
            ),
        };

        let shaft_start = match start_head {
            Some((start_base, _, _)) => start_base,
            None => self.from.screen_pos,
        };

        let shaft_end = end_base;

        let (start_tip, start_left, start_right) = match start_head {
            Some((_base, left, right)) => (Some(self.from.screen_pos), Some(left), Some(right)),
            None => (None, None, None),
        };

        Some(ArrowSegments {
            shaft_start,
            shaft_end,
            end_tip: self.to.screen_pos,
            end_left,
            end_right,
            start_tip,
            start_left,
            start_right,
        })
    }
    /// Returns the pointer distance to the arrow if it is inside the hit radius.
    #[must_use]
    pub fn hit_distance_screen(&self, pointer: Pos2) -> Option<f32> {
        if self.is_visible() {
            let seg = self.segments()?;
            let hit_radius = self.hit_radius();

            let mut best = f32::INFINITY;

            // Compare to the shaft
            best = best.min(distance_to_segment(pointer, seg.shaft_start, seg.shaft_end));

            // Compare to the end point 3 segment
            best = best.min(distance_to_segment(pointer, seg.end_tip, seg.end_left));
            best = best.min(distance_to_segment(pointer, seg.end_tip, seg.end_right));
            best = best.min(distance_to_segment(pointer, seg.end_left, seg.end_right));

            // Compare to the arrow head at start, if it is dubbelheaded arrow.
            if let (Some(start_tip), Some(start_left), Some(start_right)) =
                (seg.start_tip, seg.start_left, seg.start_right)
            {
                best = best.min(distance_to_segment(pointer, start_tip, start_left));
                best = best.min(distance_to_segment(pointer, start_tip, start_right));
                best = best.min(distance_to_segment(pointer, start_left, start_right));
            }

            if best <= hit_radius { Some(best) } else { None }
        } else {
            let radius = (self.annotation_data.stroke.width * 2.0) + HITBOX_SIZE;
            let mut best = (pointer - self.to.screen_pos).length();

            if let ArrowHeadMode::Double = self.head_mode {
                best = best.min((pointer - self.from.screen_pos).length());
            }

            if best <= radius { Some(best) } else { None }
        }
    }

    fn paint_arrow_head(&self, ui: &mut Ui, tip: Pos2, left: Pos2, right: Pos2) {
        ui.painter()
            .line_segment([tip, left], self.annotation_data.stroke);
        ui.painter()
            .line_segment([tip, right], self.annotation_data.stroke);
        ui.painter()
            .line_segment([left, right], self.annotation_data.stroke);
    }

    /// Returns arrow `end_position` in global coordinates
    #[must_use]
    pub fn get_pos(
        &self,
        waves: &WaveData,
        viewport: &Viewport,
        ctx: &DrawingContext,
        offset_y: f32,
    ) -> Option<Pos2> {
        let range = waves.time_range();

        let to_x = viewport.pixel_from_time(&self.to.time, ctx.cfg.canvas_size.x, range);
        let to_y = self.to.screen_pos.y;
        let mut position = (ctx.to_screen)(to_x, to_y);
        position.y = to_y + offset_y;

        Some(position)
    }
}

impl Widget for ArrowAnnotation {
    fn ui(self, ui: &mut Ui) -> Response {
        // The widget does custom painting and uses explicit hit detection elsewhere,
        // so it only allocates an empty egui response here.
        let _response = ui.allocate_response(egui::Vec2::ZERO, egui::Sense::empty());
        if !self.is_visible() {
            self.hide_annotation(ui, self.annotation_data.stroke, self.to.screen_pos);

            if let ArrowHeadMode::Double = self.head_mode {
                self.hide_annotation(ui, self.annotation_data.stroke, self.from.screen_pos);
            }
        } else if let Some(seg) = self.segments() {
            // Paint shaft
            ui.painter().line_segment(
                [seg.shaft_start, seg.shaft_end],
                self.annotation_data.stroke,
            );

            // Paint arrow head at the end of the arrow
            self.paint_arrow_head(ui, seg.end_tip, seg.end_left, seg.end_right);

            // Paint arrow head at the start if it is a doubleheaded arrow.
            if let (Some(start_tip), Some(start_left), Some(start_right)) =
                (seg.start_tip, seg.start_left, seg.start_right)
            {
                self.paint_arrow_head(ui, start_tip, start_left, start_right);
            }
        }
        _response
    }
}

use crate::{
    displayed_item::DisplayedFieldRef, displayed_item_tree::VisibleItemIndex,
    transaction_container::TransactionStreamRef, wave_container::FieldRef,
};

#[derive(Debug)]
pub struct VariableDrawingInfo {
    pub field_ref: FieldRef,
    pub displayed_field_ref: DisplayedFieldRef,
    pub vidx: VisibleItemIndex,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Debug)]
pub struct DividerDrawingInfo {
    pub vidx: VisibleItemIndex,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Debug)]
pub struct MarkerDrawingInfo {
    pub vidx: VisibleItemIndex,
    pub top: f32,
    pub bottom: f32,
    pub idx: u8,
}

#[derive(Debug)]
pub struct TimeLineDrawingInfo {
    pub vidx: VisibleItemIndex,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Debug)]
pub struct StreamDrawingInfo {
    pub transaction_stream_ref: TransactionStreamRef,
    pub vidx: VisibleItemIndex,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Debug)]
pub struct DecoderDrawingInfo {
    pub item: crate::displayed_item::DisplayedItemRef,
    pub rows: usize,
    pub vidx: VisibleItemIndex,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Debug)]
pub struct GroupDrawingInfo {
    pub vidx: VisibleItemIndex,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Debug)]
pub struct PlaceholderDrawingInfo {
    pub vidx: VisibleItemIndex,
    pub top: f32,
    pub bottom: f32,
}

pub enum ItemDrawingInfo {
    Variable(VariableDrawingInfo),
    Divider(DividerDrawingInfo),
    Marker(MarkerDrawingInfo),
    TimeLine(TimeLineDrawingInfo),
    Stream(StreamDrawingInfo),
    Decoder(DecoderDrawingInfo),
    Group(GroupDrawingInfo),
    Placeholder(PlaceholderDrawingInfo),
}

impl ItemDrawingInfo {
    #[must_use]
    pub fn top(&self) -> f32 {
        match self {
            ItemDrawingInfo::Variable(drawing_info) => drawing_info.top,
            ItemDrawingInfo::Divider(drawing_info) => drawing_info.top,
            ItemDrawingInfo::Marker(drawing_info) => drawing_info.top,
            ItemDrawingInfo::TimeLine(drawing_info) => drawing_info.top,
            ItemDrawingInfo::Stream(drawing_info) => drawing_info.top,
            ItemDrawingInfo::Decoder(drawing_info) => drawing_info.top,
            ItemDrawingInfo::Group(drawing_info) => drawing_info.top,
            ItemDrawingInfo::Placeholder(drawing_info) => drawing_info.top,
        }
    }
    #[must_use]
    pub fn bottom(&self) -> f32 {
        match self {
            ItemDrawingInfo::Variable(drawing_info) => drawing_info.bottom,
            ItemDrawingInfo::Divider(drawing_info) => drawing_info.bottom,
            ItemDrawingInfo::Marker(drawing_info) => drawing_info.bottom,
            ItemDrawingInfo::TimeLine(drawing_info) => drawing_info.bottom,
            ItemDrawingInfo::Stream(drawing_info) => drawing_info.bottom,
            ItemDrawingInfo::Decoder(drawing_info) => drawing_info.bottom,
            ItemDrawingInfo::Group(drawing_info) => drawing_info.bottom,
            ItemDrawingInfo::Placeholder(drawing_info) => drawing_info.bottom,
        }
    }
    #[must_use]
    pub fn vidx(&self) -> VisibleItemIndex {
        match self {
            ItemDrawingInfo::Variable(drawing_info) => drawing_info.vidx,
            ItemDrawingInfo::Divider(drawing_info) => drawing_info.vidx,
            ItemDrawingInfo::Marker(drawing_info) => drawing_info.vidx,
            ItemDrawingInfo::TimeLine(drawing_info) => drawing_info.vidx,
            ItemDrawingInfo::Stream(drawing_info) => drawing_info.vidx,
            ItemDrawingInfo::Decoder(drawing_info) => drawing_info.vidx,
            ItemDrawingInfo::Group(drawing_info) => drawing_info.vidx,
            ItemDrawingInfo::Placeholder(drawing_info) => drawing_info.vidx,
        }
    }

    pub(crate) fn get_y_from_anchor(&self, anchor: &crate::graphics::Anchor) -> f32 {
        match anchor {
            crate::graphics::Anchor::Top => self.top(),
            crate::graphics::Anchor::Center => self.center(),
            crate::graphics::Anchor::Percentual(p) => self.top() + p * (self.height()),
            crate::graphics::Anchor::Bottom => self.bottom(),
        }
    }

    pub(crate) fn center(&self) -> f32 {
        self.top().midpoint(self.bottom())
    }

    pub(crate) fn height(&self) -> f32 {
        self.bottom() - self.top()
    }

    /// Inverse of `get_y_from_anchor(Anchor::Percentual(_))`: where `y` falls within this
    /// row's `[top, bottom]` range, as a fraction (0 = top, 1 = bottom).
    #[must_use]
    pub(crate) fn percent_of(&self, y: f32) -> f32 {
        (y - self.top()) / self.height()
    }

    /// `top()` translated by `offset` (e.g. a panel's current on-screen starting position),
    /// without allocating a full translated copy.
    #[must_use]
    pub(crate) fn top_at(&self, offset: f32) -> f32 {
        self.top() + offset
    }

    /// `bottom()` translated by `offset`.
    #[must_use]
    pub(crate) fn bottom_at(&self, offset: f32) -> f32 {
        self.bottom() + offset
    }

    /// True if this row overlaps `[clip_top, clip_bottom]` (must be in the same coordinate
    /// space as `self`, e.g. both offset-free/canonical or both shifted by the same amount).
    #[must_use]
    pub(crate) fn overlaps(&self, clip_top: f32, clip_bottom: f32) -> bool {
        self.bottom() >= clip_top && self.top() <= clip_bottom
    }
}

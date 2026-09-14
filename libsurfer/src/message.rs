use bytes::Bytes;
use camino::Utf8PathBuf;
use derive_more::Debug;
use egui::{DroppedFileHandle, Id, Rect};
use emath::{Pos2, RectTransform, Vec2};
use ftr_parser::types::Transaction;
use num::BigInt;
use serde::Deserialize;
use std::sync::Arc;
use surver::SurverStatus;

use crate::annotation_list::AnnotationGroup;
use crate::arrow::{ArrowHeadMode, WavePoint};
use crate::async_util::AsyncJob;
use crate::comment::Comment;
use crate::config::{FocusHighlight, PrimaryMouseDrag, TransitionValue};
use crate::decoders::{DecodedData, DecoderCacheEntry, DecoderInput, DecoderSettings, ValueFormat};
use crate::displayed_item_tree::{ItemIndex, VisibleItemIndex};
use crate::frame_buffer::FrameBufferColorMode;
use crate::graphics::{Graphic, GraphicId, GraphicsY};
use crate::hierarchy::{ParameterDisplayLocation, ScopeExpandType};
use crate::mousegestures::AnnotationKind;
use crate::state::UserState;
use crate::trace_style::TraceStyle;
use crate::transaction_container::{
    StreamScopeRef, TransactionContainer, TransactionRef, TransactionStreamRef,
};
use crate::translation::DynTranslator;
use crate::viewport::ViewportStrategy;
use crate::wave_data::ScopeType;
use crate::{
    MoveDir, VariableNameFilterType, WaveSource,
    clock_highlighting::ClockHighlightType,
    config::ArrowKeyBindings,
    dialog::{OpenSiblingStateFileDialog, ReloadWaveformDialog},
    displayed_item::{AnalogSettings, DisplayedFieldRef, DisplayedItemRef},
    file_dialog::OpenMode,
    hierarchy::HierarchyStyle,
    time::{TimeStringFormatting, TimeUnit},
    variable_filter::VariableIOFilterType,
    variable_name_type::VariableNameType,
    wave_container::{AnalogCacheKey, ScopeRef, VariableRef, WaveContainer},
    wave_source::{CxxrtlKind, LoadOptions, WaveFormat},
    wellen::{BodyResult, HeaderResult, LoadSignalsResult},
};

type CommandCount = usize;

/// Encapsulates either a specific variable or all selected variables
#[derive(Debug, Deserialize, Clone)]
pub enum MessageTarget<T> {
    Explicit(T),
    CurrentSelection,
}

impl<T> From<MessageTarget<T>> for Option<T> {
    fn from(value: MessageTarget<T>) -> Self {
        match value {
            MessageTarget::Explicit(val) => Some(val),
            MessageTarget::CurrentSelection => None,
        }
    }
}

impl<T> From<Option<T>> for MessageTarget<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(val) => Self::Explicit(val),
            None => Self::CurrentSelection,
        }
    }
}

impl<T: Copy> Copy for MessageTarget<T> {}

#[derive(Debug, Deserialize)]
/// The design of Surfer relies on sending messages to trigger actions.
pub enum Message {
    /// Set active scope, None corresponds to the top-level scope.
    SetActiveScope(Option<ScopeType>),
    ExpandScope(ScopeExpandType),
    /// Add one or more variables to wave view.
    AddVariables(Vec<VariableRef>),
    /// Add scope to wave view.
    ///
    /// If second argument is true, add subscopes recursively.
    AddScope(ScopeRef, bool),
    /// Add scope to wave view as a group.
    ///
    /// If second argument is true, add subscopes recursively.
    AddScopeAsGroup(ScopeRef, bool),
    /// Add a character to the repeat command counter.
    AddCount(char),
    AddStreamOrGenerator(TransactionStreamRef),
    AddStreamOrGeneratorFromName(Option<StreamScopeRef>, String),
    AddAllFromStreamScope(String),
    AddDecoder {
        decoder: String,
        inputs: Vec<DecoderInput>,
        settings: DecoderSettings,
        show_samples: bool,
        value_format: ValueFormat,
        analog: Option<AnalogSettings>,
    },
    SetDecoderSamples(MessageTarget<VisibleItemIndex>, bool),
    SetDecoderValueFormat(MessageTarget<VisibleItemIndex>, ValueFormat),
    /// Open the add (`None`) or edit (`Some`) decoder dialog.
    ShowDecoderDialog(Option<DisplayedItemRef>),
    HideDecoderDialog,
    UpdateDecoder {
        item: DisplayedItemRef,
        decoder: String,
        inputs: Vec<DecoderInput>,
        settings: DecoderSettings,
        show_samples: bool,
        value_format: ValueFormat,
        analog: Option<AnalogSettings>,
    },
    /// Reset the repeat command counter.
    InvalidateCount,
    RemoveVisibleItems(MessageTarget<VisibleItemIndex>),
    RemoveItems(Vec<DisplayedItemRef>),
    /// Focus a wave/item.
    FocusItem(VisibleItemIndex),
    ItemSelectRange(VisibleItemIndex),
    /// Select all waves/items.
    ItemSelectAll,
    SetItemSelected(VisibleItemIndex, bool),
    /// Unfocus a wave/item.
    UnfocusItem,
    MoveFocus(MoveDir, CommandCount, bool),
    MoveFocusedItem(MoveDir, CommandCount),
    FocusTransaction(Option<TransactionRef>, Option<Transaction>),
    VerticalScroll(MoveDir, CommandCount),
    /// Scroll in vertical direction so that the item at a given location in the list is at the top (or visible).
    ScrollToItem(usize),
    SetScrollOffset(f32),
    /// Change format (translator) of a variable.
    ///
    /// Passing None as first element means all selected variables.
    VariableFormatChange(MessageTarget<DisplayedFieldRef>, String),
    ItemSelectionClear,
    /// Change color of waves/items.
    ///
    /// If first argument is None, change for selected items. If second argument is None, change to default value.
    ItemColorChange(MessageTarget<VisibleItemIndex>, Option<String>),
    /// Change background color of waves/items.
    ///
    /// If first argument is None, change for selected items. If second argument is None, change to default value.
    ItemBackgroundColorChange(MessageTarget<VisibleItemIndex>, Option<String>),
    ItemNameChange(Option<VisibleItemIndex>, Option<String>),
    ItemNameReset(MessageTarget<VisibleItemIndex>),
    /// Change scaling factor/height of waves/items.
    ///
    /// If first argument is None, change for selected items.
    ItemHeightScalingFactorChange(MessageTarget<VisibleItemIndex>, f32),
    /// Change variable name type of waves/items.
    ///
    /// If first argument is None, change for selected items.
    ChangeVariableNameType(MessageTarget<VisibleItemIndex>, VariableNameType),
    ForceVariableNameTypes(VariableNameType),
    /// Set or unset right alignment of names
    SetNameAlignRight(bool),
    SetClockHighlightType(ClockHighlightType),
    SetFillHighValues(bool),
    SetTraceStyle(TraceStyle),
    /// Reset the translator for this variable back to default.
    ///
    /// Sub-variables, i.e., those with the variable idx and a shared path are also reset.
    ResetVariableFormat(DisplayedFieldRef),
    CanvasScroll {
        delta: Vec2,
        viewport_idx: usize,
    },
    CanvasZoom {
        mouse_ptr: Option<BigInt>,
        delta: f32,
        viewport_idx: usize,
    },
    ZoomToCursor {
        delta: f32,
        viewport_idx: usize,
    },
    ZoomToRange {
        start: BigInt,
        end: BigInt,
        viewport_idx: usize,
    },
    /// Set cursor at time.
    CursorSet(BigInt),
    #[serde(skip)]
    SetSurverStatus(web_time::Instant, String, SurverStatus),
    /// Load file from file path.
    LoadFile(Utf8PathBuf, LoadOptions),
    /// Load file from URL.
    LoadWaveformFileFromUrl(String, LoadOptions),
    /// Load file from data.
    LoadFromData(Vec<u8>, LoadOptions),
    #[cfg(feature = "python")]
    /// Load translator from Python file path.
    LoadPythonTranslator(Utf8PathBuf),
    /// Load a web assembly translator from file.
    ///
    /// This is loaded in addition to the translators loaded on startup.
    #[cfg(all(not(target_arch = "wasm32"), feature = "wasm_plugins"))]
    LoadWasmTranslator(Utf8PathBuf),
    /// Load command file from file path.
    LoadCommandFile(Utf8PathBuf),
    /// Load commands from data.
    LoadCommandFromData(Vec<u8>),
    /// Load command file from URL.
    LoadCommandFileFromUrl(String),
    #[serde(skip)]
    ExecuteBatchCommand {
        line: usize,
        command: String,
    },
    SetupCxxrtl(CxxrtlKind),
    #[serde(skip)]
    /// Message sent when waveform file header is loaded.
    WaveHeaderLoaded(
        web_time::Instant,
        WaveSource,
        LoadOptions,
        #[debug(skip)] HeaderResult,
    ),
    #[serde(skip)]
    /// Message sent when waveform file body is loaded.
    WaveBodyLoaded(web_time::Instant, WaveSource, #[debug(skip)] BodyResult),
    #[serde(skip)]
    WavesLoaded(
        WaveSource,
        WaveFormat,
        #[debug(skip)] Box<WaveContainer>,
        LoadOptions,
    ),
    #[serde(skip)]
    SignalsLoaded(web_time::Instant, #[debug(skip)] LoadSignalsResult),
    #[serde(skip)]
    TransactionStreamsLoaded(
        WaveSource,
        WaveFormat,
        #[debug(skip)] TransactionContainer,
        LoadOptions,
    ),
    #[serde(skip)]
    TranslatorLoaded(#[debug(skip)] Arc<DynTranslator>),
    /// Take note that the specified translator errored on a `translates` call on the
    /// specified variable
    BlacklistTranslator(VariableRef, String),
    HideCommandPrompt,
    ShowCommandPrompt(String, Option<String>),
    /// Message sent when file is dropped onto Surfer.
    #[serde(skip)]
    FileDropped(DroppedFileHandle),
    #[serde(skip)]
    /// Message sent when dropped-file bytes are asynchronously available.
    DroppedFileBytesLoaded(Option<Utf8PathBuf>, Vec<u8>),
    #[serde(skip)]
    /// Message sent when download of a waveform file is complete.
    FileDownloaded(String, Bytes, LoadOptions),
    #[serde(skip)]
    /// Message sent when download of a command file is complete.
    CommandFileDownloaded(String, Bytes),
    ReloadConfig,
    ReloadWaveform(bool),
    /// Suggest reloading the current waveform as the file on disk has changed.
    /// This should first take the user's confirmation before reloading the waveform.
    /// However, there is a configuration setting that the user can overwrite.
    #[serde(skip)]
    SuggestReloadWaveform,
    /// Close the '`reload_waveform`' dialog.
    /// The `reload_file` boolean is the return value of the dialog.
    /// If `do_not_show_again` is true, the `reload_file` setting will be persisted.
    #[serde(skip)]
    CloseReloadWaveformDialog {
        reload_file: bool,
        do_not_show_again: bool,
    },
    /// Update the waveform dialog UI with the provided dialog model.
    #[serde(skip)]
    UpdateReloadWaveformDialog(ReloadWaveformDialog),
    // When a file is open, suggest opening state files in the same directory
    OpenSiblingStateFile(bool),
    #[serde(skip)]
    SuggestOpenSiblingStateFile,
    #[serde(skip)]
    CloseOpenSiblingStateFileDialog {
        load_state: bool,
        do_not_show_again: bool,
    },
    #[serde(skip)]
    UpdateOpenSiblingStateFileDialog(OpenSiblingStateFileDialog),
    RemovePlaceholders,
    ZoomToFit {
        viewport_idx: usize,
    },
    GoToStart {
        viewport_idx: usize,
    },
    GoToEnd {
        viewport_idx: usize,
    },
    GoToTime(Option<BigInt>, usize),
    SetMenuVisible(bool),
    ToggleMenu,
    SetToolbarVisible(bool),
    SetToolbarGroupEnabled(String, bool),
    SetToolbarGroupRow(String, u8),
    SetOverviewVisible(bool),
    SetStatusbarVisible(bool),
    SetShowIndices(bool),
    SetShowVariableDirection(bool),
    SetShowEmptyScopes(bool),
    SetShowHierarchyIcons(bool),
    SetParameterDisplayLocation(ParameterDisplayLocation),
    SetSidePanelVisible(bool),
    ToggleItemSelected(Option<VisibleItemIndex>),
    SetDefaultTimeline(bool),
    SetTickLines(bool),
    SetVariableTooltip(bool),
    SetScopeTooltip(bool),
    SetSurverFileWindowVisible(bool),
    LoadSurverFileByIndex(Option<usize>, LoadOptions),
    LoadSurverFileByName(String, LoadOptions),
    SetTransitionValue(TransitionValue),
    ToggleFullscreen,
    StopProgressTracker,
    /// Set which time unit to use.
    SetTimeUnit(TimeUnit),
    /// Set how to format the time strings.
    ///
    /// Passing None resets it to default.
    SetTimeStringFormatting(Option<TimeStringFormatting>),
    CommandPromptClear,
    CommandPromptUpdate {
        suggestions: Vec<(String, Vec<bool>)>,
    },
    CommandPromptPushPrevious(String),
    SelectPrevCommand,
    SelectNextCommand,
    OpenFileDialog(OpenMode),
    OpenCommandFileDialog,
    #[cfg(feature = "python")]
    OpenPythonPluginDialog,
    #[cfg(feature = "python")]
    ReloadPythonPlugin,
    SaveStateFile(Option<Utf8PathBuf>),
    /// Export the currently displayed variables (and only the hierarchy needed for them) to an FST file.
    #[cfg(not(target_arch = "wasm32"))]
    ExportSignalsToFst(Option<Utf8PathBuf>),
    /// Load state from data.
    /// Note: the internal state is not a stable format and this should not be
    /// relied on to work across revisions.
    LoadStateFromData(Vec<u8>),
    LoadStateFile(Option<Utf8PathBuf>),
    LoadState(Box<UserState>, Option<Utf8PathBuf>),
    SetStateFile(Utf8PathBuf),
    SetAboutVisible(bool),
    SetKeyHelpVisible(bool),
    SetGestureHelpVisible(bool),
    SetQuickStartVisible(bool),
    #[serde(skip)]
    SetUrlEntryVisible(
        bool,
        #[debug(skip)] Option<Box<dyn Fn(String) -> Message + Send + 'static>>,
    ),
    SetLicenseVisible(bool),
    SetLogsVisible(bool),
    SetFrameBufferVariable(VariableRef),
    SetFrameBufferVisibleVariable(Option<VisibleItemIndex>),
    SetFrameBufferArray(ScopeRef),
    SetFrameBufferMode(FrameBufferColorMode, u8, u8, u8),
    SetFrameBufferWidth(usize),
    SetFrameBufferRange(Vec<(i64, i64)>),
    SetMouseGestureDragStart(Option<Pos2>, Option<BigInt>),
    OpenMemoryViewer {
        scope: ScopeRef,
        name: Option<String>,
    },
    SetMeasureDragStart(Option<Pos2>),
    /// Set or clear focus state for a widget identified by id string.
    SetTextEditFocused(String, bool),
    /// Request focus (one-shot) for a widget identified by id string.
    SetRequestTextEditFocus(String, bool),
    /// Clear focus state for all widgets.
    ClearAllTextEditFocuses,
    SetVariableNameFilterType(VariableNameFilterType),
    SetVariableNameFilterCaseInsensitive(bool),
    SetVariableIOFilter(VariableIOFilterType, bool),
    SetVariableGroupByDirection(bool),
    SetUIZoomFactor(f32),
    SetPerformanceVisible(bool),
    SetContinuousRedraw(bool),
    SetCursorWindowVisible(bool),
    SetDrawVectorUnknownsAsLine(bool),
    SetFocusHighlight(FocusHighlight),
    SetHierarchyStyle(HierarchyStyle),
    SetArrowKeyBindings(ArrowKeyBindings),
    SetPrimaryMouseDragBehavior(PrimaryMouseDrag),
    SetTimeOffsetEnabled(bool),
    // Second argument is position to insert after, None inserts after focused item,
    // or last if no focused item
    AddDivider(Option<String>, Option<VisibleItemIndex>),
    // Argument is position to insert after, None inserts after focused item,
    // or last if no focused item
    AddTimeLine(Option<VisibleItemIndex>),
    AddMarker {
        time: BigInt,
        name: Option<String>,
        move_focus: bool,
    },
    /// Resolve a marker name or `#id` at execution time, then set or create the marker.
    // FIXME Resolving by `#id` does not work as expected; characters after a `#` are
    // stripped as comments before being parsed.
    ResolveMarkerSet {
        name: String,
        time: BigInt,
    },
    /// Set a marker at a specific position.
    ///
    /// If it doesn't exist, it will be created
    SetMarker {
        id: u8,
        time: BigInt,
    },
    /// Resolve a marker name or `#id` at execution time, then remove the marker if it exists.
    // FIXME Resolving by `#id` does not work as expected; characters after a `#` are
    // stripped as comments before being parsed.
    ResolveMarkerRemove(String),
    /// Remove marker.
    RemoveMarker(u8),
    /// Set or move a marker to the position of the current cursor.
    MoveMarkerToCursor(u8),
    /// Scroll in horizontal direction so that the cursor is visible.
    GoToCursorIfNotInView,
    GoToMarkerPosition(u8, usize),
    MoveCursorToTransition {
        next: bool,
        variable: Option<VisibleItemIndex>,
        skip_zero: bool,
    },
    MoveTransaction {
        next: bool,
    },
    VariableValueToClipbord(MessageTarget<VisibleItemIndex>),
    VariableNameToClipboard(MessageTarget<VisibleItemIndex>),
    VariableFullNameToClipboard(MessageTarget<VisibleItemIndex>),
    InvalidateDrawCommands,
    AddGraphic(GraphicId, Graphic),
    RemoveGraphic(GraphicId),

    /// Variable dragging messages
    VariableDragStarted(VisibleItemIndex),
    VariableDragTargetChanged(crate::displayed_item_tree::TargetPosition),
    VariableDragFinished,
    AddDraggedVariables(Vec<VariableRef>),
    /// Unpauses the simulation if the wave source supports this kind of interactivity.
    ///
    /// Otherwise does nothing
    UnpauseSimulation,
    /// Pause the simulation if the wave source supports this kind of interactivity.
    ///
    /// Otherwise does nothing
    PauseSimulation,
    /// Expand the displayed item into subfields.
    ///
    /// Levels controls how many layers of subfields are expanded. 0 unexpands it completely.
    ExpandDrawnItem {
        item: DisplayedItemRef,
        levels: usize,
    },
    /// Toggle whether a compound variable's field (path relative to the variable's root) is
    /// expanded to show its own subfields.
    ToggleVariableFieldFold(DisplayedItemRef, Vec<String>),
    SetAnalogSettings(
        MessageTarget<VisibleItemIndex>,
        Option<crate::displayed_item::AnalogSettings>,
    ),
    BuildAnalogCache {
        display_id: DisplayedItemRef,
        cache_key: AnalogCacheKey,
    },
    #[serde(skip)]
    AnalogCacheBuilt {
        #[debug(skip)]
        entry: Arc<crate::analog_signal_cache::AnalogCacheEntry>,
        #[debug(skip)]
        result: Result<crate::analog_signal_cache::AnalogSignalCache, String>,
    },
    BuildDecoderCache {
        display_id: DisplayedItemRef,
        cache_key: crate::decoders::DecoderCacheKey,
    },
    #[serde(skip)]
    DecoderCacheBuilt {
        display_id: DisplayedItemRef,
        #[debug(skip)]
        entry: Arc<DecoderCacheEntry>,
        #[debug(skip)]
        result: Result<Arc<DecodedData>, String>,
    },

    SetViewportStrategy(ViewportStrategy),
    SetConfigFromString(String),
    AddCharToPrompt(char),

    /// Run more than one message in sequence
    Batch(Vec<Message>),
    AddViewport,
    RemoveViewport,
    /// Select Theme
    SelectTheme(Option<String>),
    /// Enable animations
    EnableAnimations(bool),
    /// Show text of the dividers inline with the signals
    ShowDividerText(bool),
    /// Undo the last n changes
    Undo(usize),
    /// Redo the last n changes
    Redo(usize),
    DumpTree,
    GroupNew {
        name: Option<String>,
        before: Option<ItemIndex>,
        items: Option<Vec<DisplayedItemRef>>,
    },
    GroupDissolve(Option<DisplayedItemRef>),
    GroupFold(Option<DisplayedItemRef>),
    GroupUnfold(Option<DisplayedItemRef>),
    GroupFoldRecursive(Option<DisplayedItemRef>),
    GroupUnfoldRecursive(Option<DisplayedItemRef>),
    GroupFoldAll,
    GroupUnfoldAll,
    /// WCP Server
    StartWcpServer {
        address: Option<String>,
        initiate: bool,
    },
    StopWcpServer,
    /// Configures the WCP system to listen for messages over internal channels.
    /// This is used to start WCP on wasm
    SetupChannelWCP,
    DownloadDefaultConfig,
    /// Exit the application.
    ///
    /// This has no effect on wasm and closes the window
    /// on other platforms
    Exit,
    /// Expands the parameter section so that one can test the rendering.
    ///
    /// Should only used for tests.
    ExpandParameterSection,
    AsyncDone(AsyncJob),
    SetMouseGestureAnnotation(Option<AnnotationKind>),
    RectangleAdded {
        time_at_start: BigInt,
        time_at_end: BigInt,
        wave_from: Option<GraphicsY>,
        wave_to: Option<GraphicsY>,
        rect: Rect,
    },
    ArrowAdded {
        wave_point_from: WavePoint,
        wave_point_to: WavePoint,
        head_mode: ArrowHeadMode,
    },
    RemoveAnnotation(Id),
    ToggleAnnotationVisiblility(Id),
    ToggleAnnotationListShowComments(Id),
    GoToAnnotationPosition(Id, usize),
    ToggleAnnotationlistVisibility(),
    CreateAnnotationGroup(String),
    DeleteAnnotationGroup(String),
    DeleteAllAnnotationInGroup(String),
    UpdateAnnotationGroup(Id, Option<String>),
    SetGroupVisibility(AnnotationGroup, bool),
    UpdateAnnotationName(Id, String),
    AnnotationClicked(
        Option<Id>,
        Option<Pos2>,
        Option<usize>,
        Option<RectTransform>,
        Option<f32>,
    ),
    SetActiveViewport(usize),
    ClickHandled(),
    UpdateCommentBox(Vec<(Id, Comment)>),
    AddCommentMessage(Id, String, String),
    RemoveCommentMessage(Id, Id),
    ToggleCommentVisibility(Id),
}

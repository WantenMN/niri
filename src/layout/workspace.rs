use std::rc::Rc;
use std::time::Duration;

use niri_config::{OutputName, Workspace as WorkspaceConfig};
use niri_ipc::SizeChange;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::{layer_map_for_output, Window};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Scale, Serial, Size, Transform};

use super::scrolling::{
    Column, ColumnWidth, InsertHint, InsertPosition, ScrollingSpace, ScrollingSpaceRenderElement,
};
use super::tile::{Tile, TileRenderSnapshot};
use super::{InteractiveResizeData, LayoutElement, Options, RemovedTile};
use crate::animation::Clock;
use crate::niri_render_elements;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::RenderTarget;
use crate::utils::id::IdCounter;
use crate::utils::transaction::{Transaction, TransactionBlocker};
use crate::utils::{output_size, send_scale_transform, ResizeEdge};
use crate::window::ResolvedWindowRules;

#[derive(Debug)]
pub struct Workspace<W: LayoutElement> {
    /// The scrollable-tiling layout.
    scrolling: ScrollingSpace<W>,

    /// The original output of this workspace.
    ///
    /// Most of the time this will be the workspace's current output, however, after an output
    /// disconnection, it may remain pointing to the disconnected output.
    pub(super) original_output: OutputId,

    /// Current output of this workspace.
    output: Option<Output>,

    /// Latest known output scale for this workspace.
    ///
    /// This should be set from the current workspace output, or, if all outputs have been
    /// disconnected, preserved until a new output is connected.
    scale: smithay::output::Scale,

    /// Latest known output transform for this workspace.
    ///
    /// This should be set from the current workspace output, or, if all outputs have been
    /// disconnected, preserved until a new output is connected.
    transform: Transform,

    /// Latest known view size for this workspace.
    ///
    /// This should be computed from the current workspace output size, or, if all outputs have
    /// been disconnected, preserved until a new output is connected.
    view_size: Size<f64, Logical>,

    /// Latest known working area for this workspace.
    ///
    /// Not rounded to physical pixels.
    ///
    /// This is similar to view size, but takes into account things like layer shell exclusive
    /// zones.
    working_area: Rectangle<f64, Logical>,

    /// Clock for driving animations.
    pub(super) clock: Clock,

    /// Configurable properties of the layout as received from the parent monitor.
    pub(super) base_options: Rc<Options>,

    /// Configurable properties of the layout with logical sizes adjusted for the current `scale`.
    pub(super) options: Rc<Options>,

    /// Optional name of this workspace.
    pub(super) name: Option<String>,

    /// Unique ID of this workspace.
    id: WorkspaceId,
}

#[derive(Debug, Clone)]
pub struct OutputId(String);

impl OutputId {
    pub fn matches(&self, output: &Output) -> bool {
        let output_name = output.user_data().get::<OutputName>().unwrap();
        output_name.matches(&self.0)
    }
}

static WORKSPACE_ID_COUNTER: IdCounter = IdCounter::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkspaceId(u64);

impl WorkspaceId {
    fn next() -> WorkspaceId {
        WorkspaceId(WORKSPACE_ID_COUNTER.next())
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn specific(id: u64) -> Self {
        Self(id)
    }
}

niri_render_elements! {
    WorkspaceRenderElement<R> => {
        Scrolling = ScrollingSpaceRenderElement<R>,
    }
}

#[derive(Debug)]
pub(super) struct InteractiveResize<W: LayoutElement> {
    pub window: W::Id,
    pub original_window_size: Size<f64, Logical>,
    pub data: InteractiveResizeData,
}

/// Resolved width or height in logical pixels.
#[derive(Debug, Clone, Copy)]
pub enum ResolvedSize {
    /// Size of the tile including borders.
    Tile(f64),
    /// Size of the window excluding borders.
    Window(f64),
}

impl OutputId {
    pub fn new(output: &Output) -> Self {
        let output_name = output.user_data().get::<OutputName>().unwrap();
        Self(output_name.format_make_model_serial_or_connector())
    }
}

impl<W: LayoutElement> Workspace<W> {
    pub fn new(output: Output, clock: Clock, options: Rc<Options>) -> Self {
        Self::new_with_config(output, None, clock, options)
    }

    pub fn new_with_config(
        output: Output,
        config: Option<WorkspaceConfig>,
        clock: Clock,
        base_options: Rc<Options>,
    ) -> Self {
        let original_output = config
            .as_ref()
            .and_then(|c| c.open_on_output.clone())
            .map(OutputId)
            .unwrap_or(OutputId::new(&output));

        let scale = output.current_scale();
        let options =
            Rc::new(Options::clone(&base_options).adjusted_for_scale(scale.fractional_scale()));

        let view_size = output_size(&output);
        let working_area = compute_working_area(&output);

        let scrolling = ScrollingSpace::new(
            view_size,
            working_area,
            scale.fractional_scale(),
            clock.clone(),
            options.clone(),
        );

        Self {
            scrolling,
            original_output,
            scale,
            transform: output.current_transform(),
            view_size,
            working_area,
            output: Some(output),
            clock,
            base_options,
            options,
            name: config.map(|c| c.name.0),
            id: WorkspaceId::next(),
        }
    }

    pub fn new_with_config_no_outputs(
        config: Option<WorkspaceConfig>,
        clock: Clock,
        base_options: Rc<Options>,
    ) -> Self {
        let original_output = OutputId(
            config
                .as_ref()
                .and_then(|c| c.open_on_output.clone())
                .unwrap_or_default(),
        );

        let scale = smithay::output::Scale::Integer(1);
        let options =
            Rc::new(Options::clone(&base_options).adjusted_for_scale(scale.fractional_scale()));

        let view_size = Size::from((1280., 720.));
        let working_area = Rectangle::from_loc_and_size((0., 0.), (1280., 720.));

        let scrolling = ScrollingSpace::new(
            view_size,
            working_area,
            scale.fractional_scale(),
            clock.clone(),
            options.clone(),
        );

        Self {
            scrolling,
            output: None,
            scale,
            transform: Transform::Normal,
            original_output,
            view_size,
            working_area,
            clock,
            base_options,
            options,
            name: config.map(|c| c.name.0),
            id: WorkspaceId::next(),
        }
    }

    pub fn new_no_outputs(clock: Clock, options: Rc<Options>) -> Self {
        Self::new_with_config_no_outputs(None, clock, options)
    }

    pub fn id(&self) -> WorkspaceId {
        self.id
    }

    pub fn name(&self) -> Option<&String> {
        self.name.as_ref()
    }

    pub fn unname(&mut self) {
        self.name = None;
    }

    pub fn has_windows_or_name(&self) -> bool {
        self.has_windows() || self.name.is_some()
    }

    pub fn scale(&self) -> smithay::output::Scale {
        self.scale
    }

    pub fn advance_animations(&mut self) {
        self.scrolling.advance_animations();
    }

    pub fn are_animations_ongoing(&self) -> bool {
        self.scrolling.are_animations_ongoing()
    }

    pub fn are_transitions_ongoing(&self) -> bool {
        self.scrolling.are_transitions_ongoing()
    }

    pub fn update_render_elements(&mut self, is_active: bool) {
        self.scrolling.update_render_elements(is_active);
    }

    pub fn update_config(&mut self, base_options: Rc<Options>) {
        let scale = self.scale.fractional_scale();
        let options = Rc::new(Options::clone(&base_options).adjusted_for_scale(scale));

        self.scrolling.update_config(
            self.view_size,
            self.working_area,
            self.scale.fractional_scale(),
            options.clone(),
        );

        self.base_options = base_options;
        self.options = options;
    }

    pub fn update_shaders(&mut self) {
        self.scrolling.update_shaders();
    }

    pub fn windows(&self) -> impl Iterator<Item = &W> + '_ {
        self.tiles().map(Tile::window)
    }

    pub fn windows_mut(&mut self) -> impl Iterator<Item = &mut W> + '_ {
        self.tiles_mut().map(Tile::window_mut)
    }

    pub fn tiles(&self) -> impl Iterator<Item = &Tile<W>> + '_ {
        self.scrolling.tiles()
    }

    pub fn tiles_mut(&mut self) -> impl Iterator<Item = &mut Tile<W>> + '_ {
        self.scrolling.tiles_mut()
    }

    pub fn current_output(&self) -> Option<&Output> {
        self.output.as_ref()
    }

    pub fn active_window(&self) -> Option<&W> {
        self.scrolling.active_window()
    }

    pub fn is_active_fullscreen(&self) -> bool {
        self.scrolling.is_active_fullscreen()
    }

    pub fn set_output(&mut self, output: Option<Output>) {
        if self.output == output {
            return;
        }

        if let Some(output) = self.output.take() {
            for win in self.windows() {
                win.output_leave(&output);
            }
        }

        self.output = output;

        if let Some(output) = &self.output {
            // Normalize original output: possibly replace connector with make/model/serial.
            if self.original_output.matches(output) {
                self.original_output = OutputId::new(output);
            }

            self.update_output_size();

            for win in self.windows() {
                self.enter_output_for_window(win);
            }
        }
    }

    fn enter_output_for_window(&self, window: &W) {
        if let Some(output) = &self.output {
            window.set_preferred_scale_transform(self.scale, self.transform);
            window.output_enter(output);
        }
    }

    pub fn update_output_size(&mut self) {
        let output = self.output.as_ref().unwrap();
        let scale = output.current_scale();
        let transform = output.current_transform();
        let view_size = output_size(output);
        let working_area = compute_working_area(output);
        self.set_view_size(scale, transform, view_size, working_area);
    }

    fn set_view_size(
        &mut self,
        scale: smithay::output::Scale,
        transform: Transform,
        size: Size<f64, Logical>,
        working_area: Rectangle<f64, Logical>,
    ) {
        let scale_transform_changed = self.transform != transform
            || self.scale.integer_scale() != scale.integer_scale()
            || self.scale.fractional_scale() != scale.fractional_scale();
        if !scale_transform_changed && self.view_size == size && self.working_area == working_area {
            return;
        }

        let fractional_scale_changed = self.scale.fractional_scale() != scale.fractional_scale();

        self.scale = scale;
        self.transform = transform;
        self.view_size = size;
        self.working_area = working_area;

        if fractional_scale_changed {
            // Options need to be recomputed for the new scale.
            self.update_config(self.base_options.clone());
        } else {
            // Pass our existing options as is.
            self.scrolling.update_config(
                size,
                working_area,
                scale.fractional_scale(),
                self.options.clone(),
            );
        }

        if scale_transform_changed {
            for window in self.windows() {
                window.set_preferred_scale_transform(self.scale, self.transform);
            }
        }
    }

    pub fn view_size(&self) -> Size<f64, Logical> {
        self.view_size
    }

    pub fn add_window(
        &mut self,
        window: W,
        activate: bool,
        width: ColumnWidth,
        is_full_width: bool,
    ) {
        let tile = Tile::new(
            window,
            self.scale.fractional_scale(),
            self.clock.clone(),
            self.options.clone(),
        );
        self.add_tile(None, tile, activate, width, is_full_width);
    }

    pub fn add_tile(
        &mut self,
        col_idx: Option<usize>,
        tile: Tile<W>,
        activate: bool,
        width: ColumnWidth,
        is_full_width: bool,
    ) {
        self.enter_output_for_window(tile.window());
        self.scrolling
            .add_tile(col_idx, tile, activate, width, is_full_width, None);
    }

    pub fn add_tile_to_column(
        &mut self,
        col_idx: usize,
        tile_idx: Option<usize>,
        tile: Tile<W>,
        activate: bool,
    ) {
        self.enter_output_for_window(tile.window());
        self.scrolling
            .add_tile_to_column(col_idx, tile_idx, tile, activate);
    }

    pub fn add_window_right_of(
        &mut self,
        right_of: &W::Id,
        window: W,
        width: ColumnWidth,
        is_full_width: bool,
    ) {
        let tile = Tile::new(
            window,
            self.scale.fractional_scale(),
            self.clock.clone(),
            self.options.clone(),
        );
        self.add_tile_right_of(right_of, tile, width, is_full_width);
    }

    pub fn add_tile_right_of(
        &mut self,
        right_of: &W::Id,
        tile: Tile<W>,
        width: ColumnWidth,
        is_full_width: bool,
    ) {
        self.enter_output_for_window(tile.window());
        self.scrolling
            .add_tile_right_of(right_of, tile, width, is_full_width);
    }

    pub fn add_column(&mut self, column: Column<W>, activate: bool) {
        for (tile, _) in column.tiles() {
            self.enter_output_for_window(tile.window());
        }

        self.scrolling.add_column(None, column, activate, None);
    }

    pub fn remove_tile(&mut self, id: &W::Id, transaction: Transaction) -> RemovedTile<W> {
        let removed = self.scrolling.remove_tile(id, transaction);

        if let Some(output) = &self.output {
            removed.tile.window().output_leave(output);
        }

        removed
    }

    pub fn remove_active_tile(&mut self, transaction: Transaction) -> Option<RemovedTile<W>> {
        let removed = self.scrolling.remove_active_tile(transaction)?;

        if let Some(output) = &self.output {
            removed.tile.window().output_leave(output);
        }

        Some(removed)
    }

    pub fn remove_active_column(&mut self) -> Option<Column<W>> {
        let column = self.scrolling.remove_active_column()?;

        if let Some(output) = &self.output {
            for (tile, _) in column.tiles() {
                tile.window().output_leave(output);
            }
        }

        Some(column)
    }

    pub fn resolve_default_width(
        &self,
        default_width: Option<Option<ColumnWidth>>,
    ) -> Option<ColumnWidth> {
        match default_width {
            Some(Some(width)) => Some(width),
            Some(None) => None,
            None => self.options.default_column_width,
        }
    }

    pub fn new_window_size(
        &self,
        width: Option<ColumnWidth>,
        rules: &ResolvedWindowRules,
    ) -> Size<i32, Logical> {
        self.scrolling.new_window_size(width, rules)
    }

    pub fn configure_new_window(
        &self,
        window: &Window,
        width: Option<ColumnWidth>,
        rules: &ResolvedWindowRules,
    ) {
        window.with_surfaces(|surface, data| {
            send_scale_transform(surface, data, self.scale, self.transform);
        });

        window
            .toplevel()
            .expect("no x11 support")
            .with_pending_state(|state| {
                if state.states.contains(xdg_toplevel::State::Fullscreen) {
                    state.size = Some(self.view_size.to_i32_round());
                } else {
                    state.size = Some(self.new_window_size(width, rules));
                }

                state.bounds = Some(self.scrolling.toplevel_bounds(rules));
            });
    }

    pub fn focus_left(&mut self) -> bool {
        self.scrolling.focus_left()
    }

    pub fn focus_right(&mut self) -> bool {
        self.scrolling.focus_right()
    }

    pub fn focus_column_first(&mut self) {
        self.scrolling.focus_column_first();
    }

    pub fn focus_column_last(&mut self) {
        self.scrolling.focus_column_last();
    }

    pub fn focus_column_right_or_first(&mut self) {
        self.scrolling.focus_column_right_or_first();
    }

    pub fn focus_column_left_or_last(&mut self) {
        self.scrolling.focus_column_left_or_last();
    }

    pub fn focus_down(&mut self) -> bool {
        self.scrolling.focus_down()
    }

    pub fn focus_up(&mut self) -> bool {
        self.scrolling.focus_up()
    }

    pub fn focus_down_or_left(&mut self) {
        self.scrolling.focus_down_or_left();
    }

    pub fn focus_down_or_right(&mut self) {
        self.scrolling.focus_down_or_right();
    }

    pub fn focus_up_or_left(&mut self) {
        self.scrolling.focus_up_or_left();
    }

    pub fn focus_up_or_right(&mut self) {
        self.scrolling.focus_up_or_right();
    }

    pub fn move_left(&mut self) -> bool {
        self.scrolling.move_left()
    }

    pub fn move_right(&mut self) -> bool {
        self.scrolling.move_right()
    }

    pub fn move_column_to_first(&mut self) {
        self.scrolling.move_column_to_first();
    }

    pub fn move_column_to_last(&mut self) {
        self.scrolling.move_column_to_last();
    }

    pub fn move_down(&mut self) -> bool {
        self.scrolling.move_down()
    }

    pub fn move_up(&mut self) -> bool {
        self.scrolling.move_up()
    }

    pub fn consume_or_expel_window_left(&mut self, window: Option<&W::Id>) {
        self.scrolling.consume_or_expel_window_left(window);
    }

    pub fn consume_or_expel_window_right(&mut self, window: Option<&W::Id>) {
        self.scrolling.consume_or_expel_window_right(window);
    }

    pub fn consume_into_column(&mut self) {
        self.scrolling.consume_into_column();
    }

    pub fn expel_from_column(&mut self) {
        self.scrolling.expel_from_column();
    }

    pub fn center_column(&mut self) {
        self.scrolling.center_column();
    }

    pub fn toggle_width(&mut self) {
        self.scrolling.toggle_width();
    }

    pub fn toggle_full_width(&mut self) {
        self.scrolling.toggle_full_width();
    }

    pub fn set_column_width(&mut self, change: SizeChange) {
        self.scrolling.set_column_width(change);
    }

    pub fn set_window_height(&mut self, window: Option<&W::Id>, change: SizeChange) {
        self.scrolling.set_window_height(window, change);
    }

    pub fn reset_window_height(&mut self, window: Option<&W::Id>) {
        self.scrolling.reset_window_height(window);
    }

    pub fn toggle_window_height(&mut self, window: Option<&W::Id>) {
        self.scrolling.toggle_window_height(window);
    }

    pub fn set_fullscreen(&mut self, window: &W::Id, is_fullscreen: bool) {
        self.scrolling.set_fullscreen(window, is_fullscreen);
    }

    pub fn toggle_fullscreen(&mut self, window: &W::Id) {
        self.scrolling.toggle_fullscreen(window);
    }

    pub fn has_windows(&self) -> bool {
        self.windows().next().is_some()
    }

    pub fn has_window(&self, window: &W::Id) -> bool {
        self.windows().any(|win| win.id() == window)
    }

    pub fn find_wl_surface(&self, wl_surface: &WlSurface) -> Option<&W> {
        self.windows().find(|win| win.is_wl_surface(wl_surface))
    }

    pub fn find_wl_surface_mut(&mut self, wl_surface: &WlSurface) -> Option<&mut W> {
        self.windows_mut().find(|win| win.is_wl_surface(wl_surface))
    }

    pub fn tiles_with_render_positions(
        &self,
    ) -> impl Iterator<Item = (&Tile<W>, Point<f64, Logical>)> {
        self.scrolling.tiles_with_render_positions()
    }

    pub fn tiles_with_render_positions_mut(
        &mut self,
        round: bool,
    ) -> impl Iterator<Item = (&mut Tile<W>, Point<f64, Logical>)> {
        self.scrolling.tiles_with_render_positions_mut(round)
    }

    pub fn active_tile_visual_rectangle(&self) -> Option<Rectangle<f64, Logical>> {
        self.scrolling.active_tile_visual_rectangle()
    }

    pub fn popup_target_rect(&self, window: &W::Id) -> Option<Rectangle<f64, Logical>> {
        self.scrolling.popup_target_rect(window)
    }

    pub fn render_elements<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        target: RenderTarget,
    ) -> impl Iterator<Item = WorkspaceRenderElement<R>> {
        let scale = Scale::from(self.scale.fractional_scale());
        let scrolling = self.scrolling.render_elements(renderer, scale, target);
        scrolling.into_iter().map(WorkspaceRenderElement::from)
    }

    pub fn render_above_top_layer(&self) -> bool {
        self.scrolling.render_above_top_layer()
    }

    pub fn store_unmap_snapshot_if_empty(&mut self, renderer: &mut GlesRenderer, window: &W::Id) {
        let output_scale = Scale::from(self.scale.fractional_scale());
        let view_size = self.view_size();
        for (tile, tile_pos) in self.tiles_with_render_positions_mut(false) {
            if tile.window().id() == window {
                let view_pos = Point::from((-tile_pos.x, -tile_pos.y));
                let view_rect = Rectangle::from_loc_and_size(view_pos, view_size);
                tile.update(false, view_rect);
                tile.store_unmap_snapshot_if_empty(renderer, output_scale);
                return;
            }
        }
    }

    pub fn clear_unmap_snapshot(&mut self, window: &W::Id) {
        for tile in self.tiles_mut() {
            if tile.window().id() == window {
                let _ = tile.take_unmap_snapshot();
                return;
            }
        }
    }

    pub fn start_close_animation_for_window(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &W::Id,
        blocker: TransactionBlocker,
    ) {
        self.scrolling
            .start_close_animation_for_window(renderer, window, blocker);
    }

    pub fn start_close_animation_for_tile(
        &mut self,
        renderer: &mut GlesRenderer,
        snapshot: TileRenderSnapshot,
        tile_size: Size<f64, Logical>,
        tile_pos: Point<f64, Logical>,
        blocker: TransactionBlocker,
    ) {
        // FIXME: when floating happens, put this on the floating layer. It's used for interactive
        // move, which is floating rather than scrolling.
        let tile_pos = tile_pos + Point::from((self.scrolling.view_pos(), 0.));
        self.scrolling
            .start_close_animation_for_tile(renderer, snapshot, tile_size, tile_pos, blocker);
    }

    pub fn window_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(&W, Option<Point<f64, Logical>>)> {
        self.tiles_with_render_positions()
            .find_map(|(tile, tile_pos)| {
                let pos_within_tile = pos - tile_pos;

                if tile.is_in_input_region(pos_within_tile) {
                    let pos_within_surface = tile_pos + tile.buf_loc();
                    return Some((tile.window(), Some(pos_within_surface)));
                } else if tile.is_in_activation_region(pos_within_tile) {
                    return Some((tile.window(), None));
                }

                None
            })
    }

    pub fn resize_edges_under(&self, pos: Point<f64, Logical>) -> Option<ResizeEdge> {
        self.tiles_with_render_positions()
            .find_map(|(tile, tile_pos)| {
                let pos_within_tile = pos - tile_pos;

                // This logic should be consistent with window_under() in when it returns Some vs.
                // None.
                if tile.is_in_input_region(pos_within_tile)
                    || tile.is_in_activation_region(pos_within_tile)
                {
                    let size = tile.tile_size().to_f64();

                    let mut edges = ResizeEdge::empty();
                    if pos_within_tile.x < size.w / 3. {
                        edges |= ResizeEdge::LEFT;
                    } else if 2. * size.w / 3. < pos_within_tile.x {
                        edges |= ResizeEdge::RIGHT;
                    }
                    if pos_within_tile.y < size.h / 3. {
                        edges |= ResizeEdge::TOP;
                    } else if 2. * size.h / 3. < pos_within_tile.y {
                        edges |= ResizeEdge::BOTTOM;
                    }
                    return Some(edges);
                }

                None
            })
    }

    pub fn update_window(&mut self, window: &W::Id, serial: Option<Serial>) {
        self.scrolling.update_window(window, serial);
    }

    pub fn refresh(&mut self, is_active: bool) {
        self.scrolling.refresh(is_active);
    }

    pub fn scroll_amount_to_activate(&self, window: &W::Id) -> f64 {
        self.scrolling.scroll_amount_to_activate(window)
    }

    pub fn activate_window(&mut self, window: &W::Id) -> bool {
        self.scrolling.activate_window(window)
    }

    pub fn set_insert_hint(&mut self, insert_hint: InsertHint) {
        self.scrolling.set_insert_hint(insert_hint);
    }

    pub fn clear_insert_hint(&mut self) {
        self.scrolling.clear_insert_hint();
    }

    pub fn get_insert_position(&self, pos: Point<f64, Logical>) -> InsertPosition {
        self.scrolling.get_insert_position(pos)
    }

    pub fn view_offset_gesture_begin(&mut self, is_touchpad: bool) {
        self.scrolling.view_offset_gesture_begin(is_touchpad);
    }

    pub fn view_offset_gesture_update(
        &mut self,
        delta_x: f64,
        timestamp: Duration,
        is_touchpad: bool,
    ) -> Option<bool> {
        self.scrolling
            .view_offset_gesture_update(delta_x, timestamp, is_touchpad)
    }

    pub fn view_offset_gesture_end(&mut self, cancelled: bool, is_touchpad: Option<bool>) -> bool {
        self.scrolling
            .view_offset_gesture_end(cancelled, is_touchpad)
    }

    pub fn interactive_resize_begin(&mut self, window: W::Id, edges: ResizeEdge) -> bool {
        self.scrolling.interactive_resize_begin(window, edges)
    }

    pub fn interactive_resize_update(
        &mut self,
        window: &W::Id,
        delta: Point<f64, Logical>,
    ) -> bool {
        self.scrolling.interactive_resize_update(window, delta)
    }

    pub fn interactive_resize_end(&mut self, window: Option<&W::Id>) {
        self.scrolling.interactive_resize_end(window);
    }

    #[cfg(test)]
    pub fn scrolling(&self) -> &ScrollingSpace<W> {
        &self.scrolling
    }

    #[cfg(test)]
    pub fn verify_invariants(&self, move_win_id: Option<&W::Id>) {
        use approx::assert_abs_diff_eq;

        let scale = self.scale.fractional_scale();
        assert!(self.view_size.w > 0.);
        assert!(self.view_size.h > 0.);
        assert!(scale > 0.);
        assert!(scale.is_finite());

        assert_eq!(self.view_size, self.scrolling.view_size());
        assert_eq!(&self.clock, self.scrolling.clock());
        assert!(Rc::ptr_eq(&self.options, self.scrolling.options()));
        self.scrolling.verify_invariants(self.working_area);

        for (tile, tile_pos) in self.tiles_with_render_positions() {
            if Some(tile.window().id()) != move_win_id {
                assert_eq!(tile.interactive_move_offset, Point::from((0., 0.)));
            }

            let rounded_pos = tile_pos.to_physical_precise_round(scale).to_logical(scale);

            // Tile positions must be rounded to physical pixels.
            assert_abs_diff_eq!(tile_pos.x, rounded_pos.x, epsilon = 1e-5);
            assert_abs_diff_eq!(tile_pos.y, rounded_pos.y, epsilon = 1e-5);
        }
    }
}

fn compute_working_area(output: &Output) -> Rectangle<f64, Logical> {
    layer_map_for_output(output).non_exclusive_zone().to_f64()
}

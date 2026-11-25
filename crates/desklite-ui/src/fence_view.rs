use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use desklite_core::{
    DisplayTopology, FenceKind, FenceSpec, MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH, NativeLayoutConfig,
    Rect, clamp_rect_to_monitors,
};
use desklite_win32::{
    IconCache, attach_desktop_owner_and_rounded_corners, find_window_by_exact_title, launch_item,
    resolve_dropped_item, reveal_in_explorer, start_file_drag_out,
};
use egui::{
    Align, Color32, ColorImage, CornerRadius, CursorIcon, Frame, Layout, Margin, PointerButton,
    Pos2, ResizeDirection, RichText, ScrollArea, Sense, Stroke, TextureHandle, TextureOptions, Ui,
    Vec2, ViewportCommand, ViewportId,
};

use crate::snap::snap_move_rect;

pub const COLLAPSED_HEIGHT: f32 = 38.0;
const RESIZE_EDGE: f32 = 6.0;
const HEADER_BTN_RESERVE_W: f32 = 92.0;

#[derive(Debug, Clone)]
pub enum FenceCommand {
    RequestCreateBlank,
    RequestCreatePortal,
    RequestBindPortal(i32),
    RequestDelete(i32),
    RequestExitApp,
    SyncPortals,
}

#[derive(Debug, Clone)]
pub struct RenamePrompt {
    pub fence_id: i32,
    pub draft: String,
    pub needs_focus: bool,
}

#[derive(Debug, Clone)]
pub struct DragSourceState {
    pub fence_id: i32,
    pub path: String,
    pub finished_at: Option<Instant>,
}

pub struct SharedFenceState {
    pub config: NativeLayoutConfig,
    pub topology: DisplayTopology,
    pub icon_cache: IconCache,
    pub textures: HashMap<String, Option<TextureHandle>>,
    pub attached_hwnds: HashMap<i32, isize>,
    pub builder_rects: HashMap<i32, (Rect, bool)>,
    pub rename_prompt: Option<RenamePrompt>,
    pub selected_item: HashMap<i32, usize>,
    pub last_title_click: HashMap<i32, Instant>,
    pub dragging_from_fence: Option<DragSourceState>,
    pub pending_commands: Vec<FenceCommand>,
    pub dirty_layout: bool,
    pub pending_save_at: Option<Instant>,
}

impl SharedFenceState {
    pub fn new(config: NativeLayoutConfig, topology: DisplayTopology, icon_cache: IconCache) -> Self {
        let builder_rects = config
            .fences
            .iter()
            .map(|f| (f.id, (f.rect, f.collapsed)))
            .collect();
        Self {
            config,
            topology,
            icon_cache,
            textures: HashMap::new(),
            attached_hwnds: HashMap::new(),
            builder_rects,
            rename_prompt: None,
            selected_item: HashMap::new(),
            last_title_click: HashMap::new(),
            dragging_from_fence: None,
            pending_commands: Vec::new(),
            dirty_layout: false,
            pending_save_at: None,
        }
    }

    pub fn push_command(&mut self, ctx: &egui::Context, cmd: FenceCommand) {
        self.pending_commands.push(cmd);
        ctx.request_repaint_of(ViewportId::ROOT);
    }

    pub fn mark_debounced_save(&mut self, ctx: &egui::Context) {
        self.dirty_layout = true;
        self.pending_save_at = Some(Instant::now() + Duration::from_millis(300));
        ctx.request_repaint_of(ViewportId::ROOT);
    }

    pub fn get_or_load_texture(&mut self, ctx: &egui::Context, path: &str) -> Option<TextureHandle> {
        let key = path.to_ascii_lowercase();
        if let Some(cached) = self.textures.get(&key) {
            return cached.clone();
        }
        let Some(decoded) = self.icon_cache.get_or_load(path).cloned() else {
            self.textures.insert(key, None);
            return None;
        };
        let mut rgba = decoded.pbgra;
        for px in rgba.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        let img = ColorImage::from_rgba_premultiplied(
            [decoded.width as usize, decoded.height as usize],
            &rgba,
        );
        let handle = ctx.load_texture(key.clone(), img, TextureOptions::LINEAR);
        self.textures.insert(key, Some(handle.clone()));
        Some(handle)
    }

    pub fn sync_builder_rects_from_config(&mut self) {
        self.builder_rects = self
            .config
            .fences
            .iter()
            .map(|f| (f.id, (f.rect, f.collapsed)))
            .collect();
    }

    pub fn prune_removed_fences(&mut self) {
        let active_ids: HashSet<i32> = self.config.fences.iter().map(|f| f.id).collect();
        self.attached_hwnds.retain(|id, _| active_ids.contains(id));
        self.builder_rects.retain(|id, _| active_ids.contains(id));
        self.selected_item.retain(|id, _| active_ids.contains(id));
    }
}

#[derive(Debug, Default)]
pub struct FenceUiOutput {
    pub needs_immediate_save: bool,
}

pub fn configure_egui_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    // Popup menus & tooltips use window_fill; viewports still clear to [0,0,0,0] via clear_color.
    visuals.window_fill = Color32::from_rgba_unmultiplied(30, 34, 44, 246);
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 55));
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;
    ctx.set_visuals(visuals);
}

fn ensure_fence_attached(fence_id: i32, shared: &Arc<Mutex<SharedFenceState>>) {
    let already = shared.lock().unwrap().attached_hwnds.contains_key(&fence_id);
    if already {
        return;
    }
    let Some(raw_hwnd) = find_window_by_exact_title(&format!("DeskLite#{fence_id}")) else {
        return;
    };
    attach_desktop_owner_and_rounded_corners(raw_hwnd);
    shared.lock().unwrap().attached_hwnds.insert(fence_id, raw_hwnd);
}

pub fn render_fence_viewport(
    ui: &mut Ui,
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> FenceUiOutput {
    let ctx = ui.ctx().clone();
    let mut out = FenceUiOutput::default();
    ensure_fence_attached(fence_id, shared);

    // 1. Read fence snapshot & preferences once per frame
    let (spec_opt, cell_w, cell_h, snap_px, monitors, sibling_rects, is_renaming_this, selected_idx) = {
        let mut guard = shared.lock().unwrap();
        // Expire stale inter-fence drag source after 2 seconds
        if let Some(ref ds) = guard.dragging_from_fence
            && let Some(done_at) = ds.finished_at
            && done_at.elapsed() > Duration::from_secs(2)
        {
            guard.dragging_from_fence = None;
        }
        let spec = guard.config.fences.iter().find(|f| f.id == fence_id).cloned();
        let siblings: Vec<Rect> = guard
            .config
            .fences
            .iter()
            .filter(|f| f.id != fence_id)
            .map(|f| f.rect)
            .collect();
        let renaming = guard
            .rename_prompt
            .as_ref()
            .is_some_and(|r| r.fence_id == fence_id);
        let sel = guard.selected_item.get(&fence_id).copied();
        (
            spec,
            guard.config.preferences.icon_cell_w.max(64) as f32,
            guard.config.preferences.icon_cell_h.max(72) as f32,
            guard.config.preferences.snap_threshold_px,
            guard.topology.monitors().to_vec(),
            siblings,
            renaming,
            sel,
        )
    };

    let Some(spec) = spec_opt else {
        ctx.send_viewport_cmd(ViewportCommand::Close);
        return out;
    };

    // 2. Sync user move/resize back into physical pixels with DPI scaling & debounced save
    let ppp = ctx.pixels_per_point().max(0.5);
    let outer_rect = ctx.input(|i| i.viewport().outer_rect);
    let any_pointer_down = ctx.input(|i| i.pointer.any_down());
    let alt_held = ctx.input(|i| i.modifiers.alt);
    if let Some(or) = outer_rect
        && !any_pointer_down
    {
        let cur_phys = Rect::new(
            (or.min.x * ppp).round() as i32,
            (or.min.y * ppp).round() as i32,
            (or.max.x * ppp).round() as i32,
            (or.max.y * ppp).round() as i32,
        );
        let collapsed_phys_h = (COLLAPSED_HEIGHT * ppp).round() as i32;
        let expected_h = if spec.collapsed {
            collapsed_phys_h
        } else {
            spec.rect.height()
        };
        let resized = (cur_phys.width() - spec.rect.width()).abs() > 2
            || (!spec.collapsed && (cur_phys.height() - expected_h).abs() > 2);
        let moved = (cur_phys.left - spec.rect.left).abs() > 2
            || (cur_phys.top - spec.rect.top).abs() > 2;

        if (resized || moved) && cur_phys.width() >= MIN_FENCE_WIDTH {
            let final_rect = if resized {
                let candidate = if spec.collapsed {
                    Rect::new(
                        cur_phys.left,
                        cur_phys.top,
                        cur_phys.right,
                        cur_phys.top + spec.rect.height().max(MIN_FENCE_HEIGHT),
                    )
                } else {
                    cur_phys
                };
                clamp_rect_to_monitors(candidate, &monitors)
                    .map(|(r, _)| r)
                    .unwrap_or(candidate)
            } else {
                // Move-only: use collapsed bar height when collapsed so bar can reach bottom edge
                let move_box = if spec.collapsed {
                    Rect::new(
                        cur_phys.left,
                        cur_phys.top,
                        cur_phys.right,
                        cur_phys.top + collapsed_phys_h,
                    )
                } else {
                    cur_phys
                };
                let snapped = snap_move_rect(move_box, &sibling_rects, &monitors, snap_px, alt_held);
                if (snapped.left - cur_phys.left).abs() > 1 || (snapped.top - cur_phys.top).abs() > 1
                {
                    ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(
                        snapped.left as f32 / ppp,
                        snapped.top as f32 / ppp,
                    )));
                }
                if spec.collapsed {
                    Rect::new(
                        snapped.left,
                        snapped.top,
                        snapped.right,
                        snapped.top + spec.rect.height().max(MIN_FENCE_HEIGHT),
                    )
                } else {
                    snapped
                }
            };

            let mut guard = shared.lock().unwrap();
            if let Some(f) = guard.config.fence_mut(fence_id) {
                f.rect = final_rect;
            }
            guard.config.update_active_snapshot_rect(fence_id, final_rect);
            guard.mark_debounced_save(&ctx);
        }
    }

    // 3. Handle 8-edge resize borders cleanly (excluding top-right header control buttons)
    let win_rect = ui.max_rect();
    if let Some(pos) = ctx.input(|i| i.pointer.hover_pos())
        && let Some(dir) = detect_resize_edge(win_rect, pos, spec.collapsed)
    {
        ctx.set_cursor_icon(resize_cursor(dir));
        if ctx.input(|i| i.pointer.button_pressed(PointerButton::Primary)) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }

    // 4. Handle files dragged over / dropped onto this fence
    let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let dropped_paths: Vec<String> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .collect()
    });
    if !dropped_paths.is_empty() && spec.portal_dir.is_none() {
        let mut guard = shared.lock().unwrap();
        let source_transfer = guard.dragging_from_fence.take();
        if let Some(src_info) = source_transfer
            && src_info.fence_id != fence_id
            && let Some(src) = guard.config.fence_mut(src_info.fence_id)
            && src.portal_dir.is_none()
        {
            src.items
                .retain(|it| !it.path.eq_ignore_ascii_case(&src_info.path));
            for (idx, it) in src.items.iter_mut().enumerate() {
                it.order = idx as i32;
            }
            ctx.request_repaint_of(ViewportId::from_hash_of(("desklite_fence", src_info.fence_id)));
        }
        if let Some(f) = guard.config.fence_mut(fence_id) {
            for raw in dropped_paths {
                if !f.items.iter().any(|it| it.path.eq_ignore_ascii_case(&raw)) {
                    let order = f.items.len() as i32;
                    f.items.push(resolve_dropped_item(Path::new(&raw), order));
                }
            }
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
        }
        ctx.request_repaint();
    }

    let is_portal = spec.portal_dir.is_some();
    let border_stroke = if hovering_files && !is_portal {
        Stroke::new(2.0, Color32::from_rgb(96, 165, 250))
    } else if hovering_files && is_portal {
        Stroke::new(2.0, Color32::from_rgb(245, 158, 11))
    } else if is_portal {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(110, 185, 255, 110))
    } else {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 45))
    };

    let bg_alpha = spec.backdrop_alpha.clamp(120, 230);
    let panel_frame = Frame::new()
        .fill(Color32::from_rgba_unmultiplied(22, 24, 30, bg_alpha))
        .stroke(border_stroke)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(0));

    egui::CentralPanel::default().frame(panel_frame).show(ui, |ui| {
        // ---- Title bar ----
        let title_bar_frame = Frame::new()
            .fill(Color32::from_rgba_unmultiplied(32, 36, 46, 215))
            .corner_radius(CornerRadius {
                nw: 10,
                ne: 10,
                sw: if spec.collapsed { 10 } else { 0 },
                se: if spec.collapsed { 10 } else { 0 },
            })
            .inner_margin(Margin::symmetric(10, 6));

        title_bar_frame.show(ui, |ui| {
            ui.set_height(COLLAPSED_HEIGHT - 12.0);
            // Lay out right-side buttons FIRST so they never overflow at narrow widths
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let menu_resp = draw_header_icon_btn(ui, "格子菜单", |painter, r, fg| {
                    let cx = r.center().x;
                    let cy = r.center().y;
                    for dy in [-4.5_f32, 0.0, 4.5] {
                        painter.circle_filled(Pos2::new(cx, cy + dy), 1.4, fg);
                    }
                });
                egui::Popup::menu(&menu_resp).show(|ui| {
                    render_fence_menu_items(ui, fence_id, &spec, shared, &ctx, &mut out);
                });

                let is_list = matches!(spec.kind, FenceKind::FileList);
                if draw_header_icon_btn(ui, "切换网格 / 列表视图", |painter, r, fg| {
                    let c = r.center();
                    if is_list {
                        for dy in [-3.5_f32, 0.0, 3.5] {
                            painter.line_segment(
                                [Pos2::new(c.x - 5.0, c.y + dy), Pos2::new(c.x + 5.0, c.y + dy)],
                                Stroke::new(1.5, fg),
                            );
                        }
                    } else {
                        for dx in [-4.0_f32, 1.0] {
                            for dy in [-4.0_f32, 1.0] {
                                painter.rect_filled(
                                    egui::Rect::from_min_size(
                                        Pos2::new(c.x + dx, c.y + dy),
                                        Vec2::splat(3.5),
                                    ),
                                    CornerRadius::same(1),
                                    fg,
                                );
                            }
                        }
                    }
                })
                .clicked()
                {
                    let mut guard = shared.lock().unwrap();
                    if let Some(f) = guard.config.fence_mut(fence_id) {
                        f.kind = match f.kind {
                            FenceKind::FileList => FenceKind::IconGrid,
                            _ => FenceKind::FileList,
                        };
                        guard.dirty_layout = true;
                        out.needs_immediate_save = true;
                    }
                }

                let collapsed = spec.collapsed;
                if draw_header_icon_btn(
                    ui,
                    if collapsed { "展开格子" } else { "折叠格子" },
                    |painter, r, fg| {
                        let c = r.center();
                        let stroke = Stroke::new(1.6, fg);
                        if collapsed {
                            painter.line_segment(
                                [Pos2::new(c.x - 2.0, c.y - 4.0), Pos2::new(c.x + 2.5, c.y)],
                                stroke,
                            );
                            painter.line_segment(
                                [Pos2::new(c.x + 2.5, c.y), Pos2::new(c.x - 2.0, c.y + 4.0)],
                                stroke,
                            );
                        } else {
                            painter.line_segment(
                                [Pos2::new(c.x - 4.0, c.y - 2.0), Pos2::new(c.x, c.y + 2.5)],
                                stroke,
                            );
                            painter.line_segment(
                                [Pos2::new(c.x, c.y + 2.5), Pos2::new(c.x + 4.0, c.y - 2.0)],
                                stroke,
                            );
                        }
                    },
                )
                .clicked()
                {
                    toggle_collapse(fence_id, &spec, shared, &ctx, &mut out);
                }

                // Remaining left width belongs to title / rename edit
                let rem_w = ui.available_width().max(24.0);
                if is_renaming_this {
                    let mut commit = None;
                    let mut cancel = false;
                    {
                        let mut guard = shared.lock().unwrap();
                        if let Some(ref mut rp) = guard.rename_prompt {
                            if rp.needs_focus {
                                ctx.send_viewport_cmd(ViewportCommand::Focus);
                            }
                            let resp = ui.add_sized(
                                [rem_w, 22.0],
                                egui::TextEdit::singleline(&mut rp.draft).desired_width(rem_w),
                            );
                            if rp.needs_focus {
                                resp.request_focus();
                                rp.needs_focus = false;
                            }
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                let trimmed = rp.draft.trim().to_string();
                                if !trimmed.is_empty() {
                                    commit = Some(trimmed);
                                } else {
                                    cancel = true;
                                }
                            } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                cancel = true;
                            } else if resp.lost_focus() {
                                let trimmed = rp.draft.trim().to_string();
                                if !trimmed.is_empty() {
                                    commit = Some(trimmed);
                                } else {
                                    cancel = true;
                                }
                            }
                        }
                    }
                    if let Some(new_name) = commit {
                        let mut guard = shared.lock().unwrap();
                        if let Some(f) = guard.config.fence_mut(fence_id) {
                            f.name = new_name;
                            guard.dirty_layout = true;
                            out.needs_immediate_save = true;
                        }
                        guard.rename_prompt = None;
                    } else if cancel {
                        shared.lock().unwrap().rename_prompt = None;
                    }
                } else {
                    let display_title = if spec.name.is_empty() {
                        "未命名格子".to_string()
                    } else if is_portal {
                        format!("{} [文件夹]", spec.name)
                    } else {
                        spec.name.clone()
                    };

                    let (title_rect, _) =
                        ui.allocate_exact_size(Vec2::new(rem_w, 24.0), Sense::hover());
                    let title_resp = ui.interact(
                        title_rect.expand2(Vec2::new(0.0, 4.0)),
                        ui.make_persistent_id(("title_bar", fence_id)),
                        Sense::click_and_drag(),
                    );
                    let clipped_title = ui.painter().with_clip_rect(title_rect);
                    let title_font = egui::FontId::proportional(13.5);
                    let fitted_title = fit_single_line(
                        ui.painter(),
                        &display_title,
                        title_font.clone(),
                        (rem_w - 6.0).max(16.0),
                    );
                    clipped_title.text(
                        Pos2::new(title_rect.min.x + 2.0, title_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        fitted_title,
                        title_font,
                        Color32::from_rgb(242, 245, 252),
                    );

                    let pointer_dbl = ctx.input(|i| {
                        i.pointer.button_double_clicked(PointerButton::Primary)
                            && i.pointer
                                .interact_pos()
                                .is_some_and(|p| title_rect.expand(4.0).contains(p))
                    });
                    let mut quick_second_click = false;
                    if title_resp.clicked() {
                        let now = Instant::now();
                        let mut guard = shared.lock().unwrap();
                        if let Some(prev) = guard.last_title_click.get(&fence_id)
                            && now.duration_since(*prev) <= Duration::from_millis(550)
                        {
                            quick_second_click = true;
                            guard.last_title_click.remove(&fence_id);
                        } else {
                            guard.last_title_click.insert(fence_id, now);
                        }
                    }

                    if title_resp.double_clicked() || pointer_dbl || quick_second_click {
                        toggle_collapse(fence_id, &spec, shared, &ctx, &mut out);
                    } else if title_resp.dragged_by(PointerButton::Primary) {
                        let moved_enough = ctx.input(|i| {
                            i.pointer
                                .press_origin()
                                .zip(i.pointer.interact_pos())
                                .is_some_and(|(a, b)| (b - a).length() > 4.0)
                        });
                        if moved_enough {
                            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                        }
                    }
                    title_resp.context_menu(|ui| {
                        render_fence_menu_items(ui, fence_id, &spec, shared, &ctx, &mut out);
                    });
                }
            });
        });

        if spec.collapsed {
            return;
        }

        // ---- Body area ----
        let body_frame = Frame::new().inner_margin(Margin::symmetric(8, 6));
        body_frame.show(ui, |ui| {
            if hovering_files {
                if is_portal {
                    ui.colored_label(
                        Color32::from_rgb(251, 191, 36),
                        "文件夹映射格子由磁盘目录同步，不支持拖入",
                    );
                } else {
                    ui.colored_label(
                        Color32::from_rgb(130, 195, 255),
                        "松开鼠标收纳到此格子",
                    );
                }
            } else if spec.items.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new("可从桌面或资源管理器拖入文件\n也可以拖动条目放到桌面或其它格子")
                            .size(12.0)
                            .color(Color32::from_rgba_unmultiplied(200, 210, 228, 175)),
                    );
                });
                return;
            }

            let mut item_action: Option<ItemUiAction> = None;

            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let avail_w = ui.available_width().max(cell_w);
                    match spec.kind {
                        FenceKind::FileList => {
                            for (idx, item) in spec.items.iter().enumerate() {
                                let tex = {
                                    let mut guard = shared.lock().unwrap();
                                    guard.get_or_load_texture(&ctx, &item.path)
                                };
                                let selected = selected_idx == Some(idx);
                                let row_resp = render_list_item(ui, item, tex.as_ref(), selected);
                                handle_item_interactions(
                                    &row_resp,
                                    idx,
                                    item,
                                    is_portal,
                                    &mut item_action,
                                );
                            }
                        }
                        FenceKind::IconGrid | FenceKind::Unknown(_) => {
                            let cols = ((avail_w / cell_w).floor() as usize).max(1);
                            egui::Grid::new(("fence_grid", fence_id))
                                .num_columns(cols)
                                .spacing([4.0, 6.0])
                                .show(ui, |ui| {
                                    for (idx, item) in spec.items.iter().enumerate() {
                                        let tex = {
                                            let mut guard = shared.lock().unwrap();
                                            guard.get_or_load_texture(&ctx, &item.path)
                                        };
                                        let selected = selected_idx == Some(idx);
                                        let tile_resp = render_grid_item(
                                            ui,
                                            item,
                                            tex.as_ref(),
                                            selected,
                                            Vec2::new(cell_w - 4.0, cell_h),
                                        );
                                        handle_item_interactions(
                                            &tile_resp,
                                            idx,
                                            item,
                                            is_portal,
                                            &mut item_action,
                                        );
                                        if (idx + 1) % cols == 0 {
                                            ui.end_row();
                                        }
                                    }
                                });
                        }
                    }
                });

            if let Some(act) = item_action {
                match act {
                    ItemUiAction::Select(idx) => {
                        shared.lock().unwrap().selected_item.insert(fence_id, idx);
                    }
                    ItemUiAction::Launch(path) => {
                        let _ = launch_item(&path);
                    }
                    ItemUiAction::Reveal(path) => {
                        let _ = reveal_in_explorer(&path);
                    }
                    ItemUiAction::Remove(idx) => {
                        let mut guard = shared.lock().unwrap();
                        if let Some(f) = guard.config.fence_mut(fence_id)
                            && f.portal_dir.is_none()
                            && idx < f.items.len()
                        {
                            f.items.remove(idx);
                            for (i, it) in f.items.iter_mut().enumerate() {
                                it.order = i as i32;
                            }
                            guard.dirty_layout = true;
                            out.needs_immediate_save = true;
                        }
                    }
                    ItemUiAction::StartDragOut(path) => {
                        {
                            let mut guard = shared.lock().unwrap();
                            guard.dragging_from_fence = Some(DragSourceState {
                                fence_id,
                                path: path.clone(),
                                finished_at: None,
                            });
                        }
                        let _ = start_file_drag_out(&[path]);
                        {
                            let mut guard = shared.lock().unwrap();
                            if let Some(ref mut ds) = guard.dragging_from_fence {
                                ds.finished_at = Some(Instant::now());
                            }
                        }
                        ctx.request_repaint();
                    }
                }
            }
        });
    });

    out
}

enum ItemUiAction {
    Select(usize),
    Launch(String),
    Reveal(String),
    Remove(usize),
    StartDragOut(String),
}

fn handle_item_interactions(
    resp: &egui::Response,
    idx: usize,
    item: &desklite_core::FenceItem,
    is_portal: bool,
    out_action: &mut Option<ItemUiAction>,
) {
    if resp.clicked() {
        *out_action = Some(ItemUiAction::Select(idx));
    }
    if resp.double_clicked() {
        *out_action = Some(ItemUiAction::Launch(item.path.clone()));
    }
    if resp.drag_started_by(PointerButton::Primary) {
        *out_action = Some(ItemUiAction::StartDragOut(item.path.clone()));
    }
    resp.context_menu(|ui| {
        if ui.button("打开").clicked() {
            *out_action = Some(ItemUiAction::Launch(item.path.clone()));
            ui.close();
        }
        if ui.button("打开所在位置").clicked() {
            *out_action = Some(ItemUiAction::Reveal(item.path.clone()));
            ui.close();
        }
        if !is_portal && ui.button("从格子移除").clicked() {
            *out_action = Some(ItemUiAction::Remove(idx));
            ui.close();
        }
    });
}

fn render_grid_item(
    ui: &mut Ui,
    item: &desklite_core::FenceItem,
    tex: Option<&TextureHandle>,
    selected: bool,
    size: Vec2,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            Color32::from_rgba_unmultiplied(88, 156, 250, 75)
        } else if resp.hovered() {
            Color32::from_rgba_unmultiplied(255, 255, 255, 34)
        } else {
            Color32::TRANSPARENT
        };
        if fill != Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(rect.shrink(2.0), CornerRadius::same(6), fill);
        }

        if let Some(t) = tex {
            let icon_sz = 40.0;
            let icon_rect = egui::Rect::from_min_size(
                Pos2::new(rect.center().x - icon_sz / 2.0, rect.min.y + 6.0),
                Vec2::splat(icon_sz),
            );
            ui.painter().image(
                t.id(),
                icon_rect,
                egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        let clipped = ui.painter().with_clip_rect(rect.shrink(2.0));
        let font = egui::FontId::proportional(11.5);
        let fitted = fit_two_lines(ui.painter(), &item.display_name, font.clone(), (size.x - 8.0).max(20.0));
        let label_galley = ui.painter().layout(
            fitted,
            font,
            Color32::from_rgb(240, 244, 252),
            (size.x - 8.0).max(20.0),
        );
        let text_pos = Pos2::new(
            (rect.center().x - label_galley.size().x / 2.0).max(rect.min.x + 4.0),
            rect.min.y + 50.0,
        );
        clipped.galley(text_pos, label_galley, Color32::WHITE);
    }
    resp.on_hover_text(&item.path)
}

fn render_list_item(
    ui: &mut Ui,
    item: &desklite_core::FenceItem,
    tex: Option<&TextureHandle>,
    selected: bool,
) -> egui::Response {
    let size = Vec2::new(ui.available_width(), 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            Color32::from_rgba_unmultiplied(88, 156, 250, 75)
        } else if resp.hovered() {
            Color32::from_rgba_unmultiplied(255, 255, 255, 34)
        } else {
            Color32::TRANSPARENT
        };
        if fill != Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(5), fill);
        }
        if let Some(t) = tex {
            let icon_rect = egui::Rect::from_min_size(
                Pos2::new(rect.min.x + 6.0, rect.center().y - 9.0),
                Vec2::splat(18.0),
            );
            ui.painter().image(
                t.id(),
                icon_rect,
                egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let text_max_w = (rect.width() - 36.0).max(20.0);
        let font = egui::FontId::proportional(12.5);
        let fitted = fit_single_line(ui.painter(), &item.display_name, font.clone(), text_max_w);
        let clipped = ui.painter().with_clip_rect(rect.shrink2(Vec2::new(2.0, 0.0)));
        clipped.text(
            Pos2::new(rect.min.x + 30.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            fitted,
            font,
            Color32::from_rgb(240, 244, 252),
        );
    }
    resp.on_hover_text(&item.path)
}

fn draw_header_icon_btn(
    ui: &mut Ui,
    tooltip: &str,
    paint_icon: impl FnOnce(&egui::Painter, egui::Rect, Color32),
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    if ui.is_rect_visible(rect) {
        if resp.hovered() || resp.is_pointer_button_down_on() {
            let bg = if resp.is_pointer_button_down_on() {
                Color32::from_rgba_unmultiplied(255, 255, 255, 45)
            } else {
                Color32::from_rgba_unmultiplied(255, 255, 255, 26)
            };
            ui.painter().rect_filled(rect, CornerRadius::same(5), bg);
        }
        let fg = if resp.hovered() {
            Color32::WHITE
        } else {
            Color32::from_rgb(208, 214, 228)
        };
        paint_icon(ui.painter(), rect, fg);
    }
    resp.on_hover_text(tooltip)
}

fn fit_single_line(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    max_width: f32,
) -> String {
    let g = painter.layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE);
    if g.size().x <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo.midpoint(hi).max(lo + 1);
        let cand: String = chars[..mid].iter().chain(std::iter::once(&'…')).collect();
        let w = painter
            .layout_no_wrap(cand, font.clone(), Color32::WHITE)
            .size()
            .x;
        if w <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        "…".to_string()
    } else {
        chars[..lo].iter().chain(std::iter::once(&'…')).collect()
    }
}

fn fit_two_lines(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    max_width: f32,
) -> String {
    let g = painter.layout(text.to_string(), font.clone(), Color32::WHITE, max_width);
    if g.rows.len() <= 2 {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo.midpoint(hi).max(lo + 1);
        let cand: String = chars[..mid].iter().chain(std::iter::once(&'…')).collect();
        let rows = painter
            .layout(cand, font.clone(), Color32::WHITE, max_width)
            .rows
            .len();
        if rows <= 2 {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        "…".to_string()
    } else {
        chars[..lo].iter().chain(std::iter::once(&'…')).collect()
    }
}

fn toggle_collapse(
    fence_id: i32,
    spec: &FenceSpec,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let new_collapsed = !spec.collapsed;
    let ppp = ctx.pixels_per_point().max(0.5);
    let target_h = if new_collapsed {
        COLLAPSED_HEIGHT
    } else {
        (spec.rect.height().max(MIN_FENCE_HEIGHT) as f32) / ppp
    };
    let target_w = (spec.rect.width().max(MIN_FENCE_WIDTH) as f32) / ppp;
    ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(target_w, target_h)));
    let mut guard = shared.lock().unwrap();
    if let Some(f) = guard.config.fence_mut(fence_id) {
        f.collapsed = new_collapsed;
        let r = f.rect;
        guard.builder_rects.insert(fence_id, (r, new_collapsed));
        guard.dirty_layout = true;
        out.needs_immediate_save = true;
    }
}

fn render_fence_menu_items(
    ui: &mut Ui,
    fence_id: i32,
    spec: &FenceSpec,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let collapse_lbl = if spec.collapsed { "展开格子" } else { "折叠格子" };
    if ui.button(collapse_lbl).clicked() {
        toggle_collapse(fence_id, spec, shared, ctx, out);
        ui.close();
    }
    if ui.button("重命名格子").clicked() {
        shared.lock().unwrap().rename_prompt = Some(RenamePrompt {
            fence_id,
            draft: spec.name.clone(),
            needs_focus: true,
        });
        ui.close();
    }
    let view_lbl = match spec.kind {
        FenceKind::FileList => "切换为图标网格",
        _ => "切换为紧凑列表",
    };
    if ui.button(view_lbl).clicked() {
        let mut guard = shared.lock().unwrap();
        if let Some(f) = guard.config.fence_mut(fence_id) {
            f.kind = match f.kind {
                FenceKind::FileList => FenceKind::IconGrid,
                _ => FenceKind::FileList,
            };
            guard.dirty_layout = true;
            out.needs_immediate_save = true;
        }
        ui.close();
    }
    if spec.portal_dir.is_some() {
        if ui.button("取消文件夹映射").clicked() {
            let mut guard = shared.lock().unwrap();
            if let Some(f) = guard.config.fence_mut(fence_id) {
                f.portal_dir = None;
                guard.dirty_layout = true;
                out.needs_immediate_save = true;
            }
            guard.push_command(ctx, FenceCommand::SyncPortals);
            ui.close();
        }
    } else if ui.button("绑定文件夹映射...").clicked() {
        shared
            .lock()
            .unwrap()
            .push_command(ctx, FenceCommand::RequestBindPortal(fence_id));
        ui.close();
    }
    ui.separator();
    if ui.button("新建空白格子").clicked() {
        shared
            .lock()
            .unwrap()
            .push_command(ctx, FenceCommand::RequestCreateBlank);
        ui.close();
    }
    if ui.button("新建文件夹格子...").clicked() {
        shared
            .lock()
            .unwrap()
            .push_command(ctx, FenceCommand::RequestCreatePortal);
        ui.close();
    }
    if ui.button("删除此格子").clicked() {
        shared
            .lock()
            .unwrap()
            .push_command(ctx, FenceCommand::RequestDelete(fence_id));
        ui.close();
    }
    ui.separator();
    if ui.button("退出 DeskLite").clicked() {
        shared
            .lock()
            .unwrap()
            .push_command(ctx, FenceCommand::RequestExitApp);
        ui.close();
    }
}

fn detect_resize_edge(rect: egui::Rect, pos: Pos2, collapsed: bool) -> Option<ResizeDirection> {
    // Keep top-right header control buttons free of accidental resize-edge grabs.
    if pos.y - rect.min.y < COLLAPSED_HEIGHT && rect.max.x - pos.x < HEADER_BTN_RESERVE_W {
        return None;
    }

    let left = pos.x - rect.min.x < RESIZE_EDGE;
    let right = rect.max.x - pos.x < RESIZE_EDGE;
    let top = pos.y - rect.min.y < RESIZE_EDGE;
    let bottom = rect.max.y - pos.y < RESIZE_EDGE;
    if collapsed {
        return match (left, right) {
            (true, _) => Some(ResizeDirection::West),
            (_, true) => Some(ResizeDirection::East),
            _ => None,
        };
    }
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, true, _) => Some(ResizeDirection::North),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

fn resize_cursor(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
    }
}

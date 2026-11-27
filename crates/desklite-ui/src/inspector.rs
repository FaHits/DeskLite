use std::sync::{Arc, Mutex};
use std::time::Instant;

use desklite_core::{FenceKind, MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH, Rect};
use desklite_win32::{
    AutostartMode, find_window_by_exact_title, focus_window_now, live_sizemove_rect_for_hwnd,
    post_uncloak_after_swap, query_autostart_status, set_autostart_mode, set_hwnd_locked,
    set_window_cloaked, style_floating_inspector_window,
};
use egui::{
    Align, Color32, CornerRadius, Frame, Key, Layout, Margin, PointerButton, Pos2, ScrollArea,
    Sense, Stroke, Ui, Vec2, ViewportCommand, ViewportId,
};

use crate::item_grid::draw_header_icon_btn;
use crate::state::{
    FenceUiOutput, INSPECTOR_HEIGHT, INSPECTOR_TITLE, INSPECTOR_WIDTH, InspectorState,
    SharedFenceState, UndoAction, ViewModeAnimState, apply_param_editor_rect,
    duplicate_fence_with_style, format_fence_title, lock_unpoisoned, request_repaint_all_fences,
    resolve_fence_layout,
};

fn clamp_drag_pos_to_monitors(
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    cursor: (i32, i32),
    monitors: &[desklite_core::MonitorDescriptor],
) -> (i32, i32) {
    if monitors.is_empty() {
        return (left, top);
    }
    let mon = desklite_core::nearest_monitor(
        desklite_core::Point {
            x: cursor.0,
            y: cursor.1,
        },
        monitors,
    );
    let work = mon.work_area;
    let max_left = (work.right - width).max(work.left);
    let max_top = (work.bottom - height).max(work.top);
    (left.clamp(work.left, max_left), top.clamp(work.top, max_top))
}

fn compute_inspector_initial_pos(
    spec_rect: Rect,
    monitors: &[desklite_core::MonitorDescriptor],
) -> (i32, i32) {
    if monitors.is_empty() {
        return (spec_rect.right + 14, spec_rect.top);
    }
    let mon = desklite_core::nearest_monitor(spec_rect.center(), monitors);
    let scale = (mon.dpi_x as f32 / 96.0).max(0.5);
    let insp_w = (INSPECTOR_WIDTH * scale).round() as i32;
    let insp_h = (INSPECTOR_HEIGHT * scale).round() as i32;
    let work = mon.work_area;
    let gap = (14.0 * scale).round() as i32;
    let x = if spec_rect.right + gap + insp_w <= work.right {
        spec_rect.right + gap
    } else if spec_rect.left - gap - insp_w >= work.left {
        spec_rect.left - gap - insp_w
    } else {
        (work.right - insp_w).max(work.left)
    };
    let y = spec_rect
        .top
        .clamp(work.top, (work.bottom - insp_h).max(work.top));
    (x, y)
}

fn compute_inspector_center_pos(monitors: &[desklite_core::MonitorDescriptor]) -> (i32, i32) {
    let insp_w = INSPECTOR_WIDTH.round() as i32;
    let insp_h = INSPECTOR_HEIGHT.round() as i32;
    let Some(mon) = monitors.iter().find(|m| m.is_primary).or_else(|| monitors.first()) else {
        return (120, 120);
    };
    let work = mon.work_area;
    let x = work.left + ((work.width() - insp_w) / 2).max(0);
    let y = work.top + ((work.height() - insp_h) / 2).max(0);
    (x, y)
}

fn refresh_autostart_async(shared: &Arc<Mutex<SharedFenceState>>, ctx: &egui::Context) {
    {
        let guard = lock_unpoisoned(shared);
        if guard.autostart_busy {
            return;
        }
    }
    let shared_bg = Arc::clone(shared);
    let ctx_bg = ctx.clone();
    std::thread::spawn(move || {
        let status = query_autostart_status();
        let mut guard = lock_unpoisoned(&shared_bg);
        guard.autostart_status = Some(status);
        guard.request_repaint_inspector(&ctx_bg);
    });
}

fn apply_autostart_mode_async(
    mode: AutostartMode,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) {
    {
        let mut guard = lock_unpoisoned(shared);
        if guard.autostart_busy {
            return;
        }
        guard.autostart_busy = true;
        guard.autostart_feedback = Some(("正在应用启动项设置...".to_string(), false, Instant::now()));
    }
    let shared_bg = Arc::clone(shared);
    let ctx_bg = ctx.clone();
    std::thread::spawn(move || {
        let res = set_autostart_mode(mode);
        let mut guard = lock_unpoisoned(&shared_bg);
        guard.autostart_busy = false;
        match res {
            Ok(outcome) => {
                guard.autostart_status = Some(outcome.status);
                let msg = if outcome.cancelled {
                    ("已取消 UAC 授权，未更改开机启动设置".to_string(), false)
                } else {
                    match mode {
                        AutostartMode::Disabled => ("已关闭开机自启动".to_string(), false),
                        AutostartMode::Normal => ("已开启普通开机自启动（注册表）".to_string(), false),
                        AutostartMode::HighPriority => {
                            ("已开启高优先级开机启动（登录即高优先拉起）".to_string(), false)
                        }
                    }
                };
                guard.autostart_feedback = Some((msg.0, msg.1, Instant::now()));
            }
            Err(err) => {
                guard.autostart_status = Some(query_autostart_status());
                guard.autostart_feedback = Some((format!("设置失败：{err}"), true, Instant::now()));
            }
        }
        guard.request_repaint_inspector(&ctx_bg);
    });
}

fn show_inspector_window(
    initial_pos: (i32, i32),
    existing_hwnd: isize,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    uncollapsed_fence: Option<i32>,
) {
    let insp_vid = ViewportId::from_hash_of("desklite_inspector");
    if existing_hwnd != 0 {
        set_window_cloaked(existing_hwnd, true);
        desklite_win32::move_window_no_z(existing_hwnd, initial_pos.0, initial_pos.1);
        ctx.send_viewport_cmd_to(insp_vid, ViewportCommand::Visible(true));
        focus_window_now(existing_hwnd);
    }
    {
        let guard = lock_unpoisoned(shared);
        if let Some(fid) = uncollapsed_fence {
            guard.request_repaint_fence(ctx, fid);
        }
        guard.request_repaint_inspector(ctx);
    }
    refresh_autostart_async(shared, ctx);
    ctx.request_repaint_of(ViewportId::ROOT);
}

pub fn open_param_editor_for_fence(
    fence_id: i32,
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) {
    let (insp_hwnd, initial_pos, needs_uncollapse) = {
        let mut guard = lock_unpoisoned(shared);
        let Some(spec) = guard.config.fences.iter().find(|f| f.id == fence_id).cloned() else {
            return;
        };
        let initial_pos = compute_inspector_initial_pos(spec.rect, guard.topology.monitors());
        let existing_hwnd = guard.inspector.as_ref().map(|i| i.hwnd).unwrap_or(0);
        let styled = guard.inspector.as_ref().is_some_and(|i| i.styled);
        guard.inspector = Some(InspectorState {
            open: true,
            target_fence_id: fence_id,
            initial_pos,
            hwnd: existing_hwnd,
            styled,
            reveal_frames: 1,
            needs_uncloak: true,
            drag_grab: None,
            draft_name: Some((fence_id, spec.name.clone())),
            name_edit_initial: Some((fence_id, spec.name.clone())),
        });
        guard.inspector_spawned = true;
        let uncollapse = spec.collapsed;
        if uncollapse {
            guard.pending_toggle_collapse.insert(fence_id);
        }
        (existing_hwnd, initial_pos, uncollapse)
    };
    show_inspector_window(
        initial_pos,
        insp_hwnd,
        shared,
        ctx,
        needs_uncollapse.then_some(fence_id),
    );
}

pub fn open_param_editor_for_default(
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
) {
    let target_fence = {
        let guard = lock_unpoisoned(shared);
        guard
            .inspector
            .as_ref()
            .map(|ins| ins.target_fence_id)
            .filter(|fid| guard.config.fences.iter().any(|f| f.id == *fid))
            .or_else(|| guard.config.fences.first().map(|f| f.id))
    };
    if let Some(fid) = target_fence {
        open_param_editor_for_fence(fid, shared, ctx);
        return;
    }

    let (insp_hwnd, initial_pos) = {
        let mut guard = lock_unpoisoned(shared);
        let initial_pos = compute_inspector_center_pos(guard.topology.monitors());
        let existing_hwnd = guard.inspector.as_ref().map(|i| i.hwnd).unwrap_or(0);
        let styled = guard.inspector.as_ref().is_some_and(|i| i.styled);
        guard.inspector = Some(InspectorState {
            open: true,
            target_fence_id: 0,
            initial_pos,
            hwnd: existing_hwnd,
            styled,
            reveal_frames: 1,
            needs_uncloak: true,
            drag_grab: None,
            draft_name: None,
            name_edit_initial: None,
        });
        guard.inspector_spawned = true;
        (existing_hwnd, initial_pos)
    };
    show_inspector_window(initial_pos, insp_hwnd, shared, ctx, None);
}

fn inspector_card_with_header<R>(
    ui: &mut Ui,
    title: &str,
    add_header_right: impl FnOnce(&mut Ui),
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    let card_frame = Frame::new()
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 9))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 16),
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 10));
    card_frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(title)
                        .size(11.5)
                        .strong()
                        .color(Color32::from_rgb(154, 170, 198)),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), add_header_right);
            });
            ui.add_space(6.0);
            add_contents(ui)
        })
        .inner
}

fn inspector_card<R>(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    inspector_card_with_header(ui, title, |_| {}, add_contents)
}

fn labeled_row(ui: &mut Ui, label: &str, add_control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(72.0, 22.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(label)
                        .size(12.0)
                        .color(Color32::from_rgb(218, 224, 236)),
                );
            },
        );
        ui.with_layout(Layout::left_to_right(Align::Center), add_control);
    });
}

fn px_slider(ui: &mut Ui, val: &mut i32, range: std::ops::RangeInclusive<i32>) -> bool {
    let w = (ui.available_width() - 70.0).max(72.0);
    ui.spacing_mut().slider_width = w;
    ui.add(egui::Slider::new(val, range).suffix(" px")).changed()
}

pub fn close_inspector_safely(
    shared: &Arc<Mutex<SharedFenceState>>,
    ctx: &egui::Context,
    out: &mut FenceUiOutput,
) {
    let mut repainted_fid = None;
    let insp_hwnd_for_hide = {
        let mut guard = lock_unpoisoned(shared);
        let h = guard.inspector.as_ref().map(|i| i.hwnd).unwrap_or(0);
        let (draft_to_commit, initial_name) = if let Some(ref mut ins) = guard.inspector {
            ins.open = false;
            ins.needs_uncloak = false;
            ins.reveal_frames = 0;
            ins.drag_grab = None;
            (ins.draft_name.take(), ins.name_edit_initial.take())
        } else {
            (None, None)
        };
        if let Some((fid, draft)) = draft_to_commit {
            let trimmed = draft.trim().to_string();
            if !trimmed.is_empty()
                && let Some(f) = guard.config.fence_mut(fid)
            {
                let baseline = initial_name
                    .filter(|(ifid, _)| *ifid == fid)
                    .map(|(_, s)| s)
                    .unwrap_or_else(|| f.name.clone());
                f.name = trimmed.clone();
                if baseline != trimmed {
                    guard.push_undo(UndoAction::RenameFence {
                        fence_id: fid,
                        old_name: baseline,
                    });
                }
                repainted_fid = Some(fid);
            }
        }
        guard.dirty_layout = true;
        h
    };
    out.needs_immediate_save = true;
    if insp_hwnd_for_hide != 0 {
        set_window_cloaked(insp_hwnd_for_hide, true);
    }
    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    if let Some(fid) = repainted_fid {
        lock_unpoisoned(shared).request_repaint_fence(ctx, fid);
    }
    ctx.request_repaint_of(ViewportId::ROOT);
}

/// Floating live-preview settings window positioned beside the target fence. Uses `Visible(false)`
/// on close rather than viewport destruction so `eframe` never tears down an active WGL surface.
pub fn render_inspector_viewport(
    ui: &mut Ui,
    shared: &Arc<Mutex<SharedFenceState>>,
) -> FenceUiOutput {
    let ctx = ui.ctx().clone();
    let mut out = FenceUiOutput::default();

    if ctx.input(|i| i.viewport().close_requested()) {
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        close_inspector_safely(shared, &ctx, &mut out);
        return out;
    }

    let (insp_opt, all_fences, monitors, prefs_snap, hide_all_snap) = {
        let mut guard = lock_unpoisoned(shared);
        if let Some(ref mut ins) = guard.inspector {
            if ins.hwnd == 0
                && let Some(h) = find_window_by_exact_title(INSPECTOR_TITLE)
            {
                ins.hwnd = h;
            }
            if ins.hwnd != 0 && !ins.styled {
                set_window_cloaked(ins.hwnd, true);
                style_floating_inspector_window(ins.hwnd);
                ins.styled = true;
                ins.reveal_frames = ins.reveal_frames.max(1);
                ins.needs_uncloak = true;
            }
        }
        (
            guard.inspector.clone(),
            guard
                .config
                .fences
                .iter()
                .map(|f| (f.id, format_fence_title(f)))
                .collect::<Vec<_>>(),
            guard.topology.monitors().to_vec(),
            guard.config.preferences.clone(),
            guard.config.hide_all_fences,
        )
    };

    let Some(insp) = insp_opt else {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        return out;
    };
    if !insp.open {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        return out;
    }

    let mut target_id = insp.target_fence_id;
    let (target_spec_opt, target_hwnd, overridden_names, autostart_snap, autostart_busy, autostart_fb) = {
        let mut guard = lock_unpoisoned(shared);
        let mut found = guard
            .config
            .fences
            .iter()
            .find(|f| f.id == target_id)
            .cloned();
        if found.is_none()
            && let Some(first) = guard.config.fences.first().cloned()
        {
            target_id = first.id;
            if let Some(ref mut ins) = guard.inspector {
                ins.target_fence_id = first.id;
                ins.draft_name = Some((first.id, first.name.clone()));
                ins.name_edit_initial = Some((first.id, first.name.clone()));
            }
            found = Some(first);
        }
        let overridden_names = guard
            .config
            .fences
            .iter()
            .filter(|f| f.layout_override.as_ref().is_some_and(|o| !o.is_empty()))
            .map(format_fence_title)
            .collect::<Vec<_>>();
        let hwnd = found
            .as_ref()
            .and_then(|f| guard.attached_hwnds.get(&f.id).copied())
            .unwrap_or(0);
        let fb = guard.autostart_feedback.clone().and_then(|(msg, is_err, at)| {
            if at.elapsed().as_secs_f32() < 6.0 {
                Some((msg, is_err))
            } else {
                None
            }
        });
        (
            found,
            hwnd,
            overridden_names,
            guard.autostart_status.clone(),
            guard.autostart_busy,
            fb,
        )
    };

    if let Some((gx, gy)) = insp.drag_grab {
        if desklite_win32::is_primary_mouse_down() {
            if let Some((cx, cy)) = desklite_win32::cursor_screen_pos()
                && let Some((wl, wt, wr, wb)) = desklite_win32::window_outer_rect(insp.hwnd)
            {
                let (nx, ny) = clamp_drag_pos_to_monitors(
                    cx - gx,
                    cy - gy,
                    (wr - wl).max(200),
                    (wb - wt).max(200),
                    (cx, cy),
                    &monitors,
                );
                desklite_win32::move_window_no_z(insp.hwnd, nx, ny);
            }
            ctx.request_repaint();
        } else if let Some(ref mut ins) = lock_unpoisoned(shared).inspector {
            ins.drag_grab = None;
        }
    }

    let live_rect = target_spec_opt.as_ref().map(|spec| {
        if let Some((l, t, r, b)) = live_sizemove_rect_for_hwnd(target_hwnd) {
            ctx.request_repaint();
            if spec.collapsed {
                Rect::new(l, t, r, t + spec.rect.height().max(MIN_FENCE_HEIGHT))
            } else {
                Rect::new(l, t, r, b)
            }
        } else if target_hwnd != 0
            && desklite_win32::is_in_size_move()
            && let Some((l, t, r, b)) = desklite_win32::window_outer_rect(target_hwnd)
        {
            ctx.request_repaint();
            if spec.collapsed {
                Rect::new(l, t, r, t + spec.rect.height().max(MIN_FENCE_HEIGHT))
            } else {
                Rect::new(l, t, r, b)
            }
        } else {
            spec.rect
        }
    });

    let mut close_inspector =
        ctx.input(|i| i.key_pressed(Key::Escape)) && !ctx.egui_wants_keyboard_input();

    let panel_frame = Frame::new()
        .fill(Color32::from_rgba_unmultiplied(24, 27, 34, 242))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 24),
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(0));

    egui::CentralPanel::default().frame(panel_frame).show(ui, |ui| {
        let header_frame = Frame::new()
            .fill(Color32::from_rgba_unmultiplied(32, 36, 46, 225))
            .corner_radius(CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            })
            .inner_margin(Margin {
                left: 12,
                right: 10,
                top: 7,
                bottom: 7,
            });
        header_frame.show(ui, |ui| {
            ui.set_height(24.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if draw_header_icon_btn(ui, "关闭设置 (Esc)", |painter, r, fg| {
                    let c = r.center();
                    let s = Stroke::new(1.5, fg);
                    painter.line_segment(
                        [Pos2::new(c.x - 4.0, c.y - 4.0), Pos2::new(c.x + 4.0, c.y + 4.0)],
                        s,
                    );
                    painter.line_segment(
                        [Pos2::new(c.x + 4.0, c.y - 4.0), Pos2::new(c.x - 4.0, c.y + 4.0)],
                        s,
                    );
                })
                .clicked()
                {
                    close_inspector = true;
                }

                if all_fences.len() > 1
                    && let Some(ref spec) = target_spec_opt
                {
                    let cur_label = format_fence_title(spec);
                    let mut picked = target_id;
                    egui::ComboBox::from_id_salt("insp_target_fence")
                        .width(156.0)
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            for (fid, fname) in &all_fences {
                                ui.selectable_value(&mut picked, *fid, fname);
                            }
                        });
                    if picked != target_id {
                        let mut guard = lock_unpoisoned(shared);
                        let next_name = guard
                            .config
                            .fences
                            .iter()
                            .find(|f| f.id == picked)
                            .map(|f| f.name.clone())
                            .unwrap_or_default();
                        if let Some(ref mut ins) = guard.inspector {
                            ins.target_fence_id = picked;
                            ins.draft_name = Some((picked, next_name.clone()));
                            ins.name_edit_initial = Some((picked, next_name));
                        }
                        if guard
                            .config
                            .fences
                            .iter()
                            .find(|f| f.id == picked)
                            .is_some_and(|f| f.collapsed)
                        {
                            guard.pending_toggle_collapse.insert(picked);
                            guard.request_repaint_fence(&ctx, picked);
                        }
                    }
                }

                let rem_w = ui.available_width().max(40.0);
                let (h_rect, _) = ui.allocate_exact_size(Vec2::new(rem_w, 24.0), Sense::hover());
                let drag_resp = ui.interact(
                    h_rect,
                    ui.make_persistent_id("insp_header_drag"),
                    Sense::click_and_drag(),
                );
                ui.painter().text(
                    Pos2::new(h_rect.min.x + 2.0, h_rect.center().y - 0.5),
                    egui::Align2::LEFT_CENTER,
                    "参数设置",
                    egui::FontId::proportional(13.5),
                    Color32::from_rgb(242, 246, 255),
                );
                if drag_resp.drag_started_by(PointerButton::Primary)
                    && let Some((cx, cy)) = desklite_win32::cursor_screen_pos()
                    && let Some((wl, wt, _, _)) = desklite_win32::window_outer_rect(insp.hwnd)
                    && let Some(ref mut ins) = lock_unpoisoned(shared).inspector
                {
                    ins.drag_grab = Some((cx - wl, cy - wt));
                    ctx.request_repaint();
                }
            });
        });

        let body_h = (ui.available_height() - 46.0).max(120.0);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), body_h),
            Layout::top_down(Align::Min),
            |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Frame::new().inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;

                            let mut name_buf = String::new();
                            let mut next_kind = FenceKind::IconGrid;
                            let mut alpha = 180u8;
                            let mut locked = false;
                            let mut rx = 0i32;
                            let mut ry = 0i32;
                            let mut rw = MIN_FENCE_WIDTH;
                            let mut rh = MIN_FENCE_HEIGHT;
                            let mut ov = desklite_core::FenceLayoutOverride::default();

                            let mut name_changed = false;
                            let mut name_committed = false;
                            let mut kind_changed = false;
                            let mut alpha_changed = false;
                            let mut lock_changed = false;
                            let mut rect_changed = false;
                            let mut single_changed = false;
                            let mut single_reset = false;

                            if let (Some(spec), Some(lrect)) = (target_spec_opt.as_ref(), live_rect) {
                                name_buf = insp
                                    .draft_name
                                    .as_ref()
                                    .filter(|(fid, _)| *fid == target_id)
                                    .map(|(_, s)| s.clone())
                                    .unwrap_or_else(|| spec.name.clone());
                                next_kind = spec.kind;
                                alpha = spec.backdrop_alpha.clamp(40, 245);
                                locked = spec.locked;
                                rx = lrect.left;
                                ry = lrect.top;
                                rw = lrect.width().max(MIN_FENCE_WIDTH);
                                rh = lrect.height().max(MIN_FENCE_HEIGHT);

                                inspector_card(ui, "当前格子外观与位置", |ui| {
                                    ui.spacing_mut().item_spacing.y = 7.0;
                                    labeled_row(ui, "名称", |ui| {
                                        let w = ui.available_width().max(120.0);
                                        let name_resp = ui.add(
                                            egui::TextEdit::singleline(&mut name_buf).desired_width(w),
                                        );
                                        if name_resp.changed() {
                                            name_changed = true;
                                        }
                                        if name_resp.lost_focus()
                                            || (name_resp.has_focus()
                                                && ui.input(|i| i.key_pressed(Key::Enter)))
                                        {
                                            name_committed = true;
                                        }
                                    });

                                    labeled_row(ui, "视图模式", |ui| {
                                        ui.columns(2, |cols| {
                                            let is_list = matches!(next_kind, FenceKind::FileList);
                                            if cols[0]
                                                .add_sized(
                                                    [cols[0].available_width(), 24.0],
                                                    egui::Button::selectable(!is_list, "图标网格"),
                                                )
                                                .clicked()
                                                && is_list
                                            {
                                                next_kind = FenceKind::IconGrid;
                                                kind_changed = true;
                                            }
                                            if cols[1]
                                                .add_sized(
                                                    [cols[1].available_width(), 24.0],
                                                    egui::Button::selectable(is_list, "紧凑列表"),
                                                )
                                                .clicked()
                                                && !is_list
                                            {
                                                next_kind = FenceKind::FileList;
                                                kind_changed = true;
                                            }
                                        });
                                    });

                                    labeled_row(ui, "背景不透明", |ui| {
                                        let pct = ((alpha as f32 / 255.0) * 100.0).round() as i32;
                                        ui.spacing_mut().slider_width =
                                            (ui.available_width() - 56.0).max(80.0);
                                        if ui
                                            .add(egui::Slider::new(&mut alpha, 40..=245).show_value(false))
                                            .changed()
                                        {
                                            alpha_changed = true;
                                        }
                                        ui.label(
                                            egui::RichText::new(format!("{pct}%"))
                                                .size(11.5)
                                                .color(Color32::from_rgb(176, 192, 218)),
                                        );
                                    });

                                    labeled_row(ui, "位置 / 尺寸", |ui| {
                                        ui.add_enabled_ui(!locked, |ui| {
                                            ui.spacing_mut().item_spacing.x = 6.0;
                                            let mut add_coord_drag = |dv: egui::DragValue<'_>| {
                                                let resp = ui.add(dv);
                                                if resp.hovered() && !resp.dragged() {
                                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                                                }
                                                if resp.changed() {
                                                    rect_changed = true;
                                                }
                                            };
                                            add_coord_drag(
                                                egui::DragValue::new(&mut rx)
                                                    .speed(2.0)
                                                    .prefix("X "),
                                            );
                                            add_coord_drag(
                                                egui::DragValue::new(&mut ry)
                                                    .speed(2.0)
                                                    .prefix("Y "),
                                            );
                                            add_coord_drag(
                                                egui::DragValue::new(&mut rw)
                                                    .range(MIN_FENCE_WIDTH..=3840)
                                                    .speed(2.0)
                                                    .prefix("宽 "),
                                            );
                                            add_coord_drag(
                                                egui::DragValue::new(&mut rh)
                                                    .range(MIN_FENCE_HEIGHT..=2160)
                                                    .speed(2.0)
                                                    .prefix("高 "),
                                            );
                                        });
                                    });

                                    labeled_row(ui, "锁定防误触", |ui| {
                                        if ui.checkbox(&mut locked, "锁定位置与尺寸").changed() {
                                            lock_changed = true;
                                        }
                                    });
                                });

                                let is_list_mode = matches!(next_kind, FenceKind::FileList);
                                ov = spec.layout_override.clone().unwrap_or_default();
                                let (_, _, _, _) = resolve_fence_layout(spec, &prefs_snap);
                                let mut s_cell_w =
                                    ov.icon_cell_w.unwrap_or(prefs_snap.icon_cell_w).clamp(64, 160);
                                let mut s_cell_h =
                                    ov.icon_cell_h.unwrap_or(prefs_snap.icon_cell_h).clamp(72, 180);
                                let mut s_row_h =
                                    ov.list_row_h.unwrap_or(prefs_snap.list_row_h).clamp(24, 56);
                                let mut s_list_icon =
                                    ov.list_icon_px.unwrap_or(prefs_snap.list_icon_px).clamp(14, 36);

                                let has_mode_override = if is_list_mode {
                                    ov.list_row_h.is_some() || ov.list_icon_px.is_some()
                                } else {
                                    ov.icon_cell_w.is_some() || ov.icon_cell_h.is_some()
                                };
                                let has_any_override = !ov.is_empty();
                                let single_title = if is_list_mode {
                                    "单格独立覆写 · 紧凑列表"
                                } else {
                                    "单格独立覆写 · 图标网格"
                                };
                                inspector_card_with_header(
                                    ui,
                                    single_title,
                                    |ui| {
                                        if has_mode_override {
                                            if ui.small_button("恢复跟随").clicked() {
                                                single_reset = true;
                                            }
                                            ui.label(
                                                egui::RichText::new("已独立")
                                                    .size(11.0)
                                                    .color(Color32::from_rgb(245, 194, 107)),
                                            );
                                        } else if has_any_override {
                                            if ui.small_button("恢复跟随").clicked() {
                                                single_reset = true;
                                            }
                                            ui.label(
                                                egui::RichText::new("他模式已独立")
                                                    .size(11.0)
                                                    .color(Color32::from_rgb(245, 194, 107)),
                                            );
                                        } else {
                                            ui.label(
                                                egui::RichText::new("跟随全局中")
                                                    .size(11.0)
                                                    .color(Color32::from_rgb(138, 152, 176)),
                                            );
                                        }
                                    },
                                    |ui| {
                                        ui.spacing_mut().item_spacing.y = 7.0;
                                        if is_list_mode {
                                            labeled_row(ui, "列表行高", |ui| {
                                                if px_slider(ui, &mut s_row_h, 24..=56) {
                                                    ov.list_row_h = Some(s_row_h);
                                                    single_changed = true;
                                                }
                                            });
                                            labeled_row(ui, "列表图标", |ui| {
                                                if px_slider(ui, &mut s_list_icon, 14..=36) {
                                                    ov.list_icon_px = Some(s_list_icon);
                                                    single_changed = true;
                                                }
                                            });
                                        } else {
                                            labeled_row(ui, "网格格宽", |ui| {
                                                if px_slider(ui, &mut s_cell_w, 64..=160) {
                                                    ov.icon_cell_w = Some(s_cell_w);
                                                    single_changed = true;
                                                }
                                            });
                                            labeled_row(ui, "网格格高", |ui| {
                                                if px_slider(ui, &mut s_cell_h, 72..=180) {
                                                    ov.icon_cell_h = Some(s_cell_h);
                                                    single_changed = true;
                                                }
                                            });
                                        }
                                    },
                                );
                            }

                            let mut g_cell_w = prefs_snap.icon_cell_w.clamp(64, 160);
                            let mut g_cell_h = prefs_snap.icon_cell_h.clamp(72, 180);
                            let mut g_row_h = prefs_snap.list_row_h.clamp(24, 56);
                            let mut g_list_icon = prefs_snap.list_icon_px.clamp(14, 36);
                            let mut snap_px = prefs_snap.snap_threshold_px.clamp(0, 40);
                            let mut hide_all = hide_all_snap;
                            let mut prefs_changed = false;
                            let mut free_cell_prefs_changed = false;
                            let mut hide_all_changed = false;
                            let mut sync_all_to_global = false;

                            inspector_card_with_header(
                                ui,
                                "全局默认排版与桌面设置",
                                |ui| {
                                    if !overridden_names.is_empty() {
                                        if ui.small_button("全部跟随全局").clicked() {
                                            sync_all_to_global = true;
                                        }
                                        let badge = ui.label(
                                            egui::RichText::new(format!(
                                                "{} 格独立",
                                                overridden_names.len()
                                            ))
                                            .size(11.0)
                                            .color(Color32::from_rgb(245, 194, 107)),
                                        );
                                        badge.on_hover_text(format!(
                                            "以下格子已启用单格独立覆写（不随全局变化）：\n{}",
                                            overridden_names.join("、")
                                        ));
                                    } else {
                                        ui.label(
                                            egui::RichText::new("全部已跟随")
                                                .size(11.0)
                                                .color(Color32::from_rgb(138, 152, 176)),
                                        );
                                    }
                                },
                                |ui| {
                                    ui.spacing_mut().item_spacing.y = 7.0;
                                    labeled_row(ui, "网格格宽", |ui| {
                                        if px_slider(ui, &mut g_cell_w, 64..=160) {
                                            prefs_changed = true;
                                            free_cell_prefs_changed = true;
                                        }
                                    });
                                    labeled_row(ui, "网格格高", |ui| {
                                        if px_slider(ui, &mut g_cell_h, 72..=180) {
                                            prefs_changed = true;
                                            free_cell_prefs_changed = true;
                                        }
                                    });
                                    labeled_row(ui, "列表行高", |ui| {
                                        if px_slider(ui, &mut g_row_h, 24..=56) {
                                            prefs_changed = true;
                                        }
                                    });
                                    labeled_row(ui, "列表图标", |ui| {
                                        if px_slider(ui, &mut g_list_icon, 14..=36) {
                                            prefs_changed = true;
                                        }
                                    });
                                    labeled_row(ui, "边缘吸附", |ui| {
                                        if px_slider(ui, &mut snap_px, 0..=40) {
                                            prefs_changed = true;
                                        }
                                    });
                                    labeled_row(ui, "桌面显隐", |ui| {
                                        if ui
                                            .checkbox(&mut hide_all, "隐藏全部格子（双击桌面切换）")
                                            .changed()
                                        {
                                            hide_all_changed = true;
                                        }
                                    });
                                },
                            );

                            let cur_mode = autostart_snap
                                .as_ref()
                                .map(|s| s.mode)
                                .unwrap_or(AutostartMode::Disabled);
                            let path_mismatch =
                                autostart_snap.as_ref().is_some_and(|s| s.path_mismatch);
                            let mut pending_autostart: Option<AutostartMode> = None;

                            inspector_card_with_header(
                                ui,
                                "开机自启动与启动优先级",
                                |ui| {
                                    let (badge_text, badge_color) = if autostart_busy {
                                        ("处理中...", Color32::from_rgb(138, 178, 255))
                                    } else {
                                        match autostart_snap.as_ref().map(|s| s.mode) {
                                            None => ("检测中...", Color32::from_rgb(138, 152, 176)),
                                            Some(AutostartMode::Disabled) => {
                                                ("未启用", Color32::from_rgb(138, 152, 176))
                                            }
                                            Some(AutostartMode::Normal) => {
                                                ("普通自启动", Color32::from_rgb(120, 178, 255))
                                            }
                                            Some(AutostartMode::HighPriority) => {
                                                ("高优先级已启用", Color32::from_rgb(104, 211, 145))
                                            }
                                        }
                                    };
                                    ui.label(
                                        egui::RichText::new(badge_text)
                                            .size(11.0)
                                            .color(badge_color),
                                    );
                                },
                                |ui| {
                                    ui.spacing_mut().item_spacing.y = 7.0;
                                    ui.add_enabled_ui(!autostart_busy && autostart_snap.is_some(), |ui| {
                                        ui.columns(3, |cols| {
                                            let off_selected = cur_mode == AutostartMode::Disabled;
                                            if cols[0]
                                                .add_sized(
                                                    [cols[0].available_width(), 26.0],
                                                    egui::Button::selectable(off_selected, "关闭"),
                                                )
                                                .clicked()
                                                && !off_selected
                                            {
                                                pending_autostart = Some(AutostartMode::Disabled);
                                            }

                                            let norm_selected = cur_mode == AutostartMode::Normal;
                                            if cols[1]
                                                .add_sized(
                                                    [cols[1].available_width(), 26.0],
                                                    egui::Button::selectable(norm_selected, "普通自启动"),
                                                )
                                                .clicked()
                                                && !norm_selected
                                            {
                                                pending_autostart = Some(AutostartMode::Normal);
                                            }

                                            let high_selected =
                                                cur_mode == AutostartMode::HighPriority;
                                            let high_fill = if high_selected {
                                                Color32::from_rgb(46, 138, 87)
                                            } else {
                                                Color32::from_rgba_unmultiplied(54, 118, 82, 120)
                                            };
                                            let high_btn = egui::Button::new(
                                                egui::RichText::new("   高优先级启动")
                                                    .size(12.0)
                                                    .color(Color32::WHITE),
                                            )
                                            .fill(high_fill)
                                            .stroke(if high_selected {
                                                Stroke::new(1.0, Color32::from_rgb(110, 231, 160))
                                            } else {
                                                Stroke::new(
                                                    1.0,
                                                    Color32::from_rgba_unmultiplied(110, 231, 160, 80),
                                                )
                                            });
                                            let high_resp = cols[2].add_sized(
                                                [cols[2].available_width(), 26.0],
                                                high_btn,
                                            );
                                            paint_uac_shield(
                                                cols[2].painter(),
                                                Pos2::new(
                                                    high_resp.rect.left() + 12.0,
                                                    high_resp.rect.center().y,
                                                ),
                                            );
                                            if high_resp.clicked() && !high_selected {
                                                pending_autostart =
                                                    Some(AutostartMode::HighPriority);
                                            }
                                        });
                                    });

                                    ui.label(
                                        egui::RichText::new(
                                            "普通模式随资源管理器延迟加载（免授权）；高优先级通过 Windows 任务计划在登录时立即高优先拉起（参考 Wallpaper Engine，需一次 UAC 授权，且不影响桌面拖拽）。",
                                        )
                                        .size(11.0)
                                        .color(Color32::from_rgb(152, 165, 188)),
                                    );

                                    if path_mismatch && !autostart_busy {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new("检测到当前程序位置与已注册启动路径不一致")
                                                    .size(11.0)
                                                    .color(Color32::from_rgb(245, 194, 107)),
                                            );
                                            if ui.small_button("更新启动路径").clicked() {
                                                pending_autostart = Some(cur_mode);
                                            }
                                        });
                                    }

                                    if let Some((fb_msg, is_err)) = &autostart_fb {
                                        let fb_color = if *is_err {
                                            Color32::from_rgb(248, 113, 113)
                                        } else {
                                            Color32::from_rgb(134, 239, 172)
                                        };
                                        ui.label(
                                            egui::RichText::new(fb_msg)
                                                .size(11.0)
                                                .color(fb_color),
                                        );
                                    }
                                },
                            );

                            if let Some(next_mode) = pending_autostart {
                                apply_autostart_mode_async(next_mode, shared, &ctx);
                            }

                            if single_reset {
                                ov = desklite_core::FenceLayoutOverride::default();
                                single_changed = true;
                            }

                            if name_changed
                                || name_committed
                                || kind_changed
                                || alpha_changed
                                || lock_changed
                                || single_changed
                                || prefs_changed
                                || hide_all_changed
                                || sync_all_to_global
                            {
                                let mut guard = lock_unpoisoned(shared);
                                let old_baseline = guard
                                    .inspector
                                    .as_ref()
                                    .and_then(|ins| ins.name_edit_initial.as_ref())
                                    .filter(|(fid, _)| *fid == target_id)
                                    .map(|(_, s)| s.clone());

                                if let Some(ref mut ins) = guard.inspector {
                                    if name_committed {
                                        let trimmed = name_buf.trim().to_string();
                                        ins.draft_name = Some((target_id, trimmed.clone()));
                                        if !trimmed.is_empty() {
                                            ins.name_edit_initial =
                                                Some((target_id, trimmed.clone()));
                                        }
                                        name_buf = trimmed;
                                    } else if name_changed {
                                        ins.draft_name = Some((target_id, name_buf.clone()));
                                    }
                                }
                                let mut push_rename_undo = None;
                                if let Some(f) = guard.config.fence_mut(target_id) {
                                    if name_changed || name_committed {
                                        let valid = if name_committed {
                                            name_buf.trim().to_string()
                                        } else {
                                            name_buf.clone()
                                        };
                                        if !valid.trim().is_empty() {
                                            if name_committed
                                                && let Some(prev) = old_baseline
                                                && prev != valid
                                            {
                                                push_rename_undo = Some(UndoAction::RenameFence {
                                                    fence_id: target_id,
                                                    old_name: prev,
                                                });
                                            }
                                            f.name = valid;
                                        }
                                    }
                                    if kind_changed {
                                        f.kind = next_kind;
                                    }
                                    if alpha_changed {
                                        f.backdrop_alpha = alpha;
                                    }
                                    if lock_changed {
                                        f.locked = locked;
                                        if target_hwnd != 0 {
                                            set_hwnd_locked(target_hwnd, locked);
                                        }
                                    }
                                    if single_changed && !sync_all_to_global {
                                        f.layout_override = if ov.is_empty() { None } else { Some(ov) };
                                    }
                                }
                                if kind_changed {
                                    guard.view_mode_anims.insert(
                                        target_id,
                                        ViewModeAnimState {
                                            started_at: Instant::now(),
                                            duration_ms: 170,
                                            to_list: matches!(next_kind, FenceKind::FileList),
                                        },
                                    );
                                }
                                if sync_all_to_global {
                                    for f in &mut guard.config.fences {
                                        f.layout_override = None;
                                    }
                                }
                                if let Some(act) = push_rename_undo {
                                    guard.push_undo(act);
                                }
                                if prefs_changed {
                                    guard.config.preferences.icon_cell_w = g_cell_w;
                                    guard.config.preferences.icon_cell_h = g_cell_h;
                                    guard.config.preferences.list_row_h = g_row_h;
                                    guard.config.preferences.list_icon_px = g_list_icon;
                                    guard.config.preferences.snap_threshold_px = snap_px;
                                    if free_cell_prefs_changed {
                                        let new_free_cell = guard.free_desktop_cell_size();
                                        guard.clamp_free_icons_for_cell_size(new_free_cell);
                                    }
                                }
                                if hide_all_changed {
                                    guard.config.hide_all_fences = hide_all;
                                }
                                guard.mark_debounced_save(&ctx);
                                if free_cell_prefs_changed || hide_all_changed {
                                    guard.mark_free_desktop_dirty(Some(&ctx));
                                }
                            }
                            if prefs_changed || hide_all_changed || sync_all_to_global {
                                request_repaint_all_fences(shared, &ctx);
                                ctx.request_repaint_of(ViewportId::ROOT);
                            } else if name_changed
                                || name_committed
                                || kind_changed
                                || alpha_changed
                                || lock_changed
                                || single_changed
                            {
                                lock_unpoisoned(shared).request_repaint_fence(&ctx, target_id);
                            }
                            if rect_changed && !locked {
                                let _ = apply_param_editor_rect(
                                    target_id,
                                    target_hwnd,
                                    rx,
                                    ry,
                                    rw,
                                    rh,
                                    shared,
                                    &ctx,
                                    &mut out,
                                );
                            }
                        });
                    });
            },
        );

        Frame::new()
            .fill(Color32::from_rgba_unmultiplied(28, 32, 42, 235))
            .corner_radius(CornerRadius {
                nw: 0,
                ne: 0,
                sw: 8,
                se: 8,
            })
            .inner_margin(Margin::symmetric(12, 9))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if target_spec_opt.is_some()
                        && ui.button("复制同样式格子").clicked()
                        && duplicate_fence_with_style(target_id, shared, &ctx).is_some()
                    {
                        lock_unpoisoned(shared).toast_msg.insert(
                            target_id,
                            ("已复制样式格子".to_string(), Instant::now()),
                        );
                        out.needs_immediate_save = true;
                        lock_unpoisoned(shared).request_repaint_fence(&ctx, target_id);
                        ctx.request_repaint_of(ViewportId::ROOT);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let done_btn = egui::Button::new(
                            egui::RichText::new("完成")
                                .size(12.5)
                                .color(Color32::WHITE),
                        )
                        .fill(Color32::from_rgb(56, 130, 246))
                        .min_size(Vec2::new(72.0, 26.0));
                        if ui.add(done_btn).clicked() {
                            close_inspector = true;
                        }
                    });
                });
            });
    });

    if close_inspector {
        close_inspector_safely(shared, &ctx, &mut out);
    } else if insp.hwnd != 0 {
        if insp.reveal_frames > 0 {
            if let Some(ref mut ins) = lock_unpoisoned(shared).inspector {
                ins.reveal_frames = ins.reveal_frames.saturating_sub(1);
            }
            ctx.request_repaint();
        } else if insp.needs_uncloak {
            desklite_win32::move_window_no_z(insp.hwnd, insp.initial_pos.0, insp.initial_pos.1);
            post_uncloak_after_swap(insp.hwnd);
            if let Some(ref mut ins) = lock_unpoisoned(shared).inspector {
                ins.needs_uncloak = false;
            }
            ctx.request_repaint_of(ViewportId::ROOT);
        }
    }

    out
}

fn paint_uac_shield(painter: &egui::Painter, center: Pos2) {
    let w = 4.5;
    let h = 5.0;
    let c_blue = Color32::from_rgb(59, 130, 246);
    let c_gold = Color32::from_rgb(250, 204, 21);
    painter.rect_filled(
        egui::Rect::from_min_max(
            Pos2::new(center.x - w, center.y - h),
            Pos2::new(center.x, center.y),
        ),
        0.0,
        c_blue,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            Pos2::new(center.x, center.y - h),
            Pos2::new(center.x + w, center.y),
        ),
        0.0,
        c_gold,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            Pos2::new(center.x - w, center.y),
            Pos2::new(center.x, center.y + h - 1.0),
        ),
        0.0,
        c_gold,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            Pos2::new(center.x, center.y),
            Pos2::new(center.x + w, center.y + h - 1.0),
        ),
        0.0,
        c_blue,
    );
    painter.rect_stroke(
        egui::Rect::from_min_max(
            Pos2::new(center.x - w, center.y - h),
            Pos2::new(center.x + w, center.y + h - 1.0),
        ),
        1.0,
        Stroke::new(0.8, Color32::from_rgba_unmultiplied(15, 23, 42, 200)),
        egui::StrokeKind::Outside,
    );
}

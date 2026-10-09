//! Deferred work recorded before paint — completion scans, file previews and
//! statuses, diffs, task tails, image thumbnails, the clipboard copy, git
//! detail/next-page wants — spawned off the frame path (§8.1), plus the
//! "is this view on screen" predicates that gate it.

use pacode_client::Client;
use tokio::sync::mpsc;

use super::bg_response::BgResponse;
use crate::keys::Action;
use crate::state::{AppState, PanelTarget};

/// Ceiling on the file an inline thumbnail decodes: a kitty/sixel escape
/// carries the encoded image, so an unbounded paste would put megabytes into
/// the frame path's per-frame clone. Over the cap the slot keeps its
/// placeholder; the prompt still attaches the original file to the message.
pub(super) const IMAGE_THUMB_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Service the work items recorded before paint: completion scans, image
/// preview/thumbnail renders, the stashed clipboard copy. Everything
/// lands on `spawn_blocking` — the frame path itself stays clean (§8.1).
/// Daemon-bound wants return as actions for `dispatch_action` instead — they
/// share the request's bookkeeping (`begin_log`, `mark_sent`).
pub(super) fn dispatch_deferred(
    state: &mut AppState,
    bg_tx: &mpsc::UnboundedSender<BgResponse>,
    client: &Client,
) -> Vec<Action> {
    if state.awaiting_snapshot {
        return Vec::new();
    }
    if let Some(key) = state.input.at_comp.claim_want() {
        let context = state.session_context();
        let client_context = client.event_context();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let candidates = crate::at_complete::complete_at_path(&key.query, &key.cwd);
            let _ = tx.send(BgResponse::AtCompleted {
                context,
                client_context,
                key,
                candidates,
            });
        });
    }

    if let Some(key) = state.input.bash_comp.claim_want() {
        let context = state.session_context();
        let client_context = client.event_context();
        let history = state.input.bash_history.clone();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let candidates =
                crate::bash::complete::complete(&key.text, key.cursor, &history, &key.cwd);
            let _ = tx.send(BgResponse::BashCompletionDone {
                context,
                client_context,
                key,
                candidates,
            });
        });
    }

    // Files preview: resolve + read/lex off-thread for the menu detail card
    // (§8.1 — the draw only records `preview_want`).
    if let Some(want) = state.files.preview_want.clone()
        && state.files.preview_in_flight.as_ref() != Some(&want)
        && state.files.cached_preview.as_ref().map(|c| &c.key) != Some(&want)
    {
        state.files.preview_in_flight = Some(want.clone());
        let context = state.session_context();
        let client_context = client.event_context();
        let cwd = state.cwd();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let resolved = resolve_preview_path(&want.raw, &cwd);
            let text = crate::ui::menu::files::text_preview(&resolved);
            let _ = tx.send(BgResponse::FilePreviewReady {
                context,
                client_context,
                key: want,
                resolved,
                text,
            });
        });
    }

    // Files level: refresh the `git status` labels once the row set changed
    // and no scan runs. `at_version` rides along so the reply marks which
    // `rows_version` it described.
    if files_view_on_screen(state)
        && state.files.statused_at != Some(state.files.rows_version)
        && !state.files.statuses_in_flight
    {
        state.files.statuses_in_flight = true;
        let context = state.session_context();
        let client_context = client.event_context();
        let at_version = state.files.rows_version;
        let cwd = state.cwd();
        let paths: Vec<String> = state.files.rows.iter().map(|r| r.path.clone()).collect();
        let tx = bg_tx.clone();
        tokio::spawn(async move {
            let statuses = pacode_vcs::worktree_statuses(&cwd, &paths)
                .await
                .unwrap_or_default();
            let _ = tx.send(BgResponse::FileStatusesReady {
                context,
                client_context,
                statuses,
                at_version,
            });
        });
    }

    // Files level `enter`: the worktree diff for the panel. The want dies
    // when the panel's target moved on; an in-flight probe defers a re-arm.
    if let Some(path) = state.files.diff_want.clone() {
        let still_target = matches!(
            &state.panel.target,
            Some(PanelTarget::FileDiff { path: p }) if *p == path
        );
        if !still_target {
            state.files.diff_want = None;
        } else if state.files.diff_in_flight.is_none() {
            state.files.diff_want = None;
            state.files.diff_in_flight = Some(path.clone());
            let context = state.session_context();
            let client_context = client.event_context();
            let cwd = state.cwd();
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                let diff = pacode_vcs::file_diff(&cwd, &path).await;
                let _ = tx.send(BgResponse::FileDiffReady {
                    context,
                    client_context,
                    path,
                    diff,
                });
            });
        }
    }

    // Background level: the detail card's output tail — a blocking read of
    // the selected task's spool file (§5.9). The want is keyed by (task,
    // `output_bytes`): a `TaskUpdated` carrying more output re-keys it and
    // the read re-arms by itself — the daemon's task events drive the
    // refresh, no timer and no event-side invalidation.
    if let Some(want) = state.rail.task_tail_want.clone()
        && state.rail.task_tail_in_flight.as_ref() != Some(&want)
        && state.rail.task_tail.as_ref().map(|t| t.key()).as_ref() != Some(&want)
    {
        // A task that vanished between draw and dispatch has no file to
        // read — the want dies with the next draw's re-record.
        if let Some(path) = state.rail.task(&want.task).map(|t| t.output_path.clone()) {
            state.rail.task_tail_in_flight = Some(want.clone());
            let context = state.session_context();
            let client_context = client.event_context();
            let tx = bg_tx.clone();
            tokio::task::spawn_blocking(move || {
                let (text, truncated) = crate::ui::menu::background::read_tail(&path);
                let _ = tx.send(BgResponse::TaskTailReady {
                    context,
                    client_context,
                    tail: crate::state::rail::TaskTail {
                        task: want.task,
                        output_bytes: want.output_bytes,
                        text,
                        truncated,
                    },
                });
            });
        }
    }

    // Pasted-image strip thumbnails: one job per missing/mismatched slot.
    let context = state.session_context();
    let client_context = client.event_context();
    let render_wants = std::mem::take(&mut state.image_strip.want);
    let mut pending_wants = Vec::new();
    for want in render_wants {
        let k = (want.0, want.2, want.3);
        let already = state
            .image_strip
            .thumbs
            .get(&want.0)
            .is_some_and(|thumb| thumb.cell_w == want.2 && thumb.cell_h == want.3);
        if already || state.image_strip.in_flight.contains(&k) {
            continue;
        }
        if !state.image_strip.can_start_render() {
            if pending_wants.len() < crate::state::images::IMAGE_RENDER_MAX_WANTS {
                pending_wants.push(want);
            }
            continue;
        }
        let (token, path, cell_w, cell_h) = want;
        state.image_strip.in_flight.insert(k);
        let job_context = context.clone();
        let job_client_context = client_context.clone();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            let preview = render_thumbnail(&path, cell_w, cell_h);
            let _ = tx.send(BgResponse::ThumbReady {
                context: job_context,
                client_context: job_client_context,
                token,
                cell_w,
                cell_h,
                preview,
            });
        });
    }
    state.image_strip.want = pending_wants;

    // The selection copy prepared before paint — OSC 52 + arboard, off-thread.
    if let Some(mut copy) = state.pending_copy.take() {
        let original_chars = copy.text.chars().count();
        if original_chars > crate::ui::COPY_MAX_CHARS {
            copy.text = copy.text.chars().take(crate::ui::COPY_MAX_CHARS).collect();
            log::warn!(
                "clipboard copy truncated from {original_chars} to {} chars",
                crate::ui::COPY_MAX_CHARS
            );
        }
        let context = state.session_context();
        let client_context = client.event_context();
        let tx = bg_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = crate::clipboard::copy(&copy.text) {
                log::warn!("clipboard copy failed: {e}");
            }
            let _ = tx.send(BgResponse::CopyDone {
                context,
                client_context,
                auto: copy.auto,
                chars: copy.text.chars().count(),
            });
        });
    }

    // Git level (§5.10): the draw records what the cursor wants — the
    // selected commit's `--stat` for the detail pane, and the next history
    // page when the cursor sits on the last loaded row. Both go out as
    // actions so `Action::GitLog`'s `begin_log`/`mark_sent` bookkeeping
    // applies. A stale want (menu closed, another level on top) dies here
    // rather than firing a fetch nobody sees.
    let mut actions = Vec::new();
    if git_view_on_screen(state) {
        if let Some(hash) = state.git.show_want.clone()
            && state.git.detail.as_ref().map(|d| &d.hash) != Some(&hash)
            && state.git.show_in_flight.is_none()
        {
            state.git.show_in_flight = Some(hash.clone());
            actions.push(Action::GitShow { hash });
        }
        // A page that already failed on this `after` stays blocked while
        // the want still names it — the cursor moving off (or a landed
        // `apply_log`, which clears `more_failed`) is what re-arms it.
        if state.git.more_want != state.git.more_failed {
            state.git.more_failed = None;
        }
        if let Some(after) = state.git.more_want.clone()
            && state.git.has_more
            && !state.git.loading
            && state.git.more_failed.as_deref() != Some(after.as_str())
        {
            actions.push(Action::GitLog { after: Some(after) });
        }
    } else {
        state.git.show_want = None;
        state.git.more_want = None;
        state.git.more_failed = None;
    }
    actions
}

/// Whether the `Git` menu level is on screen — gates the detail-stat and
/// next-page wants (§5.10), like [`files_view_on_screen`] does for Files.
fn git_view_on_screen(state: &AppState) -> bool {
    matches!(
        &state.focus,
        crate::state::Focus::Overlay(crate::state::Overlay::Menu { stack, .. })
            if stack
                .current()
                .is_some_and(|l| l.id == crate::ui::menu::MenuId::Git)
    )
}

/// Whether the `Files` menu level is on screen (§5.5) — gates the preview
/// cache's cleanup and the deferred `git status` scan.
pub(super) fn files_view_on_screen(state: &AppState) -> bool {
    matches!(
        &state.focus,
        crate::state::Focus::Overlay(crate::state::Overlay::Menu { stack, .. })
            if stack
                .current()
                .is_some_and(|l| l.id == crate::ui::menu::MenuId::Files)
    )
}

/// Whether the `Background` menu level is on screen — gates the task-tail
/// cache's cleanup (§5.9).
pub(super) fn background_view_on_screen(state: &AppState) -> bool {
    matches!(
        &state.focus,
        crate::state::Focus::Overlay(crate::state::Overlay::Menu { stack, .. })
            if stack
                .current()
                .is_some_and(|l| l.id == crate::ui::menu::MenuId::Background)
    )
}

/// The preview pane's path resolution — `raw` relative to the session cwd
/// when that joined path exists. Runs inside `spawn_blocking`: `exists()` is
/// a filesystem stat and must never run inside `ui::draw` (§8.1).
fn resolve_preview_path(raw: &str, cwd: &std::path::Path) -> std::path::PathBuf {
    let p = std::path::Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let joined = cwd.join(p);
    if joined.exists() {
        joined
    } else {
        p.to_path_buf()
    }
}

/// Decode one inline thumbnail, bounded by file size: a kitty/sixel escape
/// carries the encoded image, and the frame path clones that payload on every
/// frame, so a huge paste would cost megabytes per frame for a slot. Past the
/// cap the slot answers `None` — the strip paints its placeholder and never
/// re-arms — while the prompt still attaches the original file.
pub(super) fn render_thumbnail(
    path: &std::path::Path,
    cell_w: u16,
    cell_h: u16,
) -> Option<pacode_image::Preview> {
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if size > IMAGE_THUMB_MAX_BYTES {
        log::info!(
            "inline thumbnail skipped: {} is {size} bytes (cap {IMAGE_THUMB_MAX_BYTES})",
            path.display()
        );
        return None;
    }
    pacode_image::render(
        &pacode_image::PreviewRequest {
            path: path.to_path_buf(),
            cell_w,
            cell_h,
        },
        pacode_image::detect(),
    )
    .ok()
}

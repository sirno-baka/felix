use popugos::window::{Event, Window};
use rusty_h264_common::nal::split_annex_b;
use rusty_h264_common::{NalUnitType, YuvFrame};
use rusty_h264_decoder::{au_is_idr, split_access_units, Decoder};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub struct VideoChunk {
    pub sequence: u64,
    pub duration: Option<f64>,
    pub fps: Option<f64>,
    pub h264: Vec<u8>,
}

struct DecodedFrame {
    poc: i32,
    frame: YuvFrame,
}

// Twitch commonly publishes ~2 s HLS segments (~60 frames at 30 fps).
// Keep more than one segment worth of decoded video so segment publication
// jitter never reaches the presenter.
const PRESENTATION_BUFFER_FRAMES: usize = 32;
const REBUFFER_FRAMES: usize = 16;
const READY_BUFFER_FRAMES: usize = 48;
// Twitch transcodes normally use a shallow B-frame reorder. Eight frames kept
// the tail of every ~59-frame GOP hostage until the following IDR, producing a
// ~1.7 s burst followed by a short starvation. Four is ample for the observed
// stream while releasing display frames much earlier.
const REORDER_BUFFER_FRAMES: usize = 4;
// Keep only a short decoded handoff queue. Once READY_BUFFER_FRAMES is full,
// the decoder blocks and compressed HLS segments remain compressed upstream.
const READY_FRAME_CHANNEL_CAPACITY: usize = 8;

pub fn spawn(
    rx: mpsc::Receiver<VideoChunk>,
    stop: Arc<AtomicBool>,
    width: u32,
    height: u32,
    fps_hint: f64,
) -> thread::JoinHandle<Result<(), String>> {
    thread::spawn(move || {
        let result = run(rx, stop.clone(), width, height, fps_hint);
        if result.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        result
    })
}

fn run(
    rx: mpsc::Receiver<VideoChunk>,
    stop: Arc<AtomicBool>,
    width: u32,
    height: u32,
    fps_hint: f64,
) -> Result<(), String> {
    let fps = sane_fps(fps_hint).unwrap_or(30.0);
    let fps_milli = Arc::new(AtomicU32::new((fps * 1000.0).round() as u32));
    let decoded_total = Arc::new(AtomicU64::new(0));
    let (frame_tx, frame_rx) =
        std_mpsc::sync_channel::<DecodedFrame>(READY_FRAME_CHANNEL_CAPACITY);

    let decode_stop = stop.clone();
    let decode_total = decoded_total.clone();
    let decode_fps = fps_milli.clone();
    let decode_task = thread::spawn(move || {
        decode_stream(rx, decode_stop, frame_tx, decode_total, decode_fps)
    });

    let present_result = present_stream(
        frame_rx,
        stop.clone(),
        width,
        height,
        fps_milli,
        decoded_total,
    );
    if present_result.is_err() {
        stop.store(true, Ordering::Relaxed);
    }

    let decode_result = decode_task
        .join()
        .map_err(|_| "video decoder worker panicked".to_string())?;
    decode_result?;
    present_result
}

fn decode_stream(
    mut rx: mpsc::Receiver<VideoChunk>,
    stop: Arc<AtomicBool>,
    frame_tx: std_mpsc::SyncSender<DecodedFrame>,
    decoded_counter: Arc<AtomicU64>,
    fps_milli: Arc<AtomicU32>,
) -> Result<(), String> {
    let mut decoder = Decoder::new();
    let mut au_pending = Vec::new();
    let mut reorder_frames = Vec::with_capacity(REORDER_BUFFER_FRAMES + 1);
    let mut gop_index = 0u64;
    let mut decoded_total = 0u64;

    while !stop.load(Ordering::Relaxed) {
        let wait_started = Instant::now();
        let Some(chunk) = rx.blocking_recv() else { break };
        let rx_wait = wait_started.elapsed();

        if let Some(fps) = chunk.fps {
            let fps = canonical_fps(fps);
            let new_milli = (fps * 1000.0).round() as u32;
            let old_milli = fps_milli.swap(new_milli, Ordering::Relaxed);
            if old_milli.abs_diff(new_milli) >= 250 {
                println!(
                    "[video] pts-fps seq={} {:.3} -> {:.3}",
                    chunk.sequence,
                    old_milli as f64 / 1000.0,
                    fps
                );
            }
        }

        let decoded_before = decoded_total;
        let process_started = Instant::now();
        let mut tx_wait = Duration::ZERO;

        au_pending.extend_from_slice(&chunk.h264);

        let mt_threads = if decode_threaded_if_independent(
            &mut au_pending,
            &stop,
            &frame_tx,
            &mut decoded_total,
            &decoded_counter,
            &mut tx_wait,
        )? {
            3
        } else {
            decode_complete_access_units(
                &mut au_pending,
                &mut decoder,
                &stop,
                &frame_tx,
                &mut reorder_frames,
                &mut gop_index,
                &mut decoded_total,
                &decoded_counter,
                &mut tx_wait,
                false,
            )?;
            0
        };

        let process_elapsed = process_started.elapsed();
        let work_elapsed = process_elapsed.saturating_sub(tx_wait);
        let frames = decoded_total.saturating_sub(decoded_before);
        let budget_ms = chunk.duration.unwrap_or(0.0) * 1000.0;
        println!(
            "[dec] seq={} frames={} budget={:.0}ms work={:.0}ms txwait={:.0}ms rxwait={:.0}ms au_pending={:.1}KiB mt={}",
            chunk.sequence,
            frames,
            budget_ms,
            work_elapsed.as_secs_f64() * 1000.0,
            tx_wait.as_secs_f64() * 1000.0,
            rx_wait.as_secs_f64() * 1000.0,
            au_pending.len() as f64 / 1024.0,
            mt_threads,
        );
    }

    if !stop.load(Ordering::Relaxed) {
        let mut tx_wait = Duration::ZERO;
        decode_complete_access_units(
            &mut au_pending,
            &mut decoder,
            &stop,
            &frame_tx,
            &mut reorder_frames,
            &mut gop_index,
            &mut decoded_total,
            &decoded_counter,
            &mut tx_wait,
            true,
        )?;
        flush_reorder_buffer(
            &mut reorder_frames,
            gop_index,
            decoded_total,
            &frame_tx,
            &stop,
            &mut tx_wait,
        )?;
    }

    Ok(())
}

fn annexb_is_independent_idr(stream: &[u8]) -> bool {
    let mut saw_sps = false;
    let mut saw_pps = false;

    for nal in split_annex_b(stream) {
        if nal.is_empty() {
            continue;
        }
        match NalUnitType::from_id(nal[0]) {
            NalUnitType::Sps => saw_sps = true,
            NalUnitType::Pps => saw_pps = true,
            NalUnitType::IdrSlice => return saw_sps && saw_pps,
            NalUnitType::NonIdrSlice => return false,
            _ => {}
        }
    }
    false
}

/// Use rusty_h264_decoder's built-in frame-level parallel decoder when the
/// complete prefix is a self-contained GOP (SPS + PPS + IDR). Returns true when
/// that prefix was consumed by the MT path.
fn decode_threaded_if_independent(
    pending: &mut Vec<u8>,
    stop: &Arc<AtomicBool>,
    frame_tx: &std_mpsc::SyncSender<DecodedFrame>,
    decoded_total: &mut u64,
    decoded_counter: &AtomicU64,
    tx_wait: &mut Duration,
) -> Result<bool, String> {
    let aus = split_access_units(pending);
    if aus.len() < 2 {
        return Ok(false);
    }

    // Retain the final access unit: without the start of the following AU we
    // cannot know that its final NAL is complete.
    let base = pending.as_ptr() as usize;
    let consumed = aus.last().unwrap().as_ptr() as usize - base;
    if consumed == 0 {
        return Ok(false);
    }

    let complete = &pending[..consumed];
    if !annexb_is_independent_idr(complete) {
        return Ok(false);
    }

    // The threaded path owns its decoder state internally, so it is only used
    // for independently decodable GOPs. It already emits display-order frames.
    let mut mt_decoder = Decoder::new();
    let mut send_failed = false;
    let result = mt_decoder.decode_stream_threaded_sink(complete, 3, |frame| {
        if send_failed || stop.load(Ordering::Relaxed) {
            return;
        }

        let started = Instant::now();
        let sent = frame_tx.send(DecodedFrame { poc: 0, frame }).is_ok();
        *tx_wait += started.elapsed();

        if sent {
            *decoded_total = decoded_total.saturating_add(1);
            decoded_counter.store(*decoded_total, Ordering::Relaxed);
        } else {
            send_failed = true;
        }
    });

    match result {
        Ok(_) => {
            pending.drain(..consumed);
            Ok(true)
        }
        Err(error) => {
            eprintln!("[dec] frame-mt fallback: {error:?}");
            Ok(false)
        }
    }
}

fn canonical_fps(measured: f64) -> f64 {
    for fps in [23.976, 24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0] {
        if (measured - fps).abs() <= 0.35 {
            return fps;
        }
    }
    measured
}

/// Decode complete Annex-B access units and retain exactly the final unit.
///
/// Decoder::decode requires one access unit. The previous implementation called
/// it once per NAL, which violated the decoder API and made picture completion
/// depend on NAL/HLS boundaries.
///
/// Keeping the last AU until more bytes arrive also makes an HLS/PES boundary
/// harmless if it lands in the middle of the final NAL/picture.
fn decode_complete_access_units(
    pending: &mut Vec<u8>,
    decoder: &mut Decoder,
    stop: &Arc<AtomicBool>,
    frame_tx: &std_mpsc::SyncSender<DecodedFrame>,
    reorder_frames: &mut Vec<DecodedFrame>,
    gop_index: &mut u64,
    decoded_total: &mut u64,
    decoded_counter: &AtomicU64,
    tx_wait: &mut Duration,
    flush_all: bool,
) -> Result<(), String> {
    if pending.is_empty() {
        return Ok(());
    }

    let aus = split_access_units(pending);
    if aus.is_empty() {
        return Ok(());
    }

    let decode_count = if flush_all {
        aus.len()
    } else {
        aus.len().saturating_sub(1)
    };
    if decode_count == 0 {
        return Ok(());
    }

    let base = pending.as_ptr() as usize;
    let consumed = if decode_count < aus.len() {
        aus[decode_count].as_ptr() as usize - base
    } else {
        pending.len()
    };

    for au in aus.into_iter().take(decode_count) {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        if au_is_idr(au) && !reorder_frames.is_empty() {
            flush_reorder_buffer(
                reorder_frames,
                *gop_index,
                *decoded_total,
                frame_tx,
                stop,
                tx_wait,
            )?;
            *gop_index = (*gop_index).saturating_add(1);
        }

        match decoder.decode(au) {
            Ok(Some(frame)) => {
                let poc = decoder.last_poc();

                if let Some(existing) = reorder_frames.iter_mut().find(|f| f.poc == poc) {
                    existing.frame = frame;
                } else {
                    *decoded_total = decoded_total.saturating_add(1);
                    decoded_counter.store(*decoded_total, Ordering::Relaxed);
                    reorder_frames.push(DecodedFrame { poc, frame });
                }

                emit_reorder_ready(reorder_frames, frame_tx, stop, tx_wait)?;
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("[dec] error: {error:?}");
            }
        }
    }

    if consumed != 0 {
        pending.drain(..consumed);
    }
    Ok(())
}

fn send_decoded_frame(
    frame_tx: &std_mpsc::SyncSender<DecodedFrame>,
    frame: DecodedFrame,
    tx_wait: &mut Duration,
) -> bool {
    let started = Instant::now();
    let ok = frame_tx.send(frame).is_ok();
    *tx_wait += started.elapsed();
    ok
}

fn emit_reorder_ready(
    frames: &mut Vec<DecodedFrame>,
    frame_tx: &std_mpsc::SyncSender<DecodedFrame>,
    stop: &Arc<AtomicBool>,
    tx_wait: &mut Duration,
) -> Result<(), String> {
    if frames.len() <= REORDER_BUFFER_FRAMES {
        return Ok(());
    }

    frames.sort_by_key(|frame| frame.poc);
    while frames.len() > REORDER_BUFFER_FRAMES {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let frame = frames.remove(0);
        if !send_decoded_frame(frame_tx, frame, tx_wait) {
            return Ok(());
        }
    }
    Ok(())
}

fn flush_reorder_buffer(
    frames: &mut Vec<DecodedFrame>,
    gop_index: u64,
    decoded_total: u64,
    frame_tx: &std_mpsc::SyncSender<DecodedFrame>,
    stop: &Arc<AtomicBool>,
    tx_wait: &mut Duration,
) -> Result<(), String> {
    if frames.is_empty() {
        return Ok(());
    }

    frames.sort_by_key(|frame| frame.poc);
    let poc_min = frames.first().map(|frame| frame.poc).unwrap_or(0);
    let poc_max = frames.last().map(|frame| frame.poc).unwrap_or(0);
    let count = frames.len();

    println!(
        "[dec] gop={} tail={} poc={}..{} decoded={}",
        gop_index, count, poc_min, poc_max, decoded_total
    );

    for frame in frames.drain(..) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if !send_decoded_frame(frame_tx, frame, tx_wait) {
            break;
        }
    }
    Ok(())
}

fn present_stream(
    frame_rx: std_mpsc::Receiver<DecodedFrame>,
    stop: Arc<AtomicBool>,
    width: u32,
    height: u32,
    fps_milli: Arc<AtomicU32>,
    decoded_counter: Arc<AtomicU64>,
) -> Result<(), String> {
    let width = width.max(40);
    let height = height.max(40);
    let mut window = Window::builder()
        .title("Twitch 480p")
        .size(width, height)
        .build()
        .map_err(|error| format!("video window: {error}"))?;

    let mut ready_frames = VecDeque::with_capacity(READY_BUFFER_FRAMES);
    let mut disconnected = false;
    let mut next_present = Instant::now();
    let mut displayed_frames = 0u64;
    let mut dropped_frames = 0u64;
    let mut stats_started = Instant::now();
    let mut stats_shown = 0u64;
    let mut stats_decoded = 0u64;
    let mut render_time = Duration::ZERO;
    let mut present_time = Duration::ZERO;
    let mut ready_min = usize::MAX;
    let mut ready_max = 0usize;

    println!(
        "[video] {}x{} prebuf={}f (~{:.0}ms) ready_cap={} frame_ch={} reorder={}",
        window.client_width(),
        window.client_height(),
        PRESENTATION_BUFFER_FRAMES,
        PRESENTATION_BUFFER_FRAMES as f64 * 1000.0
            / (fps_milli.load(Ordering::Relaxed).max(1) as f64 / 1000.0),
        READY_BUFFER_FRAMES,
        READY_FRAME_CHANNEL_CAPACITY,
        REORDER_BUFFER_FRAMES
    );

    while !stop.load(Ordering::Relaxed) {
        if ready_frames.is_empty() && !disconnected {
            let target = if displayed_frames == 0 {
                PRESENTATION_BUFFER_FRAMES
            } else {
                REBUFFER_FRAMES
            };
            while ready_frames.len() < target && !stop.load(Ordering::Relaxed) {
                match frame_rx.recv() {
                    Ok(frame) => ready_frames.push_back(frame),
                    Err(_) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if !ready_frames.is_empty() {
                println!("[buf] resume ready={} target={}", ready_frames.len(), target);

                // A genuine underrun means the old presentation deadline is no
                // longer meaningful. If we keep it, the late-frame logic below
                // immediately drops almost the entire buffer we just rebuilt:
                //
                //   underrun -> collect N frames -> "N frames late" -> drop N-1
                //
                // Restart the presentation clock at the resume point. This does
                // not create slow motion; it only turns an unavoidable stall into
                // a small increase in live latency instead of a drop/rebuffer loop.
                next_present = Instant::now();

                if displayed_frames == 0 {
                    stats_started = next_present;
                    stats_decoded = decoded_counter.load(Ordering::Relaxed);
                }
            }
        }

        if ready_frames.is_empty() {
            break;
        }

        // Pull all already-decoded frames into the presentation queue, not just
        // the first second. With segmented live video the decoder naturally emits
        // in bursts; retaining up to ~4 s smooths those bursts without adding any
        // copy of the YUV planes.
        while ready_frames.len() < READY_BUFFER_FRAMES {
            match frame_rx.try_recv() {
                Ok(frame) => ready_frames.push_back(frame),
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        ready_min = ready_min.min(ready_frames.len());
        ready_max = ready_max.max(ready_frames.len());

        let fps = fps_milli.load(Ordering::Relaxed).max(1) as f64 / 1000.0;
        let frame_interval = Duration::from_secs_f64(1.0 / fps);

        // Live playback must follow wall clock, not promise to display every
        // decoded frame. If render/composite or scheduler latency puts us more
        // than one frame behind, discard enough old frames to catch up. Keeping
        // every frame here produces smooth but permanently slow-motion video.
        let now = Instant::now();
        if now > next_present + frame_interval && ready_frames.len() > 1 {
            let late = now.duration_since(next_present);
            let late_frames =
                (late.as_secs_f64() / frame_interval.as_secs_f64()).floor() as usize;
            let drop_count = late_frames.min(ready_frames.len().saturating_sub(1));
            for _ in 0..drop_count {
                let _ = ready_frames.pop_front();
            }
            if drop_count != 0 {
                dropped_frames = dropped_frames.saturating_add(drop_count as u64);
                next_present += Duration::from_secs_f64(
                    frame_interval.as_secs_f64() * drop_count as f64,
                );
            }
        }

        let Some(decoded) = ready_frames.pop_front() else { continue };
        let Some((render_elapsed, present_elapsed)) = present_frame(
            &decoded.frame,
            &mut window,
            &stop,
            frame_interval,
            &mut next_present,
        )? else {
            break;
        };

        render_time += render_elapsed;
        present_time += present_elapsed;
        displayed_frames = displayed_frames.saturating_add(1);
        if displayed_frames == 1 || displayed_frames % 30 == 0 {
            let ready_bytes: usize = ready_frames.iter().map(|f| frame_bytes(&f.frame)).sum();
            let decoded_total = decoded_counter.load(Ordering::Relaxed);
            let elapsed = stats_started.elapsed().as_secs_f64().max(0.001);
            let shown_delta = displayed_frames.saturating_sub(stats_shown);
            let decoded_delta = decoded_total.saturating_sub(stats_decoded);
            let wall_fps = shown_delta as f64 / elapsed;
            let decode_fps = decoded_delta as f64 / elapsed;
            let avg_render_ms =
                render_time.as_secs_f64() * 1000.0 / displayed_frames.max(1) as f64;
            let avg_present_ms =
                present_time.as_secs_f64() * 1000.0 / displayed_frames.max(1) as f64;
            #[cfg(target_os = "popugos")]
            let (heap_live, heap_peak, mmap_allocs, mmap_frees) = crate::allocator::mapped_stats();
            #[cfg(not(target_os = "popugos"))]
            let (heap_live, heap_peak, mmap_allocs, mmap_frees) = (0usize, 0usize, 0usize, 0usize);

            println!(
                "[video] target={:.2} wall={:.2} decode={:.2} decoded={} shown={} dropped={} pending={} ready={} min={} max={} ({:.1}MiB) heap={:.1}/{:.1}MiB mmap={}/{} render={:.2}ms present={:.2}ms poc={}",
                fps,
                wall_fps,
                decode_fps,
                decoded_total,
                displayed_frames,
                dropped_frames,
                decoded_total
                    .saturating_sub(displayed_frames)
                    .saturating_sub(dropped_frames),
                ready_frames.len(),
                if ready_min == usize::MAX { 0 } else { ready_min },
                ready_max,
                ready_bytes as f64 / (1024.0 * 1024.0),
                heap_live as f64 / (1024.0 * 1024.0),
                heap_peak as f64 / (1024.0 * 1024.0),
                mmap_allocs,
                mmap_frees,
                avg_render_ms,
                avg_present_ms,
                decoded.poc
            );
            #[cfg(target_os = "popugos")]
            if displayed_frames % 300 == 0 {
                let mem = crate::allocator::stats();
                println!(
                    "[mem] live={:.1}MiB peak={:.1}MiB mapped={:.1}MiB mapped_peak={:.1}MiB",
                    mem.live_bytes as f64 / (1024.0 * 1024.0),
                    mem.live_peak as f64 / (1024.0 * 1024.0),
                    mem.mapped_bytes as f64 / (1024.0 * 1024.0),
                    mem.mapped_peak as f64 / (1024.0 * 1024.0),
                );
            }

            stats_started = Instant::now();
            stats_shown = displayed_frames;
            stats_decoded = decoded_total;
            ready_min = usize::MAX;
            ready_max = 0;
        }

        if ready_frames.is_empty() && !disconnected && !stop.load(Ordering::Relaxed) {
            println!("[buf] underrun -> rebuffer {} frames", REBUFFER_FRAMES);
        }
    }

    println!(
        "[video] stopped shown={} dropped={}",
        displayed_frames, dropped_frames
    );
    Ok(())
}

fn present_frame(
    frame: &YuvFrame,
    window: &mut Window,
    stop: &Arc<AtomicBool>,
    frame_interval: Duration,
    next_present: &mut Instant,
) -> Result<Option<(Duration, Duration)>, String> {
    while let Some(event) = window.poll_event() {
        if matches!(event, Event::Close) {
            stop.store(true, Ordering::Relaxed);
            return Ok(None);
        }
    }

    // Render into the shared back buffer first. Presentation is paced against
    // an absolute timeline below, so render time and scheduler sleep overshoot
    // are not added to every frame period.
    let render_started = Instant::now();
    render_yuv420(frame, window)?;
    let render_elapsed = render_started.elapsed();

    let target = *next_present;
    let now = Instant::now();
    if target > now {
        let remaining = target - now;
        // Felix sleep is tick based. Sleep most of the interval, then spin the
        // final millisecond so a rounded-up sleep tick does not turn 30 fps
        // into smooth ~20-25 fps slow motion.
        let guard = Duration::from_millis(1);
        if remaining > guard {
            thread::sleep(remaining - guard);
        }
        while Instant::now() < target {
            core::hint::spin_loop();
        }
    }

    let present_started = Instant::now();
    window.present().map_err(|error| format!("video present: {error}"))?;
    let present_elapsed = present_started.elapsed();

    // Advance from the previous target, never from the actual wake-up time.
    // If rendering or scheduling is late, following frames automatically catch
    // up instead of permanently slowing the playback clock.
    *next_present = target + frame_interval;
    Ok(Some((render_elapsed, present_elapsed)))
}

fn render_yuv420(frame: &YuvFrame, window: &mut Window) -> Result<(), String> {
    let src_w = frame.width;
    let src_h = frame.height;
    if src_w == 0 || src_h == 0 || src_w & 1 != 0 || src_h & 1 != 0 {
        return Err(format!("unsupported H.264 frame size {}x{}", frame.width, frame.height));
    }
    if frame.y.len() < src_w * src_h
        || frame.u.len() < (src_w / 2) * (src_h / 2)
        || frame.v.len() < (src_w / 2) * (src_h / 2)
    {
        return Err("short YUV420 frame".to_string());
    }

    let dst_w = window.client_width() as usize;
    let dst_h = window.client_height() as usize;
    let pitch = window.pitch();
    let copy_w = src_w.min(dst_w);
    let copy_h = src_h.min(dst_h);
    let dst_x = (dst_w.saturating_sub(copy_w)) / 2;
    let dst_y = (dst_h.saturating_sub(copy_h)) / 2;
    let src_x = (src_w.saturating_sub(copy_w)) / 2;
    let src_y = (src_h.saturating_sub(copy_h)) / 2;

    let buffer = window.buffer_mut();

    // Common Twitch path: the video window is created at the decoded frame
    // size. Process one 2x2 YUV420 block at a time so its single U/V sample is
    // shared by four output pixels. This also avoids clearing the whole BGRX
    // buffer immediately before overwriting every visible pixel.
    if src_w == dst_w && src_h == dst_h {
        for y in (0..src_h).step_by(2) {
            let y0_row = y * src_w;
            let y1_row = (y + 1) * src_w;
            let uv_row = (y / 2) * (src_w / 2);
            let dst0_row = y * pitch;
            let dst1_row = (y + 1) * pitch;

            for x in (0..src_w).step_by(2) {
                let uv = uv_row + x / 2;
                let d = frame.u[uv] as i32 - 128;
                let e = frame.v[uv] as i32 - 128;
                let r_add = 409 * e + 128;
                let g_add = -100 * d - 208 * e + 128;
                let b_add = 516 * d + 128;

                write_bgrx_y(
                    buffer,
                    dst0_row + x * 4,
                    frame.y[y0_row + x],
                    r_add,
                    g_add,
                    b_add,
                );
                write_bgrx_y(
                    buffer,
                    dst0_row + (x + 1) * 4,
                    frame.y[y0_row + x + 1],
                    r_add,
                    g_add,
                    b_add,
                );
                write_bgrx_y(
                    buffer,
                    dst1_row + x * 4,
                    frame.y[y1_row + x],
                    r_add,
                    g_add,
                    b_add,
                );
                write_bgrx_y(
                    buffer,
                    dst1_row + (x + 1) * 4,
                    frame.y[y1_row + x + 1],
                    r_add,
                    g_add,
                    b_add,
                );
            }
        }
        return Ok(());
    }

    // Resize/crop fallback. Borders need clearing because they are not covered
    // by the source image.
    buffer.fill(0);
    for y in 0..copy_h {
        let sy = src_y + y;
        let y_row = sy * src_w;
        let uv_row = (sy / 2) * (src_w / 2);
        let dst_row = (dst_y + y) * pitch + dst_x * 4;

        for x in 0..copy_w {
            let sx = src_x + x;
            let yy = frame.y[y_row + sx] as i32;
            let d = frame.u[uv_row + sx / 2] as i32 - 128;
            let e = frame.v[uv_row + sx / 2] as i32 - 128;
            write_bgrx_y(
                buffer,
                dst_row + x * 4,
                yy as u8,
                409 * e + 128,
                -100 * d - 208 * e + 128,
                516 * d + 128,
            );
        }
    }

    Ok(())
}

#[inline(always)]
fn write_bgrx_y(
    buffer: &mut [u8],
    offset: usize,
    y: u8,
    r_add: i32,
    g_add: i32,
    b_add: i32,
) {
    let c = (y as i32 - 16).max(0);
    let luma = 298 * c;
    buffer[offset] = clamp_u8((luma + b_add) >> 8);
    buffer[offset + 1] = clamp_u8((luma + g_add) >> 8);
    buffer[offset + 2] = clamp_u8((luma + r_add) >> 8);
    buffer[offset + 3] = 0;
}

fn frame_bytes(frame: &YuvFrame) -> usize {
    frame.y.len() + frame.u.len() + frame.v.len()
}

fn sane_fps(fps: f64) -> Option<f64> {
    if fps.is_finite() && (10.0..=120.0).contains(&fps) {
        Some(fps)
    } else {
        None
    }
}

#[inline]
fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_annexb_nal_across_input_boundaries() {
        let mut pending = vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x65, 3];
        let first = take_complete_annexb(&mut pending);
        assert_eq!(first, vec![0, 0, 0, 1, 0x67, 1, 2]);
        assert_eq!(pending, vec![0, 0, 1, 0x65, 3]);

        pending.extend_from_slice(&[4, 5, 0, 0, 1, 0x41, 6]);
        let second = take_complete_annexb(&mut pending);
        assert_eq!(second, vec![0, 0, 1, 0x65, 3, 4, 5]);
        assert_eq!(pending, vec![0, 0, 1, 0x41, 6]);
    }

    #[test]
    fn identifies_nal_unit_type() {
        assert_eq!(nal_unit_type(&[0, 0, 1, 0x65, 1]), Some(5));
        assert_eq!(nal_unit_type(&[0, 0, 0, 1, 0x41, 1]), Some(1));
    }
}

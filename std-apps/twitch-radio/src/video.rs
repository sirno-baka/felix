use popugos::window::{Event, Window};
use rusty_h264_common::YuvFrame;
use rusty_h264_decoder::Decoder;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub struct VideoChunk {
    pub sequence: u64,
    pub h264: Vec<u8>,
}

struct DecodedFrame {
    poc: i32,
    frame: YuvFrame,
}

const PRESENTATION_BUFFER_FRAMES: usize = 16;
const READY_GOP_CHANNEL_CAPACITY: usize = 1;

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
    let (gop_tx, gop_rx) =
        std_mpsc::sync_channel::<Vec<DecodedFrame>>(READY_GOP_CHANNEL_CAPACITY);

    let decode_stop = stop.clone();
    let decode_total = decoded_total.clone();
    let decode_task = thread::spawn(move || {
        decode_stream(rx, decode_stop, gop_tx, decode_total)
    });

    let present_result = present_stream(
        gop_rx,
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
    gop_tx: std_mpsc::SyncSender<Vec<DecodedFrame>>,
    decoded_counter: Arc<AtomicU64>,
) -> Result<(), String> {
    let mut decoder = Decoder::new();
    let mut annexb_pending = Vec::new();
    let mut gop_frames = Vec::new();
    let mut gop_index = 0u64;
    let mut decoded_total = 0u64;

    while !stop.load(Ordering::Relaxed) {
        let Some(chunk) = rx.blocking_recv() else { break };
        annexb_pending.extend_from_slice(&chunk.h264);
        let complete = take_complete_annexb(&mut annexb_pending);
        if !complete.is_empty() {
            decode_complete_nals(
                &complete,
                &mut decoder,
                &stop,
                &gop_tx,
                &mut gop_frames,
                &mut gop_index,
                &mut decoded_total,
                &decoded_counter,
            )?;
        }
    }

    if !stop.load(Ordering::Relaxed) {
        if !annexb_pending.is_empty() {
            decode_complete_nals(
                &annexb_pending,
                &mut decoder,
                &stop,
                &gop_tx,
                &mut gop_frames,
                &mut gop_index,
                &mut decoded_total,
                &decoded_counter,
            )?;
        }
        emit_gop(
            &mut gop_frames,
            gop_index,
            decoded_total,
            &gop_tx,
            &stop,
        )?;
    }

    Ok(())
}

fn decode_complete_nals(
    bytes: &[u8],
    decoder: &mut Decoder,
    stop: &Arc<AtomicBool>,
    gop_tx: &std_mpsc::SyncSender<Vec<DecodedFrame>>,
    gop_frames: &mut Vec<DecodedFrame>,
    gop_index: &mut u64,
    decoded_total: &mut u64,
    decoded_counter: &AtomicU64,
) -> Result<(), String> {
    for nal in annexb_nals(bytes) {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // nal_unit_type == 5 is IDR. Flush the previous GOP before feeding
        // the first IDR slice. For multi-slice IDR pictures subsequent slices
        // see an empty GOP until that picture is complete, so no false split.
        let is_idr = nal_unit_type(nal) == Some(5);
        if is_idr && !gop_frames.is_empty() {
            emit_gop(gop_frames, *gop_index, *decoded_total, gop_tx, stop)?;
            *gop_index = (*gop_index).saturating_add(1);
        }

        match decoder.decode(nal) {
            Ok(Some(frame)) => {
                let poc = decoder.last_poc();
                *decoded_total = decoded_total.saturating_add(1);
                decoded_counter.store(*decoded_total, Ordering::Relaxed);
                gop_frames.push(DecodedFrame { poc, frame });
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("[dec] error: {error:?}");
            }
        }
    }
    Ok(())
}

fn emit_gop(
    frames: &mut Vec<DecodedFrame>,
    gop_index: u64,
    decoded_total: u64,
    gop_tx: &std_mpsc::SyncSender<Vec<DecodedFrame>>,
    stop: &Arc<AtomicBool>,
) -> Result<(), String> {
    if frames.is_empty() {
        return Ok(());
    }

    frames.sort_by_key(|frame| frame.poc);
    let poc_min = frames.first().map(|frame| frame.poc).unwrap_or(0);
    let poc_max = frames.last().map(|frame| frame.poc).unwrap_or(0);
    let frame_count = frames.len();
    let unique_poc = frames
        .windows(2)
        .filter(|pair| pair[0].poc != pair[1].poc)
        .count()
        .saturating_add(1);
    let duplicate_poc = frame_count.saturating_sub(unique_poc);
    let yuv_bytes: usize = frames.iter().map(|frame| frame_bytes(&frame.frame)).sum();

    println!(
        "[dec] gop={} frames={} unique_poc={} dup={} poc={}..{} yuv={:.1}MiB decoded={}",
        gop_index,
        frame_count,
        unique_poc,
        duplicate_poc,
        poc_min,
        poc_max,
        yuv_bytes as f64 / (1024.0 * 1024.0),
        decoded_total
    );

    let ready = std::mem::take(frames);
    if !stop.load(Ordering::Relaxed) && gop_tx.send(ready).is_err() {
        return Ok(());
    }
    Ok(())
}

fn take_complete_annexb(pending: &mut Vec<u8>) -> Vec<u8> {
    let mut starts = annexb_start_codes(pending);
    if starts.is_empty() {
        return Vec::new();
    }

    if starts[0] != 0 {
        pending.drain(..starts[0]);
        starts = annexb_start_codes(pending);
    }

    if starts.len() < 2 {
        return Vec::new();
    }

    let split = *starts.last().unwrap();
    let tail = pending.split_off(split);
    std::mem::replace(pending, tail)
}

fn annexb_nals(bytes: &[u8]) -> Vec<&[u8]> {
    let starts = annexb_start_codes(bytes);
    let mut nals = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(bytes.len());
        if end > start {
            nals.push(&bytes[start..end]);
        }
    }
    nals
}

fn annexb_start_codes(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        if bytes[i] == 0 && bytes[i + 1] == 0 {
            if bytes[i + 2] == 1 {
                starts.push(i);
                i += 3;
                continue;
            }
            if i + 4 <= bytes.len() && bytes[i + 2] == 0 && bytes[i + 3] == 1 {
                starts.push(i);
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    starts
}

fn nal_unit_type(nal: &[u8]) -> Option<u8> {
    let header = if nal.starts_with(&[0, 0, 0, 1]) {
        *nal.get(4)?
    } else if nal.starts_with(&[0, 0, 1]) {
        *nal.get(3)?
    } else {
        return None;
    };
    Some(header & 0x1f)
}

fn present_stream(
    gop_rx: std_mpsc::Receiver<Vec<DecodedFrame>>,
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

    let mut ready_frames = VecDeque::with_capacity(PRESENTATION_BUFFER_FRAMES);
    let mut disconnected = false;
    let mut next_present = Instant::now();
    let mut displayed_frames = 0u64;

    println!(
        "[video] {}x{} prebuf={}f (~{:.3}ms) gop_ch={}",
        window.client_width(),
        window.client_height(),
        PRESENTATION_BUFFER_FRAMES,
        PRESENTATION_BUFFER_FRAMES as f64 * 1000.0
            / (fps_milli.load(Ordering::Relaxed).max(1) as f64 / 1000.0),
        READY_GOP_CHANNEL_CAPACITY
    );

    while !stop.load(Ordering::Relaxed) {
        if ready_frames.is_empty() && !disconnected {
            while ready_frames.len() < PRESENTATION_BUFFER_FRAMES
                && !stop.load(Ordering::Relaxed)
            {
                match gop_rx.recv() {
                    Ok(frames) => ready_frames.extend(frames),
                    Err(_) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if !ready_frames.is_empty() {
                println!("[buf] resume ready={}", ready_frames.len());
                next_present = Instant::now();
            }
        }

        if ready_frames.is_empty() {
            break;
        }

        while ready_frames.len() < PRESENTATION_BUFFER_FRAMES {
            match gop_rx.try_recv() {
                Ok(frames) => ready_frames.extend(frames),
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        let fps = fps_milli.load(Ordering::Relaxed).max(1) as f64 / 1000.0;
        let frame_interval = Duration::from_secs_f64(1.0 / fps);
        let Some(decoded) = ready_frames.pop_front() else { continue };
        if !present_frame(
            &decoded.frame,
            &mut window,
            &stop,
            frame_interval,
            &mut next_present,
        )? {
            break;
        }

        displayed_frames = displayed_frames.saturating_add(1);
        if displayed_frames == 1 || displayed_frames % 30 == 0 {
            let ready_bytes: usize = ready_frames.iter().map(|f| frame_bytes(&f.frame)).sum();
            let decoded_total = decoded_counter.load(Ordering::Relaxed);
            println!(
                "[video] fps={:.2} decoded={} shown={} pending={} ready={} ({:.1}MiB) poc={}",
                fps,
                decoded_total,
                displayed_frames,
                decoded_total.saturating_sub(displayed_frames),
                ready_frames.len(),
                ready_bytes as f64 / (1024.0 * 1024.0),
                decoded.poc
            );
        }

        if ready_frames.is_empty() && !disconnected && !stop.load(Ordering::Relaxed) {
            println!("[buf] underrun -> rebuffer {} frames", PRESENTATION_BUFFER_FRAMES);
        }
    }

    println!("[video] stopped shown={}", displayed_frames);
    Ok(())
}

fn present_frame(
    frame: &YuvFrame,
    window: &mut Window,
    stop: &Arc<AtomicBool>,
    frame_interval: Duration,
    next_present: &mut Instant,
) -> Result<bool, String> {
    while let Some(event) = window.poll_event() {
        if matches!(event, Event::Close) {
            stop.store(true, Ordering::Relaxed);
            return Ok(false);
        }
    }

    let now = Instant::now();
    if *next_present > now {
        thread::sleep(*next_present - now);
    }

    // Rendering is part of the 1/FPS budget. Scheduling the next frame after
    // render+present would add that work on top of the frame interval and
    // effectively halve playback speed on real hardware.
    let frame_started = Instant::now();
    render_yuv420(frame, window)?;
    window.present().map_err(|error| format!("video present: {error}"))?;
    let deadline = frame_started + frame_interval;
    *next_present = deadline.max(Instant::now());
    Ok(true)
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
    buffer.fill(0);

    for y in 0..copy_h {
        let sy = src_y + y;
        let y_row = sy * src_w;
        let uv_row = (sy / 2) * (src_w / 2);
        let dst_row = (dst_y + y) * pitch + dst_x * 4;

        for x in 0..copy_w {
            let sx = src_x + x;
            let yy = frame.y[y_row + sx] as i32;
            let uu = frame.u[uv_row + sx / 2] as i32;
            let vv = frame.v[uv_row + sx / 2] as i32;

            let c = (yy - 16).max(0);
            let d = uu - 128;
            let e = vv - 128;
            let r = clamp_u8((298 * c + 409 * e + 128) >> 8);
            let g = clamp_u8((298 * c - 100 * d - 208 * e + 128) >> 8);
            let b = clamp_u8((298 * c + 516 * d + 128) >> 8);

            let offset = dst_row + x * 4;
            buffer[offset] = b;
            buffer[offset + 1] = g;
            buffer[offset + 2] = r;
            buffer[offset + 3] = 0;
        }
    }

    Ok(())
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

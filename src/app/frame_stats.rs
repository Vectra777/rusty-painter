//! Frame timing: how long each stage of a frame takes on the main thread,
//! for the readout in the top bar and the Frame Times window (View →
//! Frame Times). Off by default; while off it costs nothing.
//!
//! The frame loop marks the end of each stage ([`FrameStats::mark`]); the
//! time since the previous mark goes to that stage. egui's own rendering
//! (tessellating and submitting the frame) happens after `update` returns,
//! so it comes from eframe's CPU time for the previous frame, minus what
//! `update` took.

use std::collections::VecDeque;
use std::time::Instant;

/// A stage of the frame, in the order the loop runs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Theme, shortcuts, finished background work, layer thumbnails.
    Setup,
    /// The top bar, tool strip, rails and side panels.
    Panels,
    /// Placing the view, pen and touch input, the active tool's work.
    Tools,
    /// Waiting for the stroke worker to paint this frame's dabs.
    Stroke,
    /// Compositing dirty tiles and preparing their upload.
    Tiles,
    /// Drawing the canvas and the overlays on it.
    Canvas,
    /// Dialogs and floating windows.
    Windows,
    /// egui tessellating and submitting the frame to the GPU.
    Render,
}

/// What a stage counts as in the graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Ui,
    Tools,
    Pixels,
    Render,
}

impl Stage {
    pub const ALL: [Stage; 8] = [
        Stage::Setup,
        Stage::Panels,
        Stage::Tools,
        Stage::Stroke,
        Stage::Tiles,
        Stage::Canvas,
        Stage::Windows,
        Stage::Render,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Stage::Setup => "Setup",
            Stage::Panels => "Panels",
            Stage::Tools => "Input & tools",
            Stage::Stroke => "Stroke wait",
            Stage::Tiles => "Tile compositing",
            Stage::Canvas => "Canvas draw",
            Stage::Windows => "Windows",
            Stage::Render => "egui render",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Stage::Setup => "Theme, shortcuts, finished exports, layer thumbnails",
            Stage::Panels => "Top bar, tool strip, rails, side panels",
            Stage::Tools => "Placing the view, pen and touch input, the active tool",
            Stage::Stroke => "Waiting for the stroke worker to paint this frame's dabs",
            Stage::Tiles => "Compositing dirty tiles for upload to the GPU",
            Stage::Canvas => "Drawing the canvas and its overlays",
            Stage::Windows => "Dialogs and floating windows (this one included)",
            Stage::Render => "egui tessellating and submitting the frame (previous frame)",
        }
    }

    pub fn group(self) -> Group {
        match self {
            Stage::Setup | Stage::Panels | Stage::Windows => Group::Ui,
            Stage::Tools => Group::Tools,
            Stage::Stroke | Stage::Tiles | Stage::Canvas => Group::Pixels,
            Stage::Render => Group::Render,
        }
    }
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Ui, Group::Tools, Group::Pixels, Group::Render];

    pub fn label(self) -> &'static str {
        match self {
            Group::Ui => "UI",
            Group::Tools => "Input & tools",
            Group::Pixels => "Canvas pixels",
            Group::Render => "egui render",
        }
    }
}

const STAGES: usize = Stage::ALL.len();
/// Frames kept: a bit over a second at 240 Hz.
const HISTORY: usize = 300;
/// A gap between frames longer than this is idle time, not a slow frame.
const IDLE_GAP_MS: f32 = 250.0;

/// One finished frame, in milliseconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameTimes {
    /// Since the previous frame started; `None` after an idle gap.
    pub interval: Option<f32>,
    pub stages: [f32; STAGES],
}

impl FrameTimes {
    pub fn total(&self) -> f32 {
        self.stages.iter().sum()
    }

    pub fn stage(&self, stage: Stage) -> f32 {
        self.stages[stage as usize]
    }

    pub fn group(&self, group: Group) -> f32 {
        Stage::ALL
            .iter()
            .filter(|s| s.group() == group)
            .map(|&s| self.stage(s))
            .sum()
    }
}

/// Averages and worst cases over the recent frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameSummary {
    /// Frames per second while frames come back to back (idle gaps left
    /// out); `None` when there were none.
    pub fps: Option<f32>,
    pub avg: FrameTimes,
    pub max: FrameTimes,
    /// Slowest whole frame.
    pub max_total: f32,
}

#[derive(Default)]
pub struct FrameStats {
    /// Measuring (the readout is shown).
    pub enabled: bool,
    /// The Frame Times window is open.
    pub window_open: bool,
    /// Redraw every frame while the window is open, to measure the steady
    /// cost and the highest frame rate.
    pub continuous: bool,
    frames: VecDeque<FrameTimes>,
    current: [f32; STAGES],
    frame_start: Option<Instant>,
    last_mark: Option<Instant>,
    prev_start: Option<Instant>,
}

impl FrameStats {
    /// A frame starts. `cpu_usage` is eframe's CPU time for the previous
    /// frame (update and rendering), in seconds.
    pub fn begin(&mut self, cpu_usage: Option<f32>) {
        if !self.enabled {
            self.frame_start = None;
            self.prev_start = None;
            return;
        }
        let now = Instant::now();
        if let Some(start) = self.frame_start {
            let mut stages = self.current;
            let update: f32 = stages.iter().sum();
            stages[Stage::Render as usize] =
                cpu_usage.map_or(0.0, |s| (s * 1000.0 - update).max(0.0));
            let interval = self
                .prev_start
                .map(|p| ms(start - p))
                .filter(|&i| i < IDLE_GAP_MS);
            if self.frames.len() == HISTORY {
                self.frames.pop_front();
            }
            self.frames.push_back(FrameTimes { interval, stages });
            self.prev_start = Some(start);
        }
        self.current = [0.0; STAGES];
        self.frame_start = Some(now);
        self.last_mark = Some(now);
    }

    /// `stage` just finished: the time since the last mark is its.
    pub fn mark(&mut self, stage: Stage) {
        let Some(last) = self.last_mark else {
            return;
        };
        let now = Instant::now();
        self.current[stage as usize] += ms(now - last);
        self.last_mark = Some(now);
    }

    /// The recorded frames, oldest first.
    pub fn frames(&self) -> impl ExactSizeIterator<Item = &FrameTimes> + DoubleEndedIterator {
        self.frames.iter()
    }

    /// Over the frames of about the last second.
    pub fn summary(&self) -> FrameSummary {
        let mut span = 0.0;
        let recent: Vec<&FrameTimes> = self
            .frames
            .iter()
            .rev()
            .take_while(|f| {
                span += f.interval.unwrap_or(0.0);
                span <= 1000.0
            })
            .collect();
        let n = recent.len();
        if n == 0 {
            return FrameSummary::default();
        }
        let mut avg = FrameTimes::default();
        let mut max = FrameTimes::default();
        let mut max_total: f32 = 0.0;
        for f in &recent {
            for i in 0..STAGES {
                avg.stages[i] += f.stages[i] / n as f32;
                max.stages[i] = max.stages[i].max(f.stages[i]);
            }
            max_total = max_total.max(f.total());
        }
        let intervals: Vec<f32> = recent.iter().filter_map(|f| f.interval).collect();
        let fps = (!intervals.is_empty()).then(|| {
            let mean = intervals.iter().sum::<f32>() / intervals.len() as f32;
            1000.0 / mean.max(0.01)
        });
        FrameSummary {
            fps,
            avg,
            max,
            max_total,
        }
    }
}

fn ms(d: std::time::Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_add_up_and_idle_gaps_are_not_frames() {
        let mut stats = FrameStats {
            enabled: true,
            ..Default::default()
        };
        stats.begin(None);
        std::thread::sleep(std::time::Duration::from_millis(2));
        stats.mark(Stage::Panels);
        stats.mark(Stage::Tiles);
        stats.begin(Some(0.010));
        let f = *stats.frames().last().unwrap();
        assert!(f.stage(Stage::Panels) >= 2.0, "{f:?}");
        assert!(f.stage(Stage::Tiles) < 1.0);
        // eframe's 10 ms minus what update took.
        assert!((f.total() - 10.0).abs() < 0.5, "{}", f.total());
        assert_eq!(f.interval, None, "first frame: nothing before it");
        stats.begin(None);
        assert!(stats.frames().last().unwrap().interval.is_some());
        assert!(stats.summary().fps.is_some());
    }

    #[test]
    fn nothing_is_recorded_while_off() {
        let mut stats = FrameStats::default();
        stats.begin(None);
        stats.mark(Stage::Panels);
        stats.begin(None);
        assert_eq!(stats.frames().len(), 0);
    }
}

/// The display's refresh rate, measured: while frames run back to back they
/// are paced by vsync, so the usual shortest gap between them is one
/// refresh. (eframe doesn't expose the monitor's own rate.) Follows the
/// window to another monitor within a second or so of drawing there.
#[derive(Default)]
pub struct RefreshRate {
    last: Option<Instant>,
    /// Recent gaps between frames, in milliseconds.
    gaps: VecDeque<f32>,
}

/// Gaps kept for the estimate.
const REFRESH_SAMPLES: usize = 120;
/// Gaps outside this (ms) are idle time or a stall, not a refresh.
const REFRESH_GAP_RANGE: std::ops::RangeInclusive<f32> = 1.5..=40.0;

impl RefreshRate {
    /// A frame starts.
    pub fn tick(&mut self) {
        self.tick_at(Instant::now());
    }

    fn tick_at(&mut self, now: Instant) {
        if let Some(last) = self.last {
            let gap = ms(now - last);
            if REFRESH_GAP_RANGE.contains(&gap) {
                if self.gaps.len() == REFRESH_SAMPLES {
                    self.gaps.pop_front();
                }
                self.gaps.push_back(gap);
            }
        }
        self.last = Some(now);
    }

    /// One refresh, in milliseconds, once enough frames ran back to back.
    pub fn period_ms(&self) -> Option<f32> {
        if self.gaps.len() < 20 {
            return None;
        }
        let mut sorted: Vec<f32> = self.gaps.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        // A low percentile: slow frames only ever make gaps longer.
        Some(sorted[sorted.len() / 5])
    }

    /// Refreshes per second, when known.
    pub fn hz(&self) -> Option<f32> {
        self.period_ms().map(|p| 1000.0 / p)
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_refresh_rate_is_the_usual_shortest_gap() {
        let mut rate = RefreshRate::default();
        let start = Instant::now();
        let mut t = start;
        assert_eq!(rate.hz(), None, "not known yet");
        for i in 0..100 {
            rate.tick_at(t);
            // 260 Hz, with some frames running late and an idle pause.
            let gap = match i % 10 {
                3 => 7.7,
                7 => 400.0,
                _ => 1000.0 / 260.0,
            };
            t += Duration::from_secs_f32(gap / 1000.0);
        }
        let hz = rate.hz().unwrap();
        assert!((hz - 260.0).abs() < 2.0, "{hz}");
    }
}

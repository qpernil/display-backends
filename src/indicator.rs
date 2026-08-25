// SPDX-License-Identifier: GPL-3.0-or-later

//! Device-neutral activity-indicator scheduling.
//!
//! The scheduler owns timing and the logical indicator bit. Callers supply a
//! renderer that decides what that bit means visually and retain responsibility
//! for the lifetime and power state of the complete display.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cadence {
    pub on: Duration,
    pub off: Duration,
}

impl Cadence {
    pub const fn new(on: Duration, off: Duration) -> Self {
        Self { on, off }
    }

    fn delay(self, lit: bool) -> Duration {
        if lit {
            self.on
        } else {
            self.off
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlinkCount {
    Finite(u32),
    Forever,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdlePolicy {
    Off,
    Blink { cadence: Cadence, count: BlinkCount },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Policy {
    pub busy: Cadence,
    pub idle: IdlePolicy,
    pub minimum_edge: Duration,
}

impl Policy {
    pub const fn new(busy: Cadence, idle: IdlePolicy, minimum_edge: Duration) -> Self {
        Self {
            busy,
            idle,
            minimum_edge,
        }
    }
}

/// Applies the scheduler's single logical indicator bit.
///
/// Implementations may select complete frames, update a display region, or
/// drive a physical LED. The scheduler never powers down the containing
/// display.
pub trait IndicatorRenderer: Send + 'static {
    fn set_indicator(&mut self, lit: bool) -> io::Result<()>;
}

impl<F> IndicatorRenderer for F
where
    F: FnMut(bool) -> io::Result<()> + Send + 'static,
{
    fn set_indicator(&mut self, lit: bool) -> io::Result<()> {
        self(lit)
    }
}

#[derive(Clone)]
pub struct Activity {
    state: Arc<ActivityState>,
}

struct ActivityState {
    sender: Sender<Command>,
    command_active: AtomicBool,
    command_epoch: AtomicU64,
    notification_pending: AtomicBool,
}

impl ActivityState {
    fn notify(&self) {
        if !self.notification_pending.swap(true, Ordering::AcqRel)
            && self.sender.send(Command::ActivityChanged).is_err()
        {
            self.notification_pending.store(false, Ordering::Release);
        }
    }
}

impl Activity {
    /// Marks the start of the worker's single command.
    pub fn begin(&self) -> CommandGuard {
        let was_active = self.state.command_active.swap(true, Ordering::AcqRel);
        debug_assert!(!was_active, "indicator commands must not overlap");
        self.state.command_epoch.fetch_add(1, Ordering::AcqRel);
        self.state.notify();
        CommandGuard {
            state: Arc::clone(&self.state),
        }
    }

    /// Temporarily replaces command/idle scheduling with an attention cadence.
    pub fn attention(&self, cadence: Cadence) -> io::Result<AttentionGuard> {
        validate_cadence(cadence)?;
        self.state
            .sender
            .send(Command::AttentionStarted(cadence))
            .map_err(|_| stopped())?;
        Ok(AttentionGuard {
            sender: self.state.sender.clone(),
        })
    }
}

pub struct CommandGuard {
    state: Arc<ActivityState>,
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        self.state.command_active.store(false, Ordering::Release);
        self.state.notify();
    }
}

pub struct AttentionGuard {
    sender: Sender<Command>,
}

impl Drop for AttentionGuard {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::AttentionEnded);
    }
}

pub struct Controller {
    sender: Sender<Command>,
    activity_state: Arc<ActivityState>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl Controller {
    pub fn start(
        policy: Policy,
        renderer: impl IndicatorRenderer,
        thread_name: impl Into<String>,
    ) -> io::Result<Self> {
        validate_policy(policy)?;
        let (sender, receiver) = mpsc::channel();
        let activity_state = Arc::new(ActivityState {
            sender: sender.clone(),
            command_active: AtomicBool::new(false),
            command_epoch: AtomicU64::new(0),
            notification_pending: AtomicBool::new(false),
        });
        let display_state = Arc::clone(&activity_state);
        let thread = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || run(policy, renderer, receiver, display_state))?;
        Ok(Self {
            sender,
            activity_state,
            thread: Some(thread),
        })
    }

    pub fn activity(&self) -> Activity {
        Activity {
            state: Arc::clone(&self.activity_state),
        }
    }

    /// Enables indicator scheduling without changing the containing display's
    /// power state. Initial idle always starts with the indicator off.
    pub fn enable(&self) -> io::Result<()> {
        self.set_enabled(true)
    }

    /// Stops scheduling and renders the indicator off. The caller independently
    /// decides whether the containing display should remain visible or power off.
    pub fn disable(&self) -> io::Result<()> {
        self.set_enabled(false)
    }

    fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        let (sender, receiver) = mpsc::sync_channel(0);
        self.sender
            .send(Command::SetEnabled(enabled, sender))
            .map_err(|_| stopped())?;
        receiver.recv().map_err(|_| stopped())?
    }

    pub fn shutdown(mut self) -> io::Result<()> {
        self.finish()
    }

    fn finish(&mut self) -> io::Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        let _ = self.sender.send(Command::Shutdown);
        thread
            .join()
            .map_err(|_| io::Error::other("indicator thread panicked"))?
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

enum Command {
    ActivityChanged,
    AttentionStarted(Cadence),
    AttentionEnded,
    SetEnabled(bool, SyncSender<io::Result<()>>),
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Rest,
    ActivitySeparator,
    Busy,
    SettleOff,
    IdleOff,
    IdleOn,
    AttentionWaitingOn,
    Attention,
}

struct Engine<R> {
    policy: Policy,
    renderer: R,
    enabled: bool,
    lit: bool,
    command_active: bool,
    seen_epoch: u64,
    pending_activity_edge: bool,
    pending_activity_pulse: bool,
    idle_blinks_remaining: Option<u32>,
    attention: Option<Cadence>,
    mode: Mode,
    deadline: Option<Instant>,
    last_edge: Option<Instant>,
}

impl<R: IndicatorRenderer> Engine<R> {
    fn new(policy: Policy, renderer: R) -> Self {
        Self {
            policy,
            renderer,
            enabled: false,
            lit: false,
            command_active: false,
            seen_epoch: 0,
            pending_activity_edge: false,
            pending_activity_pulse: false,
            idle_blinks_remaining: Some(0),
            attention: None,
            mode: Mode::Rest,
            deadline: None,
            last_edge: None,
        }
    }

    fn set_enabled(&mut self, enabled: bool, now: Instant) -> io::Result<()> {
        self.enabled = enabled;
        self.pending_activity_edge = false;
        self.pending_activity_pulse = false;
        self.idle_blinks_remaining = Some(0);
        self.attention = None;
        self.mode = Mode::Rest;
        self.deadline = None;
        self.set_lit(false, now)?;
        if enabled {
            self.start_initial_idle(now);
        }
        Ok(())
    }

    fn observe_activity(&mut self, active: bool, epoch: u64, now: Instant) -> io::Result<()> {
        let was_active = self.command_active;
        self.command_active = active;
        let commands_started = epoch.wrapping_sub(self.seen_epoch);
        let activity_started = commands_started != 0;
        self.seen_epoch = epoch;

        if !self.enabled || self.attention.is_some() {
            return Ok(());
        }
        if activity_started {
            let activity_in_progress = self.pending_activity_edge
                || matches!(
                    self.mode,
                    Mode::ActivitySeparator | Mode::Busy | Mode::SettleOff
                );
            if activity_in_progress {
                self.pending_activity_pulse = true;
            } else {
                self.start_activity(now)?;
            }
            if commands_started > 1 {
                self.pending_activity_pulse = true;
            }
            return self.drive_due(now);
        }
        if active {
            if self.mode != Mode::Busy {
                self.mode = Mode::Busy;
                self.deadline = Some(now + self.policy.busy.delay(self.lit));
            }
        } else if was_active && !self.pending_activity_edge && self.mode != Mode::ActivitySeparator
        {
            self.start_idle_after_activity(now)?;
        }
        Ok(())
    }

    fn attention_started(&mut self, cadence: Cadence, now: Instant) -> io::Result<()> {
        self.attention = Some(cadence);
        self.pending_activity_edge = false;
        self.pending_activity_pulse = false;
        if !self.enabled {
            return Ok(());
        }
        if self.lit {
            self.mode = Mode::Attention;
            self.deadline = Some(now + cadence.on);
        } else {
            self.mode = Mode::AttentionWaitingOn;
            self.deadline = Some(self.edge_ready(now));
            self.drive_due(now)?;
        }
        Ok(())
    }

    fn attention_ended(&mut self, now: Instant) -> io::Result<()> {
        self.attention = None;
        if !self.enabled {
            return Ok(());
        }
        if self.command_active {
            self.mode = Mode::Busy;
            self.deadline = Some(now + self.policy.busy.delay(self.lit));
        } else {
            self.start_idle_after_activity(now)?;
        }
        Ok(())
    }

    fn timeout(&mut self, now: Instant) -> io::Result<()> {
        self.drive_due(now)
    }

    fn drive_due(&mut self, now: Instant) -> io::Result<()> {
        while self.enabled && self.deadline.is_some_and(|deadline| deadline <= now) {
            if self.pending_activity_edge && self.attention.is_none() {
                self.set_lit(true, now)?;
                self.pending_activity_edge = false;
                if self.command_active {
                    self.mode = Mode::Busy;
                    self.deadline = Some(self.edge_time(now) + self.policy.busy.delay(self.lit));
                } else {
                    self.start_idle_after_activity(now)?;
                }
                continue;
            }

            match self.mode {
                Mode::ActivitySeparator => {
                    self.set_lit(false, now)?;
                    self.pending_activity_edge = true;
                    self.mode = Mode::Rest;
                    self.deadline = Some(self.edge_ready(now));
                }
                Mode::Busy => {
                    if self.command_active {
                        self.toggle(now)?;
                        self.deadline =
                            Some(self.edge_time(now) + self.policy.busy.delay(self.lit));
                    } else {
                        self.start_idle_after_activity(now)?;
                    }
                }
                Mode::SettleOff => {
                    self.set_lit(false, now)?;
                    if self.pending_activity_pulse {
                        self.start_pending_pulse(now)?;
                    } else {
                        self.start_idle_after_activity(now)?;
                    }
                }
                Mode::IdleOff => {
                    self.set_lit(true, now)?;
                    self.mode = Mode::IdleOn;
                    self.deadline = Some(self.edge_time(now) + self.idle_cadence().on);
                }
                Mode::IdleOn => {
                    self.set_lit(false, now)?;
                    match self.idle_blinks_remaining {
                        None => {
                            self.mode = Mode::IdleOff;
                            self.deadline = Some(self.edge_time(now) + self.idle_cadence().off);
                        }
                        Some(remaining) if remaining > 1 => {
                            self.idle_blinks_remaining = Some(remaining - 1);
                            self.mode = Mode::IdleOff;
                            self.deadline = Some(self.edge_time(now) + self.idle_cadence().off);
                        }
                        Some(_) => {
                            self.idle_blinks_remaining = Some(0);
                            self.mode = Mode::Rest;
                            self.deadline = None;
                        }
                    }
                }
                Mode::AttentionWaitingOn => {
                    let cadence = self.attention.expect("attention cadence");
                    self.set_lit(true, now)?;
                    self.mode = Mode::Attention;
                    self.deadline = Some(self.edge_time(now) + cadence.on);
                }
                Mode::Attention => {
                    let cadence = self.attention.expect("attention cadence");
                    self.toggle(now)?;
                    self.deadline = Some(self.edge_time(now) + cadence.delay(self.lit));
                }
                Mode::Rest => {
                    self.deadline = None;
                }
            }
        }
        Ok(())
    }

    fn start_initial_idle(&mut self, now: Instant) {
        match self.policy.idle {
            IdlePolicy::Blink {
                cadence,
                count: BlinkCount::Forever,
            } => {
                self.idle_blinks_remaining = None;
                self.mode = Mode::IdleOff;
                self.deadline = Some(now + cadence.off);
            }
            IdlePolicy::Off
            | IdlePolicy::Blink {
                count: BlinkCount::Finite(_),
                ..
            } => {
                self.idle_blinks_remaining = Some(0);
                self.mode = Mode::Rest;
                self.deadline = None;
            }
        }
    }

    fn start_idle_after_activity(&mut self, now: Instant) -> io::Result<()> {
        if self.lit {
            self.mode = Mode::SettleOff;
            self.deadline = Some(self.edge_ready(now));
            self.drive_due(now)?;
            return Ok(());
        }

        if self.pending_activity_pulse {
            self.start_pending_pulse(now)?;
            return Ok(());
        }

        match self.policy.idle {
            IdlePolicy::Off => {
                self.mode = Mode::Rest;
                self.deadline = None;
            }
            IdlePolicy::Blink {
                cadence: _,
                count: BlinkCount::Finite(count),
            } => {
                self.idle_blinks_remaining = Some(count);
                self.mode = Mode::IdleOff;
                self.deadline = Some(self.edge_ready(now));
                self.drive_due(now)?;
            }
            IdlePolicy::Blink {
                cadence,
                count: BlinkCount::Forever,
            } => {
                self.idle_blinks_remaining = None;
                self.mode = Mode::IdleOff;
                self.deadline = Some(self.edge_time(now) + cadence.off);
            }
        }
        Ok(())
    }

    fn start_pending_pulse(&mut self, now: Instant) -> io::Result<()> {
        self.pending_activity_pulse = false;
        self.start_activity(now)
    }

    fn start_activity(&mut self, now: Instant) -> io::Result<()> {
        if self.lit {
            self.mode = Mode::ActivitySeparator;
        } else {
            self.pending_activity_edge = true;
            self.mode = Mode::Rest;
        }
        self.deadline = Some(self.edge_ready(now));
        self.drive_due(now)
    }

    fn idle_cadence(&self) -> Cadence {
        let IdlePolicy::Blink { cadence, .. } = self.policy.idle else {
            unreachable!();
        };
        cadence
    }

    fn edge_ready(&self, now: Instant) -> Instant {
        self.last_edge
            .map(|edge| edge + self.policy.minimum_edge)
            .unwrap_or(now)
            .max(now)
    }

    fn edge_time(&self, now: Instant) -> Instant {
        self.last_edge.unwrap_or(now).max(now)
    }

    fn toggle(&mut self, now: Instant) -> io::Result<()> {
        self.set_lit(!self.lit, now)
    }

    fn set_lit(&mut self, lit: bool, now: Instant) -> io::Result<()> {
        if self.lit != lit {
            let edge_started = Instant::now().max(now);
            self.renderer.set_indicator(lit)?;
            self.lit = lit;
            self.last_edge = Some(edge_started);
        }
        Ok(())
    }
}

fn run(
    policy: Policy,
    renderer: impl IndicatorRenderer,
    receiver: Receiver<Command>,
    activity_state: Arc<ActivityState>,
) -> io::Result<()> {
    let mut engine = Engine::new(policy, renderer);
    loop {
        let received = match engine.deadline {
            Some(deadline) => receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map(Some),
            None => receiver
                .recv()
                .map(Some)
                .map_err(|_| RecvTimeoutError::Disconnected),
        };
        let command = match received {
            Ok(Some(command)) => command,
            Ok(None) => unreachable!(),
            Err(RecvTimeoutError::Timeout) => {
                engine.timeout(Instant::now())?;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => Command::Shutdown,
        };
        match command {
            Command::ActivityChanged => {
                activity_state
                    .notification_pending
                    .store(false, Ordering::Release);
                engine.observe_activity(
                    activity_state.command_active.load(Ordering::Acquire),
                    activity_state.command_epoch.load(Ordering::Acquire),
                    Instant::now(),
                )?;
            }
            Command::AttentionStarted(cadence) => {
                engine.attention_started(cadence, Instant::now())?;
            }
            Command::AttentionEnded => engine.attention_ended(Instant::now())?,
            Command::SetEnabled(enabled, response) => {
                let _ = response.send(engine.set_enabled(enabled, Instant::now()));
            }
            Command::Shutdown => return Ok(()),
        }
    }
}

fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "indicator thread stopped")
}

fn validate_policy(policy: Policy) -> io::Result<()> {
    validate_cadence(policy.busy)?;
    match policy.idle {
        IdlePolicy::Off => Ok(()),
        IdlePolicy::Blink {
            cadence,
            count: BlinkCount::Finite(0),
        } => {
            validate_cadence(cadence)?;
            Err(invalid_policy())
        }
        IdlePolicy::Blink { cadence, .. } => validate_cadence(cadence),
    }
}

fn validate_cadence(cadence: Cadence) -> io::Result<()> {
    if cadence.on.is_zero() || cadence.off.is_zero() {
        Err(invalid_policy())
    } else {
        Ok(())
    }
}

fn invalid_policy() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "indicator cadence durations must be nonzero",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct RecordingRenderer(Arc<Mutex<Vec<bool>>>);

    impl IndicatorRenderer for RecordingRenderer {
        fn set_indicator(&mut self, lit: bool) -> io::Result<()> {
            self.0.lock().unwrap().push(lit);
            Ok(())
        }
    }

    fn policy(idle: IdlePolicy) -> Policy {
        Policy::new(
            Cadence::new(Duration::from_millis(67), Duration::from_millis(33)),
            idle,
            Duration::from_millis(8),
        )
    }

    #[test]
    fn completed_short_command_is_followed_by_one_counted_idle_blink() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let idle = Cadence::new(Duration::from_millis(1_500), Duration::from_millis(1_500));
        let mut engine = Engine::new(
            policy(IdlePolicy::Blink {
                cadence: idle,
                count: BlinkCount::Finite(1),
            }),
            renderer,
        );
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 1, start).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true]);
        assert_eq!(engine.mode, Mode::SettleOff);

        let activity_off = engine.deadline.unwrap();
        engine.timeout(activity_off).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false]);

        let idle_on = engine.deadline.unwrap();
        engine.timeout(idle_on).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false, true]);
        let idle_off = engine.deadline.unwrap();
        engine.timeout(idle_off).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false, true, false]);
        assert_eq!(engine.mode, Mode::Rest);
    }

    #[test]
    fn sustained_command_uses_asymmetric_busy_cadence_then_counted_idle() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let idle = Cadence::new(Duration::from_millis(1_500), Duration::from_millis(1_500));
        let mut engine = Engine::new(
            policy(IdlePolicy::Blink {
                cadence: idle,
                count: BlinkCount::Finite(1),
            }),
            renderer,
        );
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(true, 1, start).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true]);

        let off_due = engine.deadline.unwrap();
        engine.timeout(off_due).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
        let on_due = engine.deadline.unwrap();
        engine.timeout(on_due).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false, true]);

        let end = start + Duration::from_millis(110);
        engine.observe_activity(false, 1, end).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false, true, false]);
        let idle_on = engine.deadline.unwrap();
        engine.timeout(idle_on).unwrap();
        assert_eq!(
            *output.lock().unwrap(),
            vec![true, false, true, false, true]
        );
    }

    #[test]
    fn periodic_idle_continues_but_disable_only_forces_indicator_off() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(
            policy(IdlePolicy::Blink {
                cadence: Cadence::new(Duration::from_millis(1_500), Duration::from_millis(1_500)),
                count: BlinkCount::Forever,
            }),
            renderer,
        );
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        let idle_due = engine.deadline.unwrap();
        engine.timeout(idle_due).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true]);

        engine
            .set_enabled(false, start + Duration::from_secs(2))
            .unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
        assert!(!engine.enabled);
    }

    #[test]
    fn activity_burst_retains_only_one_additional_pulse() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(IdlePolicy::Off), renderer);
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 4, start).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true]);
        assert_eq!(engine.seen_epoch, 4);
        assert!(!engine.pending_activity_edge);
        assert!(engine.pending_activity_pulse);

        let first_off = engine.deadline.unwrap();
        engine.timeout(first_off).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
        assert!(engine.pending_activity_edge);
        assert!(!engine.pending_activity_pulse);

        let second_on = engine.deadline.unwrap();
        engine.timeout(second_on).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false, true]);
        let second_off = engine.deadline.unwrap();
        engine.timeout(second_off).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false, true, false]);
        assert_eq!(engine.mode, Mode::Rest);
    }

    #[test]
    fn periodic_idle_short_command_finishes_off_before_idle_restarts() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let idle = Cadence::new(Duration::from_millis(1_500), Duration::from_millis(1_500));
        let mut engine = Engine::new(
            policy(IdlePolicy::Blink {
                cadence: idle,
                count: BlinkCount::Forever,
            }),
            renderer,
        );
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 1, start).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true]);

        let completion = engine.deadline.unwrap();
        engine.timeout(completion).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
        assert_eq!(engine.mode, Mode::IdleOff);
    }

    #[test]
    fn periodic_idle_on_inserts_an_off_separator_before_activity_on() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let idle = Cadence::new(Duration::from_millis(1_500), Duration::from_millis(1_500));
        let mut engine = Engine::new(
            policy(IdlePolicy::Blink {
                cadence: idle,
                count: BlinkCount::Forever,
            }),
            renderer,
        );
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        let idle_on = engine.deadline.unwrap();
        engine.timeout(idle_on).unwrap();
        engine.observe_activity(false, 1, idle_on).unwrap();

        for _ in 0..3 {
            let deadline = engine.deadline.unwrap();
            engine.timeout(deadline).unwrap();
        }
        assert_eq!(*output.lock().unwrap(), vec![true, false, true, false]);
        assert_eq!(engine.mode, Mode::IdleOff);
    }

    #[test]
    fn periodic_idle_burst_retains_only_one_complete_extra_pulse() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let idle = Cadence::new(Duration::from_millis(1_500), Duration::from_millis(1_500));
        let mut engine = Engine::new(
            policy(IdlePolicy::Blink {
                cadence: idle,
                count: BlinkCount::Forever,
            }),
            renderer,
        );
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 8, start).unwrap();

        for _ in 0..3 {
            let deadline = engine.deadline.unwrap();
            engine.timeout(deadline).unwrap();
        }
        assert_eq!(*output.lock().unwrap(), vec![true, false, true, false]);
        assert_eq!(engine.mode, Mode::IdleOff);
    }

    #[test]
    fn activity_during_a_visible_pulse_retains_one_more_pulse() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(IdlePolicy::Off), renderer);
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 1, start).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true]);

        engine
            .observe_activity(false, 2, start + Duration::from_millis(1))
            .unwrap();
        engine
            .observe_activity(false, 8, start + Duration::from_millis(2))
            .unwrap();
        assert!(engine.pending_activity_pulse);

        for _ in 0..3 {
            let deadline = engine.deadline.unwrap();
            engine.timeout(deadline).unwrap();
        }
        assert_eq!(*output.lock().unwrap(), vec![true, false, true, false]);
        assert_eq!(engine.mode, Mode::Rest);
    }

    #[test]
    fn renderer_time_is_part_of_the_minimum_edge_interval() {
        let render_time = Duration::from_millis(12);
        let mut engine = Engine::new(policy(IdlePolicy::Off), move |_| {
            thread::sleep(render_time);
            Ok(())
        });
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 1, start).unwrap();

        assert!(engine.deadline.unwrap() <= Instant::now());
    }

    #[test]
    fn attention_overrides_busy_and_resumes_it() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(IdlePolicy::Off), renderer);
        let start = Instant::now();
        let attention = Cadence::new(Duration::from_millis(384), Duration::from_millis(384));
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(true, 1, start).unwrap();
        engine.attention_started(attention, start).unwrap();
        let attention_due = engine.deadline.unwrap();
        engine.timeout(attention_due).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
        engine
            .attention_ended(start + Duration::from_millis(400))
            .unwrap();
        assert_eq!(engine.mode, Mode::Busy);
    }

    #[test]
    fn controller_preserves_a_command_that_completes_before_its_wake() {
        let (sender, receiver) = mpsc::channel();
        let controller = Controller::start(
            Policy::new(
                Cadence::new(Duration::from_millis(4), Duration::from_millis(2)),
                IdlePolicy::Off,
                Duration::from_millis(1),
            ),
            move |lit| {
                sender.send(lit).unwrap();
                Ok(())
            },
            "indicator-test",
        )
        .unwrap();
        controller.enable().unwrap();

        drop(controller.activity().begin());
        assert!(receiver.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(!receiver.recv_timeout(Duration::from_secs(1)).unwrap());
        controller.shutdown().unwrap();
    }

    #[test]
    fn zero_length_cadences_are_rejected() {
        let policy = Policy::new(
            Cadence::new(Duration::ZERO, Duration::from_millis(1)),
            IdlePolicy::Off,
            Duration::ZERO,
        );
        assert_eq!(
            Controller::start(policy, |_| Ok(()), "invalid")
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

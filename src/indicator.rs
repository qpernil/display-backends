// SPDX-License-Identifier: MIT OR Apache-2.0

//! Device-neutral activity-indicator scheduling.
//!
//! The scheduler owns timing and the logical indicator bit. Callers supply a
//! renderer that decides what that bit means visually and retain responsibility
//! for the lifetime and power state of the complete display.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
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
        if lit { self.on } else { self.off }
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
    /// Minimum off time before activity starts, reusing an existing off edge.
    pub minimum_activity_off: Duration,
    /// Minimum visible on time for completed activity, independent of busy cadence.
    pub minimum_activity_on: Duration,
}

impl Policy {
    pub const fn new(busy: Cadence, idle: IdlePolicy, minimum_edge: Duration) -> Self {
        Self {
            busy,
            idle,
            minimum_edge,
            minimum_activity_off: minimum_edge,
            minimum_activity_on: busy.on,
        }
    }
    pub const fn with_minimum_activity_off(mut self, duration: Duration) -> Self {
        self.minimum_activity_off = duration;
        self
    }

    pub const fn with_minimum_activity_on(mut self, duration: Duration) -> Self {
        self.minimum_activity_on = duration;
        self
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
    Busy,
    SettleOff,
    Recovery,
    IdleOff,
    IdleOn,
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
    idle_blinks_remaining: Option<u32>,
    attention: Option<Cadence>,
    activity_over_attention: bool,
    mode: Mode,
    deadline: Option<Instant>,
    last_edge: Option<Instant>,
    activity_on_until: Option<Instant>,
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
            idle_blinks_remaining: Some(0),
            attention: None,
            activity_over_attention: false,
            mode: Mode::Rest,
            deadline: None,
            last_edge: None,
            activity_on_until: None,
        }
    }

    fn set_enabled(&mut self, enabled: bool, now: Instant) -> io::Result<()> {
        self.enabled = enabled;
        self.pending_activity_edge = false;
        self.idle_blinks_remaining = Some(0);
        self.attention = None;
        self.activity_over_attention = false;
        self.activity_on_until = None;
        self.mode = Mode::Rest;
        self.deadline = None;
        self.set_lit(false, now)?;
        if enabled {
            self.start_initial_idle(now);
        }
        Ok(())
    }

    fn activity_running(&self) -> bool {
        self.command_active && (self.attention.is_none() || self.activity_over_attention)
    }

    fn observe_activity(&mut self, active: bool, epoch: u64, now: Instant) -> io::Result<()> {
        let was_active = self.command_active;
        self.command_active = active;
        let commands_started = epoch.wrapping_sub(self.seen_epoch);
        self.seen_epoch = epoch;
        if !self.enabled {
            return Ok(());
        }
        if commands_started != 0 {
            self.activity_over_attention = true;
            // Coalesce into the current indication without extending its lifetime.
            // Epochs preserve collapsed work, but never count replay pulses.
            if !self.pending_activity_edge && !matches!(self.mode, Mode::Busy | Mode::SettleOff) {
                self.start_activity(now)?;
            } else if active && self.mode == Mode::SettleOff {
                self.mode = Mode::Busy;
                self.deadline = Some(self.last_edge.unwrap_or(now) + self.policy.busy.on);
            }
            if !active && self.mode == Mode::Busy && !self.pending_activity_edge {
                self.start_idle_after_activity(now)?;
            }
            return self.drive_due(now);
        }
        // The command which requested attention is waiting, rather than doing work.
        // A later command epoch may temporarily take over that background pattern.
        if self.attention.is_some() && !self.activity_over_attention {
            return Ok(());
        }
        if !active && was_active && self.mode == Mode::Busy && !self.pending_activity_edge {
            self.start_idle_after_activity(now)?;
        }
        Ok(())
    }

    fn attention_started(&mut self, cadence: Cadence, now: Instant) -> io::Result<()> {
        self.attention = Some(cadence);
        self.activity_over_attention = false;
        if !self.enabled {
            return Ok(());
        }
        if self.pending_activity_edge || self.mode == Mode::SettleOff {
            return Ok(());
        }
        if self.mode == Mode::Busy {
            self.start_idle_after_activity(now)
        } else {
            self.start_recovery(now)
        }
    }

    fn attention_ended(&mut self, now: Instant) -> io::Result<()> {
        self.attention = None;
        self.activity_over_attention = false;
        if !self.enabled {
            return Ok(());
        }
        if self.pending_activity_edge {
            return Ok(());
        }
        if self.command_active {
            if matches!(self.mode, Mode::Busy | Mode::SettleOff) && self.lit {
                self.mode = Mode::Busy;
                self.deadline = Some(self.activity_on_until.unwrap_or(now).max(now));
                self.drive_due(now)
            } else {
                self.start_activity(now)
            }
        } else if matches!(self.mode, Mode::Busy | Mode::SettleOff) {
            self.start_idle_after_activity(now)
        } else {
            self.start_recovery(now)
        }
    }

    fn timeout(&mut self, now: Instant) -> io::Result<()> {
        self.drive_due(now)
    }

    fn drive_due(&mut self, now: Instant) -> io::Result<()> {
        while self.enabled && self.deadline.is_some_and(|deadline| deadline <= now) {
            if self.pending_activity_edge {
                self.set_lit(true, now)?;
                self.pending_activity_edge = false;
                self.activity_on_until =
                    Some(self.edge_time(now) + self.policy.minimum_activity_on);
                self.mode = if self.activity_running() {
                    Mode::Busy
                } else {
                    Mode::SettleOff
                };
                self.deadline = if self.mode == Mode::Busy {
                    Some(self.edge_time(now) + self.policy.busy.on)
                } else {
                    self.activity_on_until
                };
                continue;
            }
            match self.mode {
                Mode::Busy => {
                    if self.activity_running() {
                        self.toggle(now)?;
                        self.activity_on_until = self
                            .lit
                            .then(|| self.edge_time(now) + self.policy.minimum_activity_on);
                        self.deadline =
                            Some(self.edge_time(now) + self.policy.busy.delay(self.lit));
                    } else {
                        self.start_idle_after_activity(now)?;
                    }
                }
                Mode::SettleOff => {
                    self.set_lit(false, now)?;
                    self.activity_on_until = None;
                    self.start_recovery(now)?;
                }
                Mode::Recovery => self.restart_background(now)?,
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
                Mode::Attention => {
                    let cadence = self.attention.expect("attention cadence");
                    self.toggle(now)?;
                    self.deadline = Some(self.edge_time(now) + cadence.delay(self.lit));
                }
                Mode::Rest => self.deadline = None,
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
            _ => {
                self.mode = Mode::Rest;
                self.deadline = None;
            }
        }
    }

    fn start_idle_after_activity(&mut self, now: Instant) -> io::Result<()> {
        if self.lit {
            self.mode = Mode::SettleOff;
            self.deadline = Some(self.activity_on_until.unwrap_or(now).max(now));
            self.drive_due(now)
        } else {
            self.start_recovery(now)
        }
    }

    fn start_recovery(&mut self, now: Instant) -> io::Result<()> {
        self.set_lit(false, now)?;
        self.activity_on_until = None;
        self.activity_over_attention = false;
        self.mode = Mode::Recovery;
        self.deadline = Some(self.edge_time(now) + self.policy.minimum_edge);
        Ok(())
    }

    fn restart_background(&mut self, now: Instant) -> io::Result<()> {
        if let Some(cadence) = self.attention {
            self.set_lit(true, now)?;
            self.mode = Mode::Attention;
            self.deadline = Some(self.edge_time(now) + cadence.on);
            return Ok(());
        }
        match self.policy.idle {
            IdlePolicy::Off => {
                self.mode = Mode::Rest;
                self.deadline = None;
            }
            IdlePolicy::Blink { cadence, count } => {
                self.idle_blinks_remaining = match count {
                    BlinkCount::Finite(n) => Some(n),
                    BlinkCount::Forever => None,
                };
                // Separate a completed activity pulse from idle blinking.
                // A full off phase avoids making short work look like a long
                // on pulse separated only by the minimum edge interval.
                self.set_lit(false, now)?;
                self.mode = Mode::IdleOff;
                self.deadline = Some(now + cadence.off);
            }
        }
        Ok(())
    }

    fn start_activity(&mut self, now: Instant) -> io::Result<()> {
        // Stop the background pattern and ensure a visible off interval. A
        // recent off edge only needs its remaining time; an already-dark LED
        // can start immediately once that minimum has elapsed. The command
        // never waits, and the old idle timer cannot shorten the on phase.
        self.set_lit(false, now)?;
        self.pending_activity_edge = true;
        self.activity_on_until = None;
        self.mode = Mode::Rest;
        self.deadline = Some(
            self.last_edge
                .map_or(now, |edge| edge + self.policy.minimum_activity_off)
                .max(now),
        );
        Ok(())
    }

    fn idle_cadence(&self) -> Cadence {
        let IdlePolicy::Blink { cadence, .. } = self.policy.idle else {
            unreachable!();
        };
        cadence
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
                engine.observe_activity(
                    activity_state.command_active.load(Ordering::Acquire),
                    activity_state.command_epoch.load(Ordering::Acquire),
                    Instant::now(),
                )?;
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
                engine.observe_activity(
                    activity_state.command_active.load(Ordering::Acquire),
                    activity_state.command_epoch.load(Ordering::Acquire),
                    Instant::now(),
                )?;
                engine.attention_started(cadence, Instant::now())?;
            }
            Command::AttentionEnded => {
                engine.observe_activity(
                    activity_state.command_active.load(Ordering::Acquire),
                    activity_state.command_epoch.load(Ordering::Acquire),
                    Instant::now(),
                )?;
                engine.attention_ended(Instant::now())?;
            }
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
    if policy.minimum_activity_off < policy.minimum_edge
        || policy.minimum_activity_on.is_zero()
        || policy.minimum_activity_on > policy.busy.on
    {
        return Err(invalid_policy());
    }
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
        .with_minimum_activity_on(Duration::from_micros(33_500))
    }
    fn idle(count: BlinkCount) -> IdlePolicy {
        IdlePolicy::Blink {
            cadence: Cadence::new(Duration::from_millis(1_500), Duration::from_millis(1_500)),
            count,
        }
    }
    fn advance(engine: &mut Engine<RecordingRenderer>) -> Instant {
        let due = engine.deadline.unwrap();
        engine.timeout(due).unwrap();
        due
    }

    #[test]
    fn completed_command_starts_immediately_when_dark_then_recovers() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(IdlePolicy::Off), renderer);
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 1, start).unwrap();
        assert!(engine.lit);
        let on = engine.last_edge.unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true]);
        assert_eq!(engine.deadline, Some(on + Duration::from_micros(33_500)));
        engine.timeout(on + Duration::from_millis(33)).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true]);
        let off = advance(&mut engine);
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
        assert_eq!(engine.mode, Mode::Recovery);
        assert_eq!(engine.deadline, Some(off + Duration::from_millis(8)));
        advance(&mut engine);
        assert_eq!(engine.mode, Mode::Rest);
        assert!(engine.deadline.is_none());
    }
    #[test]
    fn completion_during_the_on_phase_does_not_truncate_it() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(IdlePolicy::Off), renderer);
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(true, 1, start).unwrap();
        let on = engine.last_edge.unwrap();
        engine
            .observe_activity(false, 1, on + Duration::from_millis(1))
            .unwrap();
        assert_eq!(engine.deadline, Some(on + Duration::from_micros(33_500)));
        advance(&mut engine);
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
    }
    #[test]
    fn sustained_activity_preserves_67_on_33_off_then_resumes_counted_idle() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(idle(BlinkCount::Finite(1))), renderer);
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(true, 1, start).unwrap();
        let on1 = engine.last_edge.unwrap();
        let off = advance(&mut engine);
        assert_eq!(off - on1, Duration::from_millis(67));
        let on2 = advance(&mut engine);
        assert_eq!(on2 - off, Duration::from_millis(33));
        engine
            .observe_activity(false, 1, on2 + Duration::from_millis(10))
            .unwrap();
        let end = advance(&mut engine);
        assert_eq!(end - on2, Duration::from_micros(33_500));
        let recovery_end = advance(&mut engine);
        assert_eq!(recovery_end - end, Duration::from_millis(8));
        assert_eq!(engine.mode, Mode::IdleOff);
        let idle_on = advance(&mut engine);
        assert_eq!(idle_on - recovery_end, Duration::from_millis(1_500));
        let idle_off = advance(&mut engine);
        assert_eq!(idle_off - idle_on, Duration::from_millis(1_500));
        assert_eq!(
            *output.lock().unwrap(),
            vec![true, false, true, false, true, false]
        );
        assert_eq!(engine.mode, Mode::Rest);
    }
    #[test]
    fn activity_interrupts_long_idle_on_and_restarts_it_after_recovery() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(idle(BlinkCount::Forever)), renderer);
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        advance(&mut engine);
        engine
            .observe_activity(false, 1, start + Duration::from_millis(2_000))
            .unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
        let on = advance(&mut engine);
        assert_eq!(on, start + Duration::from_millis(2_008));
        let off = advance(&mut engine);
        assert_eq!(off - on, Duration::from_micros(33_500));
        let recovery_end = advance(&mut engine);
        assert_eq!(recovery_end - off, Duration::from_millis(8));
        assert_eq!(engine.mode, Mode::IdleOff);
        assert_eq!(*output.lock().unwrap(), vec![true, false, true, false]);
        let idle_on = advance(&mut engine);
        assert_eq!(idle_on - recovery_end, Duration::from_millis(1_500));
        assert_eq!(
            *output.lock().unwrap(),
            vec![true, false, true, false, true]
        );
        assert_eq!(engine.mode, Mode::IdleOn);
        assert_eq!(
            engine.deadline,
            Some(idle_on + Duration::from_millis(1_500))
        );
        let idle_off = advance(&mut engine);
        assert_eq!(
            engine.deadline,
            Some(idle_off + Duration::from_millis(1_500))
        );
    }
    #[test]
    fn completed_short_commands_leave_idle_dark_for_its_full_off_phase() {
        for count in [BlinkCount::Finite(1), BlinkCount::Forever] {
            let renderer = RecordingRenderer::default();
            let output = renderer.0.clone();
            let mut engine = Engine::new(policy(idle(count)), renderer);
            let start = Instant::now();
            engine.set_enabled(true, start).unwrap();
            engine.observe_activity(false, 1, start).unwrap();
            let on = engine.last_edge.unwrap();
            let off = advance(&mut engine);
            assert_eq!(off - on, Duration::from_micros(33_500));
            let recovery_end = advance(&mut engine);
            assert_eq!(recovery_end - off, Duration::from_millis(8));
            assert_eq!(engine.mode, Mode::IdleOff);
            for elapsed in [10, 100, 1_499] {
                engine
                    .timeout(recovery_end + Duration::from_millis(elapsed))
                    .unwrap();
                assert!(!engine.lit);
                assert_eq!(*output.lock().unwrap(), vec![true, false]);
            }
            let idle_on = advance(&mut engine);
            assert_eq!(idle_on - recovery_end, Duration::from_millis(1_500));
            assert_eq!(*output.lock().unwrap(), vec![true, false, true]);
            let idle_off = advance(&mut engine);
            assert_eq!(idle_off - idle_on, Duration::from_millis(1_500));
            assert_eq!(
                engine.mode,
                if count == BlinkCount::Finite(1) {
                    Mode::Rest
                } else {
                    Mode::IdleOff
                }
            );
        }
    }

    #[test]
    fn visible_interruption_notch_only_delays_activity_over_a_lit_background() {
        for was_lit in [false, true] {
            let mut engine = Engine::new(
                policy(idle(BlinkCount::Forever))
                    .with_minimum_activity_off(Duration::from_millis(16)),
                RecordingRenderer::default(),
            );
            let start = Instant::now();
            engine.set_enabled(true, start).unwrap();
            let work = if was_lit {
                advance(&mut engine);
                start + Duration::from_millis(2_000)
            } else {
                start + Duration::from_millis(100)
            };
            assert_eq!(engine.lit, was_lit);
            engine.observe_activity(false, 1, work).unwrap();
            let deadline = engine.deadline;
            // Collapsed commands cannot extend the notch or queue extra pulses.
            engine
                .observe_activity(false, 2, work + Duration::from_millis(2))
                .unwrap();
            assert_eq!(engine.deadline, deadline);
            let on = if was_lit {
                assert!(!engine.lit);
                assert_eq!(deadline, Some(work + Duration::from_millis(16)));
                engine
                    .timeout(work + Duration::from_micros(15_999))
                    .unwrap();
                assert!(!engine.lit);
                advance(&mut engine)
            } else {
                assert!(engine.lit);
                assert_eq!(engine.last_edge, Some(work));
                work
            };
            assert!(engine.lit);
            let off = advance(&mut engine);
            assert_eq!(off - on, Duration::from_micros(33_500));
            let recovered = advance(&mut engine);
            assert_eq!(recovered - off, Duration::from_millis(8));
            assert_eq!(engine.mode, Mode::IdleOff);
            assert_eq!(
                engine.deadline,
                Some(recovered + Duration::from_millis(1_500))
            );
        }
    }

    #[test]
    fn activity_reuses_elapsed_off_time_without_losing_the_visible_gap() {
        for elapsed_us in [0, 5_000, 15_999, 16_000, 20_000] {
            let mut engine = Engine::new(
                policy(idle(BlinkCount::Forever))
                    .with_minimum_activity_off(Duration::from_millis(16)),
                RecordingRenderer::default(),
            );
            let start = Instant::now();
            engine.set_enabled(true, start).unwrap();
            advance(&mut engine); // idle on
            let off = advance(&mut engine); // idle off
            let work = off + Duration::from_micros(elapsed_us);
            engine.observe_activity(false, 1, work).unwrap();
            let on = if elapsed_us < 16_000 {
                assert!(!engine.lit);
                assert_eq!(engine.last_edge, Some(off));
                assert_eq!(engine.deadline, Some(off + Duration::from_millis(16)));
                advance(&mut engine)
            } else {
                assert!(engine.lit);
                assert_eq!(engine.last_edge, Some(work));
                work
            };
            assert_eq!(on, (off + Duration::from_millis(16)).max(work));
            assert_eq!(engine.deadline, Some(on + Duration::from_micros(33_500)));
        }
    }

    #[test]
    fn minimum_activity_off_cannot_be_shorter_than_the_minimum_edge() {
        let invalid = policy(IdlePolicy::Off).with_minimum_activity_off(Duration::from_millis(7));
        assert!(validate_policy(invalid).is_err());
        assert!(validate_policy(policy(IdlePolicy::Off)).is_ok());
    }

    #[test]
    fn activity_interrupts_idle_off_immediately() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(idle(BlinkCount::Forever)), renderer);
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine
            .observe_activity(false, 1, start + Duration::from_millis(100))
            .unwrap();
        assert!(engine.lit);
        assert_eq!(engine.last_edge, Some(start + Duration::from_millis(100)));
        assert_eq!(
            engine.deadline,
            Some(start + Duration::from_micros(133_500))
        );
        assert_eq!(*output.lock().unwrap(), vec![true]);
    }
    #[test]
    fn bursts_coalesce_without_extending_or_replaying_the_pulse() {
        for base in [IdlePolicy::Off, idle(BlinkCount::Forever)] {
            let renderer = RecordingRenderer::default();
            let output = renderer.0.clone();
            let mut engine = Engine::new(policy(base), renderer);
            let start = Instant::now();
            engine.set_enabled(true, start).unwrap();
            engine.observe_activity(false, 4, start).unwrap();
            let on = engine.last_edge.unwrap();
            let deadline = engine.deadline;
            for epoch in 5..100 {
                engine
                    .observe_activity(false, epoch, on + Duration::from_millis(2))
                    .unwrap();
                assert_eq!(engine.deadline, deadline);
            }
            let off = advance(&mut engine);
            assert_eq!(off - on, Duration::from_micros(33_500));
            assert_eq!(*output.lock().unwrap(), vec![true, false]);
            advance(&mut engine);
            assert_eq!(
                engine.mode,
                if base == IdlePolicy::Off {
                    Mode::Rest
                } else {
                    Mode::IdleOff
                }
            );
        }
    }
    #[test]
    fn completion_after_minimum_ends_immediately() {
        let mut engine = Engine::new(policy(IdlePolicy::Off), RecordingRenderer::default());
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(true, 1, start).unwrap();
        let on = engine.last_edge.unwrap();
        let end = on + Duration::from_millis(40);
        engine.observe_activity(false, 1, end).unwrap();
        assert!(!engine.lit);
        assert_eq!(engine.mode, Mode::Recovery);
        assert_eq!(engine.deadline, Some(end + Duration::from_millis(8)));
    }
    #[test]
    fn new_work_during_minimum_resumes_busy_without_delayed_replay() {
        let mut engine = Engine::new(policy(IdlePolicy::Off), RecordingRenderer::default());
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 1, start).unwrap();
        let on = engine.last_edge.unwrap();
        engine
            .observe_activity(true, 2, on + Duration::from_millis(10))
            .unwrap();
        assert_eq!(engine.mode, Mode::Busy);
        assert_eq!(engine.deadline, Some(on + Duration::from_millis(67)));
        engine
            .observe_activity(false, 2, on + Duration::from_millis(20))
            .unwrap();
        assert_eq!(engine.deadline, Some(on + Duration::from_micros(33_500)));
        advance(&mut engine);
        advance(&mut engine);
        assert_eq!(engine.mode, Mode::Rest);
    }
    #[test]
    fn touch_wait_replaces_busy_after_its_minimum_phase_and_work_can_interrupt_touch() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(IdlePolicy::Off), renderer);
        let start = Instant::now();
        let touch = Cadence::new(Duration::from_millis(384), Duration::from_millis(384));
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(true, 1, start).unwrap();
        engine.attention_started(touch, start).unwrap();
        let on1 = engine.last_edge.unwrap();
        let off1 = advance(&mut engine);
        assert_eq!(off1 - on1, Duration::from_micros(33_500));
        let touch_on = advance(&mut engine);
        assert_eq!(touch_on - off1, Duration::from_millis(8));
        assert_eq!(engine.mode, Mode::Attention);
        assert_eq!(engine.deadline, Some(touch_on + touch.on));
        // A later command has already completed while the touch pattern is lit.
        let work = touch_on + Duration::from_millis(20);
        engine.observe_activity(false, 2, work).unwrap();
        assert!(!engine.lit);
        let on2 = advance(&mut engine);
        assert_eq!(on2 - work, Duration::from_millis(8));
        let off2 = advance(&mut engine);
        assert_eq!(off2 - on2, Duration::from_micros(33_500));
        let resume = advance(&mut engine);
        assert_eq!(resume - off2, Duration::from_millis(8));
        assert_eq!(engine.mode, Mode::Attention);
        assert_eq!(engine.deadline, Some(resume + touch.on));
        let touch_off = advance(&mut engine);
        assert_eq!(touch_off - resume, touch.on);
        assert_eq!(engine.deadline, Some(touch_off + touch.off));
        assert_eq!(
            *output.lock().unwrap(),
            vec![true, false, true, false, true, false, true, false]
        );
    }
    #[test]
    fn ending_touch_restarts_work_with_an_off_boundary() {
        let renderer = RecordingRenderer::default();
        let mut engine = Engine::new(policy(IdlePolicy::Off), renderer);
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(true, 1, start).unwrap();
        engine
            .attention_started(
                Cadence::new(Duration::from_millis(384), Duration::from_millis(384)),
                start,
            )
            .unwrap();
        for _ in 0..2 {
            advance(&mut engine);
        }
        assert!(engine.lit);
        let end = engine.last_edge.unwrap() + Duration::from_millis(20);
        engine.attention_ended(end).unwrap();
        assert!(!engine.lit);
        assert_eq!(engine.deadline, Some(end + Duration::from_millis(8)));
        advance(&mut engine);
        assert_eq!(engine.mode, Mode::Busy);
    }
    #[test]
    fn disable_cancels_activity_and_prevents_later_replay() {
        let renderer = RecordingRenderer::default();
        let output = renderer.0.clone();
        let mut engine = Engine::new(policy(IdlePolicy::Off), renderer);
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 1, start).unwrap();
        let on = engine.last_edge.unwrap();
        engine
            .set_enabled(false, on + Duration::from_millis(1))
            .unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
        assert!(engine.deadline.is_none());
        engine.timeout(on + Duration::from_secs(1)).unwrap();
        assert_eq!(*output.lock().unwrap(), vec![true, false]);
    }
    #[test]
    fn rendering_time_counts_toward_the_on_phase() {
        let mut engine = Engine::new(policy(IdlePolicy::Off), |_| {
            thread::sleep(Duration::from_millis(80));
            Ok(())
        });
        let start = Instant::now();
        engine.set_enabled(true, start).unwrap();
        engine.observe_activity(false, 1, start).unwrap();
        assert!(engine.lit);
        assert!(engine.deadline.unwrap() <= Instant::now());
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
    fn command_producer_does_not_wait_for_a_blocked_renderer() {
        let (entered, observed) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let controller = Controller::start(
            policy(IdlePolicy::Off),
            move |lit| {
                if lit {
                    entered.send(()).unwrap();
                    blocked.recv().unwrap();
                }
                Ok(())
            },
            "blocked-indicator-test",
        )
        .unwrap();
        controller.enable().unwrap();
        let activity = controller.activity();
        drop(activity.begin());
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        let (finished, done) = mpsc::channel();
        let producer = thread::spawn(move || {
            for _ in 0..100 {
                drop(activity.begin());
            }
            finished.send(()).unwrap();
        });
        let result = done.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        producer.join().unwrap();
        controller.shutdown().unwrap();
        assert!(result.is_ok());
    }
    #[test]
    fn zero_length_cadences_are_rejected() {
        assert!(
            validate_policy(Policy::new(
                Cadence::new(Duration::ZERO, Duration::from_millis(1)),
                IdlePolicy::Off,
                Duration::from_millis(1)
            ))
            .is_err()
        );
        assert!(validate_policy(policy(idle(BlinkCount::Finite(0)))).is_err());
        assert!(
            validate_policy(policy(IdlePolicy::Off).with_minimum_activity_on(Duration::ZERO))
                .is_err()
        );
        assert!(
            validate_policy(
                policy(IdlePolicy::Off).with_minimum_activity_on(Duration::from_millis(68))
            )
            .is_err()
        );
    }
}

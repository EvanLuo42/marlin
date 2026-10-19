//! Evaluating mapping contexts for one input user (0019 §9.1): a stack of
//! [`CompiledContext`]s by priority, read from one [`InputFrame`] a frame, giving each action's
//! [`ActionState`].
//!
//! For each context, highest priority first, each mapping reads its source (unless a higher
//! context consumed it), applies its modifiers and checks its triggers: the explicit ones
//! (`Down`, `Pressed`, `Released`, `Hold`, `Tap`; `Down` when there are none) must one of them
//! fire, the chords must all hold. An action fires when any of its mappings fires; its value is
//! the sum of theirs (a button action's is 1). A mapping that fires consumes its source for
//! the contexts below it, and for whoever reads the frame after this user (the next routing
//! layer). Chords are checked after every context has been read, so a chorded mapping consumes
//! its source for the next layer but not for lower contexts of the same user.

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_asset::Uuid;
use marlin_containers::{HashMap, HashMapExt};
use marlin_math::Vec3;

use crate::asset::{ActionValueType, MappingContext, Modifier, SwizzleOrder, Trigger};
use crate::source::{InputFrame, InputSource};

#[derive(Clone, Copy, PartialEq, Debug)]
enum CompiledTrigger {
    Down,
    Pressed,
    Released,
    Hold(f32),
    Tap(f32),
}

#[derive(Clone, PartialEq, Debug)]
struct CompiledMapping {
    action: Uuid,
    value_type: ActionValueType,
    source: InputSource,
    modifiers: Vec<Modifier>,
    triggers: Vec<CompiledTrigger>,
    chords: Vec<Uuid>,
}

/// A mapping context ready to evaluate: its actions resolved to ids and value types.
#[derive(Clone, PartialEq, Debug)]
pub struct CompiledContext {
    pub id: Uuid,
    mappings: Vec<CompiledMapping>,
}

impl CompiledContext {
    /// `context` (the asset `id`), with each action's value type from `value_type` (`Bool` for
    /// an action it does not know).
    pub fn compile(
        id: Uuid,
        context: &MappingContext,
        value_type: impl Fn(Uuid) -> Option<ActionValueType>,
    ) -> Self {
        let mappings = context
            .mappings
            .iter()
            .map(|m| {
                let mut triggers = Vec::new();
                let mut chords = Vec::new();
                for t in &m.triggers {
                    match *t {
                        Trigger::Down => triggers.push(CompiledTrigger::Down),
                        Trigger::Pressed => triggers.push(CompiledTrigger::Pressed),
                        Trigger::Released => triggers.push(CompiledTrigger::Released),
                        Trigger::Hold { seconds } => triggers.push(CompiledTrigger::Hold(seconds)),
                        Trigger::Tap { seconds } => triggers.push(CompiledTrigger::Tap(seconds)),
                        Trigger::Chord(action) => chords.push(action.target),
                    }
                }
                if triggers.is_empty() {
                    triggers.push(CompiledTrigger::Down);
                }
                CompiledMapping {
                    action: m.action.target,
                    value_type: value_type(m.action.target).unwrap_or_default(),
                    source: m.source,
                    modifiers: m.modifiers.clone(),
                    triggers,
                    chords,
                }
            })
            .collect();
        Self { id, mappings }
    }

    /// The actions its mappings fire.
    pub fn actions(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.mappings.iter().map(|m| m.action)
    }
}

/// An action this frame.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct ActionState {
    /// The sum of its firing mappings' values (a button action: x = 1); zero when not fired.
    pub value: Vec3,
    /// Fires this frame.
    pub triggered: bool,
    /// Fires this frame and did not the last.
    pub started: bool,
    /// Fired the last frame and does not this one.
    pub completed: bool,
}

impl ActionState {
    pub fn axis1(&self) -> f32 {
        self.value.x
    }

    pub fn axis2(&self) -> [f32; 2] {
        [self.value.x, self.value.y]
    }
}

#[derive(Clone, Copy, Default)]
struct MappingState {
    actuated: bool,
    held: f32,
}

struct ActiveContext {
    context: Arc<CompiledContext>,
    priority: i32,
    states: Vec<MappingState>,
}

/// One player's (or the host's) context stack and action states.
#[derive(Default)]
pub struct InputUser {
    contexts: Vec<ActiveContext>,
    actions: HashMap<Uuid, ActionState>,
}

fn apply(modifiers: &[Modifier], mut v: Vec3, dt: f32) -> Vec3 {
    for m in modifiers {
        v = match *m {
            Modifier::Negate => -v,
            Modifier::Swizzle(order) => match order {
                SwizzleOrder::Yxz => Vec3::new(v.y, v.x, v.z),
                SwizzleOrder::Zyx => Vec3::new(v.z, v.y, v.x),
                SwizzleOrder::Xzy => Vec3::new(v.x, v.z, v.y),
                SwizzleOrder::Yzx => Vec3::new(v.y, v.z, v.x),
                SwizzleOrder::Zxy => Vec3::new(v.z, v.x, v.y),
            },
            Modifier::Scale { factor } => v * factor,
            Modifier::ScaleByDeltaTime => v * dt,
            Modifier::DeadZone { lower, upper } => {
                let len = v.length();
                if len <= lower {
                    Vec3::ZERO
                } else {
                    let span = (upper - lower).max(1e-6);
                    v * (((len - lower) / span).min(1.0) / len)
                }
            }
        };
    }
    v
}

/// One mapping's result this frame, before chords.
struct Hit {
    action: Uuid,
    value_type: ActionValueType,
    value: Vec3,
    fired: bool,
    source: InputSource,
}

impl InputUser {
    pub fn new() -> Self {
        Self {
            contexts: Vec::new(),
            actions: HashMap::new(),
        }
    }

    /// Adds `context` at `priority` (higher first); adding one already there changes its
    /// priority.
    pub fn add_context(&mut self, context: Arc<CompiledContext>, priority: i32) {
        self.remove_context(context.id);
        let states = alloc::vec![MappingState::default(); context.mappings.len()];
        self.contexts.push(ActiveContext {
            context,
            priority,
            states,
        });
        // Stable: equal priorities keep the order they were added in.
        self.contexts
            .sort_by_key(|c| core::cmp::Reverse(c.priority));
    }

    pub fn remove_context(&mut self, id: Uuid) -> bool {
        let before = self.contexts.len();
        self.contexts.retain(|c| c.context.id != id);
        self.contexts.len() != before
    }

    pub fn has_context(&self, id: Uuid) -> bool {
        self.contexts.iter().any(|c| c.context.id == id)
    }

    /// The action `id` this frame (all zero if it never fired).
    pub fn action(&self, id: Uuid) -> ActionState {
        self.actions.get(&id).copied().unwrap_or_default()
    }

    /// Evaluates the contexts on `frame`, `dt` seconds after the last; returns the sources the
    /// firing mappings consumed (for the next routing layer).
    pub fn evaluate(&mut self, frame: &InputFrame, dt: f32) -> Vec<InputSource> {
        let mut frame = frame.clone();
        let mut hits: Vec<Hit> = Vec::new();
        let mut chords: Vec<(usize, Uuid)> = Vec::new();
        for active in &mut self.contexts {
            let first = hits.len();
            for (m, state) in active.context.mappings.iter().zip(&mut active.states) {
                let raw = frame.read(&m.source);
                let actuated = raw.length_squared() > 0.0;
                let (was, held_before) = (state.actuated, state.held);
                state.actuated = actuated;
                state.held = if actuated { held_before + dt } else { 0.0 };
                let fired = m.triggers.iter().any(|t| match *t {
                    CompiledTrigger::Down => actuated,
                    CompiledTrigger::Pressed => actuated && !was,
                    CompiledTrigger::Released => !actuated && was,
                    CompiledTrigger::Hold(s) => actuated && state.held >= s,
                    CompiledTrigger::Tap(s) => !actuated && was && held_before <= s,
                });
                for &c in &m.chords {
                    chords.push((hits.len(), c));
                }
                hits.push(Hit {
                    action: m.action,
                    value_type: m.value_type,
                    value: apply(&m.modifiers, raw, dt),
                    fired,
                    source: m.source,
                });
            }
            // What fired here is gone for the contexts below.
            for hit in &hits[first..] {
                if hit.fired {
                    frame.consume(&hit.source);
                }
            }
        }
        // Chords: the chord action must fire (by its own mappings' explicit triggers).
        let explicit: Vec<Uuid> = hits.iter().filter(|h| h.fired).map(|h| h.action).collect();
        for (i, chord) in chords {
            if !explicit.contains(&chord) {
                hits[i].fired = false;
            }
        }

        let mut next: HashMap<Uuid, ActionState> = HashMap::new();
        for hit in &hits {
            let state = next.entry(hit.action).or_default();
            if !hit.fired {
                continue;
            }
            state.triggered = true;
            state.value = match hit.value_type {
                ActionValueType::Bool => Vec3::new(1.0, 0.0, 0.0),
                _ => state.value + hit.value,
            };
        }
        // Actions no context maps any more complete once.
        for (&id, last) in &self.actions {
            next.entry(id).or_insert_with(|| ActionState {
                completed: last.triggered,
                ..ActionState::default()
            });
        }
        for (id, state) in &mut next {
            let was = self.actions.get(id).is_some_and(|s| s.triggered);
            state.started = state.triggered && !was;
            state.completed |= !state.triggered && was;
        }
        self.actions = next;
        hits.into_iter()
            .filter(|h| h.fired)
            .map(|h| h.source)
            .collect()
    }
}

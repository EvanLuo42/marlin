//! Input in worlds (0019 §9): local [`Players`] with their context stacks and action states,
//! the game's [`InputSettings`], and [`InputModule`], which loads the contexts players ask for
//! and evaluates them at the start of each frame.

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_asset::{AssetError, AssetServer, Handle, LoadState, Uuid, Vfs};
use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::IntoSystem;
use marlin_jobs::JobSystem;
use marlin_world::{EngineContext, Module, Registry, TickGroup, Time, World, WorldModule};

use crate::asset::{InputAction, MappingContext};
use crate::eval::{ActionState, CompiledContext, InputUser};
use crate::source::InputFrame;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Request {
    Add(Uuid, i32),
    Remove(Uuid),
}

/// A local player: the raw input the host routed to it this frame, and its contexts and
/// actions.
#[derive(Default)]
pub struct Player {
    pub user: InputUser,
    frame: InputFrame,
    requests: Vec<Request>,
}

impl Player {
    /// The action `id` this frame.
    pub fn action(&self, id: Uuid) -> ActionState {
        self.user.action(id)
    }

    /// Adds the mapping context asset `id` at `priority` (higher first), once it has loaded
    /// (usually by the next frame).
    pub fn add_context(&mut self, id: Uuid, priority: i32) {
        self.requests.push(Request::Add(id, priority));
    }

    pub fn remove_context(&mut self, id: Uuid) {
        self.requests.push(Request::Remove(id));
    }

    /// Contexts asked for that have not loaded yet.
    pub fn pending_contexts(&self) -> usize {
        self.requests.len()
    }
}

/// A world's local players (a world resource). The host routes each player's input into it
/// before the world's frame ([`set_input`](Self::set_input)); `FrameStart` evaluates them.
pub struct Players {
    players: Vec<Player>,
}

impl Default for Players {
    fn default() -> Self {
        Self {
            players: alloc::vec![Player::default()],
        }
    }
}

impl Players {
    pub fn count(&self) -> usize {
        self.players.len()
    }

    /// Makes room for `count` players (never fewer than there are).
    pub fn ensure(&mut self, count: usize) {
        while self.players.len() < count {
            self.players.push(Player::default());
        }
    }

    pub fn player(&self, index: usize) -> Option<&Player> {
        self.players.get(index)
    }

    pub fn player_mut(&mut self, index: usize) -> Option<&mut Player> {
        self.players.get_mut(index)
    }

    /// The raw input routed to player `index` this frame (none routed: nothing pressed).
    pub fn set_input(&mut self, index: usize, frame: InputFrame) {
        self.ensure(index + 1);
        self.players[index].frame = frame;
    }
}

/// Where the game wants input to go (0019 §9.3): to gameplay, to UI, or both.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InputMode {
    /// Gameplay gets everything; UI nothing.
    GameOnly,
    /// UI gets everything (a menu is open); gameplay nothing.
    UiOnly,
    /// UI gets what the pointer is over and text while a field has focus; gameplay the rest.
    #[default]
    GameAndUi,
}

/// What the game wants of the mouse cursor while its viewport has the input.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorMode {
    #[default]
    Visible,
    Hidden,
    /// Hidden and held in place; motion still reads (a first-person camera).
    Locked,
}

/// The game's input settings (a world resource, set by gameplay; the host follows them).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct InputSettings {
    pub mode: InputMode,
    pub cursor: CursorMode,
}

/// `FrameStart`: each player's contexts evaluated on the input routed to it.
pub fn evaluate_players(world: &mut marlin_ecs::World) {
    // Menus read input while the game is paused: the wall-clock step.
    let dt = world.resource::<Time>().map_or(0.0, Time::unscaled_delta);
    let Some(players) = world.resource_mut::<Players>() else {
        return;
    };
    for p in &mut players.players {
        let frame = core::mem::take(&mut p.frame);
        p.user.evaluate(&frame, dt);
    }
}

/// Compiles the loaded context `id` (`handle`), loading its actions through `assets`; `None`
/// while something is still loading.
fn compile(
    assets: &mut AssetServer,
    id: Uuid,
    handle: &Handle<MappingContext>,
) -> Option<Result<CompiledContext, AssetError>> {
    match assets.state(handle) {
        LoadState::Loading => return None,
        LoadState::Failed(e) => return Some(Err(e)),
        LoadState::Loaded => {}
    }
    let context = assets.get(handle)?.clone();
    let mut actions = HashMap::new();
    for m in &context.mappings {
        let ids =
            core::iter::once(m.action.target).chain(m.triggers.iter().filter_map(|t| match t {
                crate::asset::Trigger::Chord(a) => Some(a.target),
                _ => None,
            }));
        for a in ids {
            let h: Handle<InputAction> = assets.load(a);
            match assets.state(&h) {
                LoadState::Loading => return None,
                LoadState::Failed(e) => {
                    marlin_diag::warn!("input action {a}: {e:?}; read as a button");
                }
                LoadState::Loaded => {
                    if let Some(action) = assets.get(&h) {
                        actions.insert(a, action.value);
                    }
                }
            }
        }
    }
    Some(Ok(CompiledContext::compile(id, &context, |a| {
        actions.get(&a).copied()
    })))
}

/// Loads the mapping context named `name` and its actions, blocking: what hosts start with.
pub fn load_context(
    assets: &mut AssetServer,
    vfs: &Vfs,
    jobs: Option<&JobSystem>,
    name: &str,
) -> Result<Arc<CompiledContext>, AssetError> {
    let id = vfs.named(name).ok_or(AssetError::NotFound)?;
    let handle = assets.load::<MappingContext>(id);
    loop {
        assets.update_blocking(vfs, jobs);
        if let Some(result) = compile(assets, id, &handle) {
            return result.map(Arc::new);
        }
    }
}

/// The input module: actions and contexts load, players evaluate them.
#[derive(Default)]
pub struct InputModule {
    /// Contexts loading or compiled, by asset.
    contexts: HashMap<Uuid, (Handle<MappingContext>, Option<Arc<CompiledContext>>)>,
}

impl InputModule {
    pub const NAME: &'static str = "Input";

    pub fn new() -> Self {
        Self::default()
    }
}

impl Module for InputModule {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn dependencies(&self) -> &'static [&'static str] {
        &[WorldModule::NAME]
    }

    fn register(&self, r: &mut Registry) {
        r.add_loaders(|loaders, _| {
            loaders.register_serial::<InputAction>();
            loaders.register_serial::<MappingContext>();
        });
        r.add_system(TickGroup::FrameStart, || {
            (evaluate_players as fn(&mut marlin_ecs::World)).named("evaluate_players")
        });
    }

    fn init_world(&mut self, world: &mut World, _: &mut EngineContext<'_, '_>) {
        world.ecs_mut().insert_resource(Players::default());
        world.ecs_mut().insert_resource(InputSettings::default());
    }

    fn pre_world_frame(&mut self, world: &mut World, cx: &mut EngineContext<'_, '_>) {
        let Some(mut players) = world.ecs_mut().remove_resource::<Players>() else {
            return;
        };
        for p in &mut players.players {
            let mut pending = Vec::new();
            for request in core::mem::take(&mut p.requests) {
                match request {
                    Request::Remove(id) => {
                        p.user.remove_context(id);
                    }
                    Request::Add(id, priority) => {
                        let (handle, compiled) = self
                            .contexts
                            .entry(id)
                            .or_insert_with(|| (cx.assets.load(id), None));
                        if compiled.is_none() {
                            match compile(cx.assets, id, handle) {
                                None => {}
                                Some(Ok(c)) => *compiled = Some(Arc::new(c)),
                                Some(Err(e)) => {
                                    marlin_diag::warn!("mapping context {id}: {e:?}");
                                    continue;
                                }
                            }
                        }
                        match compiled {
                            Some(c) => p.user.add_context(c.clone(), priority),
                            None => pending.push(request),
                        }
                    }
                }
            }
            p.requests = pending;
        }
        world.ecs_mut().insert_resource(players);
    }

    fn world_ready(&self, world: &World) -> bool {
        world
            .ecs()
            .resource::<Players>()
            .is_none_or(|p| p.players.iter().all(|p| p.requests.is_empty()))
    }
}

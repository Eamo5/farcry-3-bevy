//! The game's interface in one player: the shim (`shim.as`, the engine's
//! side of the movies) at level 0, the movies loaded over it from the cache,
//! and the host's side of their conversation (what they ask of the engine,
//! the events it sends them, the clips it drives by DPS id).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use ruffle_core::backend::navigator::{
    ErrorResponse, NavigationMethod, NavigatorBackend, NullExecutor, NullNavigatorBackend, OwnedFuture, Request, SuccessResponse,
};
use ruffle_core::external::{ExternalInterfaceProvider, Value as ExternalValue};
use ruffle_core::indexmap::IndexMap;
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::socket::{SocketAction, SocketHandle};
use ruffle_core::{Player, PlayerBuilder, StageScaleMode};
use ruffle_render::backend::RenderBackend;
use url::{ParseError, Url};

/// The game's string table in the cache (the converter's copy of the
/// language's oasis strings).
const STRINGS_REL: &str = "story/strings.bin";

/// What a movie asked of the engine (its `FSCommand2`): the command
/// (`Send`, `Get`, `GetPlatform`...) and its arguments as text.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineCall {
    pub command: String,
    pub args: Vec<String>,
}

/// The host's side of `ExternalInterface`: calls the movies make, and the
/// answers to the engine's `Get`s.
#[derive(Default)]
struct Bridge {
    calls: RefCell<Vec<EngineCall>>,
    /// `Get` answers by key ("IsHighDefinition" -> "1"), and `GetPlatform`'s
    /// under "Platform".
    answers: RefCell<HashMap<String, String>>,
    /// The game's strings, for `fire.utils.Localizer.getText(id)`.
    strings: fc3_formats::oasis::Strings,
    /// The sounds the movies started, by name (`engineSound`, the shim's
    /// `Sound.start`).
    sounds: RefCell<Vec<String>>,
}

struct Provider(Rc<Bridge>);

/// A string for the movies: a blank one as null (Flash, and Ruffle after
/// it, hands a SWF 8 movie a blank string from the host as "null"; the shim
/// takes null as empty).
pub fn string(v: &str) -> ExternalValue {
    if v.trim().is_empty() {
        ExternalValue::Null
    } else {
        ExternalValue::String(v.to_string())
    }
}

fn text(v: &ExternalValue) -> String {
    match v {
        ExternalValue::String(s) => s.clone(),
        ExternalValue::Number(n) => n.to_string(),
        ExternalValue::Bool(b) => b.to_string(),
        ExternalValue::Undefined | ExternalValue::Null => String::new(),
        other => format!("{other:?}"),
    }
}

impl ExternalInterfaceProvider for Provider {
    fn call_method(&self, _context: &mut ruffle_core::context::UpdateContext<'_>, name: &str, args: &[ExternalValue]) -> ExternalValue {
        if name == "engineText" {
            let id = args.first().map(text).and_then(|t| t.trim().parse::<u32>().ok());
            return string(id.and_then(|id| self.0.strings.get(id)).unwrap_or_default());
        }
        if name == "engineSound" {
            if let Some(n) = args.first().map(text).filter(|n| !n.is_empty()) {
                self.0.sounds.borrow_mut().push(n);
            }
            return ExternalValue::Undefined;
        }
        if name != "engineCall" {
            return ExternalValue::Undefined;
        }
        let command = args.first().map(text).unwrap_or_default();
        let list: Vec<String> = match args.get(1) {
            Some(ExternalValue::List(l)) => l.iter().map(text).collect(),
            _ => Vec::new(),
        };
        let answer = match command.as_str() {
            "Get" => list.first().and_then(|k| self.0.answers.borrow().get(k).cloned()),
            "GetPlatform" => self.0.answers.borrow().get("Platform").cloned(),
            _ => None,
        };
        self.0.calls.borrow_mut().push(EngineCall { command, args: list });
        answer.map_or(ExternalValue::Undefined, ExternalValue::String)
    }

    fn on_callback_available(&self, _name: &str) {}

    fn get_id(&self) -> Option<String> {
        None
    }
}

/// Fetches from the cache: a movie names another (`ImportAssets`,
/// `loadMovie`) by its archive path (`UI\Common\lib.feu`), which
/// `fc3_formats::feu::movie_rel` maps to the cached SWF. A Bink video loaded
/// into a clip (the frontend's backdrop; the engine drew it there) loads an
/// empty movie (`EMPTY_REL`), so the clip's stand-in goes and what the game
/// shows under the interface shows through.
struct CacheNavigator {
    inner: NullNavigatorBackend,
    root: PathBuf,
}

/// An empty movie (SWF 8, no stage, one frame), kept in the cache.
const EMPTY_REL: &str = "ui/empty.swf";
const EMPTY_SWF: [u8; 17] = [b'F', b'W', b'S', 8, 17, 0, 0, 0, 0, 0, 24, 1, 0, 0x40, 0, 0, 0];

impl CacheNavigator {
    fn map(&self, url: &str) -> Option<Url> {
        if Url::parse(url).is_ok() {
            return None;
        }
        if url.to_ascii_lowercase().ends_with(".bik") {
            return Url::from_file_path(self.root.join(EMPTY_REL)).ok();
        }
        Url::from_file_path(self.root.join(fc3_formats::feu::movie_rel(url))).ok()
    }
}

impl NavigatorBackend for CacheNavigator {
    fn navigate_to_url(&self, url: &str, target: &str, vars: Option<(NavigationMethod, IndexMap<String, String>)>) {
        self.inner.navigate_to_url(url, target, vars);
    }

    fn fetch(&self, request: Request) -> OwnedFuture<Box<dyn SuccessResponse>, ErrorResponse> {
        match self.map(request.url()) {
            Some(url) => self.inner.fetch(Request::get(url.to_string())),
            None => self.inner.fetch(request),
        }
    }

    fn resolve_url(&self, url: &str) -> Result<Url, ParseError> {
        match self.map(url) {
            Some(u) => Ok(u),
            None => self.inner.resolve_url(url),
        }
    }

    fn spawn_future(&mut self, future: OwnedFuture<(), ruffle_core::loader::Error>) {
        self.inner.spawn_future(future);
    }

    fn pre_process_url(&self, url: Url) -> Url {
        url
    }

    fn connect_socket(
        &mut self,
        host: String,
        port: u16,
        timeout: Duration,
        handle: SocketHandle,
        receiver: async_channel::Receiver<Vec<u8>>,
        sender: async_channel::Sender<SocketAction>,
    ) {
        self.inner.connect_socket(host, port, timeout, handle, receiver, sender);
    }
}

/// The shim compiled to a movie of a size.
fn shim_swf(width: u32, height: u32) -> Result<Vec<u8>> {
    struct Source;
    impl rascal::SourceProvider for Source {
        fn load(&self, path: &str) -> Result<String, std::io::Error> {
            match path {
                "shim.as" => Ok(include_str!("shim.as").to_string()),
                _ => Err(std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string())),
            }
        }
        fn is_file(&self, path: &str) -> bool {
            path == "shim.as"
        }
    }
    let mut builder = rascal::ProgramBuilder::new(Source).with_compile_options(rascal::CompileOptions::default().with_swf_version(8));
    builder.add_script("shim.as");
    let program = builder.build().map_err(|e| anyhow!("the shim doesn't compile: {e:?}"))?;
    let options = rascal::SwfOptions::default().with_frame_rate(30.0).with_stage_size(0.0, 0.0, width as f64, height as f64);
    program.compile().to_swf(&options).map_err(|e| anyhow!("the shim's movie: {e}"))
}

/// The size the movies are laid out for (the engine's virtual screen): the
/// stage is this, scaled to the window whole.
pub const STAGE: (u32, u32) = (1280, 720);

/// The interface player.
pub struct Host {
    player: Arc<Mutex<Player>>,
    bridge: Rc<Bridge>,
    executor: NullExecutor,
}

impl Host {
    /// A player of the interface drawing a window of a size with
    /// `renderer`, its movies from the cache at `cache_root`.
    pub fn new(renderer: impl RenderBackend + 'static, cache_root: &Path, (width, height): (u32, u32)) -> Result<Self> {
        let executor = NullExecutor::new();
        let inner = NullNavigatorBackend::with_base_path(cache_root, &executor)?;
        let navigator = CacheNavigator { inner, root: cache_root.to_path_buf() };
        if !cache_root.join(EMPTY_REL).exists() {
            if let Err(e) = std::fs::write(cache_root.join(EMPTY_REL), EMPTY_SWF) {
                tracing::warn!("interface: no empty movie ({e})");
            }
        }
        // (Without the table the movies' texts read empty.)
        let strings = std::fs::read(cache_root.join(STRINGS_REL))
            .map_err(anyhow::Error::from)
            .and_then(|b| fc3_formats::oasis::Strings::from_bytes(&b))
            .unwrap_or_else(|e| {
                tracing::warn!("interface: no strings ({e:#})");
                Default::default()
            });
        let bridge = Rc::new(Bridge { strings, ..Default::default() });
        let shim = shim_swf(STAGE.0, STAGE.1)?;
        let movie = ruffle_core::tag_utils::SwfMovieData::from_data(&shim, "file:///fc3/shim.swf".into(), None, None).map_err(|e| anyhow!(e.to_string()))?;
        let player = PlayerBuilder::new()
            .with_renderer(renderer)
            .with_navigator(navigator)
            .with_external_interface(Box::new(Provider(bridge.clone())))
            .with_movie(movie)
            .with_scale_mode(StageScaleMode::ShowAll, true)
            .with_autoplay(true)
            .with_viewport_dimensions(width, height, 1.0)
            .build();
        // (Drawn over the game: what the movies leave is see-through.)
        player.lock().unwrap().set_window_mode("transparent");
        Ok(Self { player, bridge, executor })
    }

    /// Answers the engine gives to the movies' `Get`s (`IsHighDefinition`,
    /// `GetLanguage`...; `Platform` for `GetPlatform`).
    pub fn answer(&self, key: &str, value: &str) {
        self.bridge.answers.borrow_mut().insert(key.to_string(), value.to_string());
    }

    /// Loads a movie (by its archive path or cache path) at a level.
    pub fn load(&self, movie: &str, level: u32) {
        self.call("load", vec![ExternalValue::String(movie.to_string()), ExternalValue::Number(level as f64)]);
    }

    pub fn unload(&self, level: u32) {
        self.call("unload", vec![ExternalValue::Number(level as f64)]);
    }

    /// One of the game's strings by its id ("" if there is none).
    pub fn text(&self, id: u32) -> String {
        self.bridge.strings.get(id).unwrap_or_default().to_string()
    }

    /// Shows or hides the movie at a level.
    pub fn level_visible(&self, level: u32, on: bool) {
        self.call("levelVisible", vec![ExternalValue::Number(level as f64), ExternalValue::Bool(on)]);
    }

    /// Broadcasts an event to the movies listening on a channel
    /// (`_global.ExtendedEvents.<channel>`): `onEvent(args...)`.
    pub fn event(&self, channel: &str, args: &[&str]) {
        let list = args.iter().map(|a| string(a)).collect();
        self.call("event", vec![ExternalValue::String(channel.to_string()), ExternalValue::List(list)]);
    }

    /// Calls a function a movie defines (`_level1.InitAmmo`, a global's).
    pub fn call_function(&self, path: &str, args: Vec<ExternalValue>) -> ExternalValue {
        self.call("call", vec![ExternalValue::String(path.to_string()), ExternalValue::List(args)])
    }

    /// The DPS ids of the clips in the loaded movies.
    pub fn dps_ids(&self) -> Vec<String> {
        match self.call("dpsList", vec![]) {
            ExternalValue::List(l) => l.iter().map(text).collect(),
            _ => Vec::new(),
        }
    }

    /// Sets a text field's text by DPS id (false when there's none).
    pub fn dps_text(&self, id: &str, value: &str) -> bool {
        self.dps("dpsText", id, vec![string(value)])
    }

    pub fn dps_visible(&self, id: &str, on: bool) -> bool {
        self.dps("dpsVisible", id, vec![ExternalValue::Bool(on)])
    }

    /// Sends a clip to a frame (a number, or a label) and stops it there.
    pub fn dps_frame(&self, id: &str, frame: ExternalValue) -> bool {
        self.dps("dpsFrame", id, vec![frame])
    }

    /// Plays a clip on from a frame (a number, or a label).
    pub fn dps_play(&self, id: &str, frame: ExternalValue) -> bool {
        self.dps("dpsPlay", id, vec![frame])
    }

    /// Sets any property of a clip (`_alpha`, `_x`, ...).
    pub fn dps_set(&self, id: &str, property: &str, value: ExternalValue) -> bool {
        self.dps("dpsSet", id, vec![ExternalValue::String(property.to_string()), value])
    }

    /// Sets a property of the clip holding a clip.
    pub fn dps_set_parent(&self, id: &str, property: &str, value: ExternalValue) -> bool {
        self.dps("dpsSetParent", id, vec![ExternalValue::String(property.to_string()), value])
    }

    /// A clip's state as text (kind, frames, visibility, place, size, its
    /// bounds on the stage, a text field's text), for tools.
    pub fn dps_info(&self, id: &str) -> String {
        text(&self.call("dpsInfo", vec![ExternalValue::String(id.to_string())]))
    }

    /// A clip's bounds on the stage (`STAGE`'s units): x and y min, x and y
    /// max.
    pub fn dps_bounds(&self, id: &str) -> Option<[f64; 4]> {
        let ExternalValue::List(l) = self.call("dpsBounds", vec![ExternalValue::String(id.to_string())]) else { return None };
        let v: Vec<f64> = l.iter().filter_map(|x| if let ExternalValue::Number(n) = x { Some(*n) } else { None }).collect();
        (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
    }

    /// Whether a clip's page shows it (its parents are all shown; its own
    /// visibility aside).
    pub fn dps_shown(&self, id: &str) -> bool {
        matches!(self.call("dpsShown", vec![ExternalValue::String(id.to_string())]), ExternalValue::Bool(true))
    }

    /// A list for the movies' data providers (`fire.utils.DataProvider(name)`:
    /// the rucksack's loot, recipes...): each item's fields bind to the
    /// clips of the same names on its tile. The pages showing it hear of it.
    pub fn set_data(&self, name: &str, items: Vec<std::collections::BTreeMap<String, ExternalValue>>) {
        // (Blanks go as null, which the shim makes blank again: Ruffle reads
        // a blank string from the host as "null" for SWF 8.)
        let blank = |v: ExternalValue| if matches!(&v, ExternalValue::String(s) if s.is_empty()) { ExternalValue::Null } else { v };
        let list = items.into_iter().map(|item| ExternalValue::Object(item.into_iter().map(|(k, v)| (k, blank(v))).collect())).collect();
        self.call("setData", vec![ExternalValue::String(name.to_string()), ExternalValue::List(list)]);
    }

    /// A list ready for a page's request (`onRequestedMenu <id> <list>`,
    /// keyed `<list>:<id>`): the page gets it as it asks, at once, as the
    /// engine answered; the request still comes, its last argument
    /// `prepared`. A blank key forgets them all.
    pub fn prepare_data(&self, key: &str, items: Vec<std::collections::BTreeMap<String, ExternalValue>>) {
        let blank = |v: ExternalValue| if matches!(&v, ExternalValue::String(s) if s.is_empty()) { ExternalValue::Null } else { v };
        let list = items.into_iter().map(|item| ExternalValue::Object(item.into_iter().map(|(k, v)| (k, blank(v))).collect())).collect();
        self.call("prepareData", vec![ExternalValue::String(key.to_string()), ExternalValue::List(list)]);
    }

    /// Sets a property at a path (`_level20.bg.black._visible`); false if the
    /// path isn't there.
    pub fn set_path(&self, path: &str, value: ExternalValue) -> bool {
        matches!(self.call("setPath", vec![ExternalValue::String(path.to_string()), value]), ExternalValue::Bool(true))
    }

    /// What is at a path (`_level1.mcBack`, `_global.isInverted`), as text:
    /// a clip's state at a glance, else the value (tools).
    pub fn path_info(&self, path: &str) -> String {
        text(&self.call("getPath", vec![ExternalValue::String(path.to_string())]))
    }

    /// The members of the object at a path, `name:type` (tools).
    pub fn path_members(&self, path: &str) -> Vec<String> {
        let ExternalValue::List(l) = self.call("listPath", vec![ExternalValue::String(path.to_string())]) else { return vec![] };
        l.iter().map(text).collect()
    }

    pub fn dps_get(&self, id: &str, property: &str) -> ExternalValue {
        self.call("dpsGet", vec![ExternalValue::String(id.to_string()), ExternalValue::String(property.to_string())])
    }

    fn dps(&self, callback: &str, id: &str, mut args: Vec<ExternalValue>) -> bool {
        args.insert(0, ExternalValue::String(id.to_string()));
        matches!(self.call(callback, args), ExternalValue::Bool(true))
    }

    fn call(&self, name: &str, args: Vec<ExternalValue>) -> ExternalValue {
        self.player.lock().unwrap().call_internal_interface(name, args)
    }

    /// What the movies asked of the engine since the last time.
    pub fn take_calls(&self) -> Vec<EngineCall> {
        std::mem::take(&mut *self.bridge.calls.borrow_mut())
    }

    /// The sounds the movies started since the last time, by the names
    /// they export them under (`fc3_formats::feu::sound_ids` gives each its
    /// event).
    pub fn take_sounds(&self) -> Vec<String> {
        std::mem::take(&mut *self.bridge.sounds.borrow_mut())
    }

    /// Runs the movies on by `dt` seconds (their frames, scripts and loads).
    pub fn run(&mut self, dt: f64) {
        {
            let mut player = self.player.lock().unwrap();
            player.preload(&mut ExecutionLimit::none());
            player.tick(ruffle_core::FloatDuration::from_secs(dt));
        }
        self.executor.run();
    }

    /// One frame of each movie, at once (tools: no wait for the clock).
    pub fn run_frame(&mut self) {
        {
            let mut player = self.player.lock().unwrap();
            player.preload(&mut ExecutionLimit::none());
            player.run_frame();
        }
        self.executor.run();
    }

    /// Draws the interface now.
    pub fn render(&self) {
        self.player.lock().unwrap().render();
    }

    pub fn resize(&self, width: u32, height: u32) {
        self.player.lock().unwrap().set_viewport_dimensions(ruffle_core::ViewportDimensions { width, height, scale_factor: 1.0 });
    }

    /// A key, the mouse, for the movies (a menu's input manager listens to
    /// `Key` and `Mouse`). The mouse is in the window's pixels.
    pub fn input(&self, event: ruffle_core::events::PlayerEvent) {
        self.player.lock().unwrap().handle_event(event);
    }

    /// The player, for what this doesn't cover (input, the renderer).
    pub fn player(&self) -> &Arc<Mutex<Player>> {
        &self.player
    }
}

#[cfg(test)]
mod tests {
    /// The shim compiles (the game can't start its interface otherwise).
    #[test]
    fn the_shim_compiles() {
        let swf = super::shim_swf(1280, 720).unwrap();
        assert_eq!(&swf[..3], b"FWS");
    }
}

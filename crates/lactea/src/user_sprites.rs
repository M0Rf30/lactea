//! projectM user sprites (`projectm_sprite_*`): host-created image sprites with their own expression code.
//!
//! A sprite is the text of a MilkDrop sprite INI section (`img`, `init_N`, `code_N`). Every frame the code runs
//! (inputs `time frame fps progress bass mid treb bass_att mid_att treb_att`, outputs `x y sx sy rot flipx flipy
//! repeatx repeaty blendmode r g b a done burn`) and the sprite is drawn over the finished image, with the
//! 5 blend modes projectM names (the same arithmetic as MilkDrop 3's modes 0..=4). `burn` also draws it into the
//! feedback image of every preset that is running; `done` removes the sprite after that frame.
//!
//! Available in every compat mode. The code always follows projectM's rules (lines joined with `\n`, a part that
//! does not compile refuses the sprite), whatever the preset mode.
//!
//! A child of `engine` like `params.rs`, for access to the engine's private state.

use super::Engine;
use crate::cache::MediaCache;
use crate::media::Media;
use crate::sprites::{SpriteInputs, clampf};
use crate::textures::{Image, TextureProvider, pick_for_sampler, rand_slot, split_qualified};
use lactea_eel::{Context, GlobalMemory, Program, Var};
use lactea_preset::parse_user_sprite;
use lactea_render::{Compat, Renderer, SpriteDraw, SpriteVertex};
use std::collections::BTreeSet;
use std::sync::Arc;

/// `SpriteManager`'s default `m_spriteSlots`.
const DEFAULT_MAX_SPRITES: u32 = 16;

/// Renderer image ids of user sprites live above MilkDrop 3's preset-sprite id (0).
const IMAGE_KEY_BASE: u32 = 0x4000_0000;

struct Vars {
    time: Var,
    frame: Var,
    fps: Var,
    progress: Var,
    bass: Var,
    mid: Var,
    treb: Var,
    bass_att: Var,
    mid_att: Var,
    treb_att: Var,
    done: Var,
    burn: Var,
    x: Var,
    y: Var,
    sx: Var,
    sy: Var,
    rot: Var,
    flipx: Var,
    flipy: Var,
    repeatx: Var,
    repeaty: Var,
    blendmode: Var,
    r: Var,
    g: Var,
    b: Var,
    a: Var,
}

impl Vars {
    fn register(ctx: &mut Context) -> Self {
        Self {
            time: ctx.var("time"),
            frame: ctx.var("frame"),
            fps: ctx.var("fps"),
            progress: ctx.var("progress"),
            bass: ctx.var("bass"),
            mid: ctx.var("mid"),
            treb: ctx.var("treb"),
            bass_att: ctx.var("bass_att"),
            mid_att: ctx.var("mid_att"),
            treb_att: ctx.var("treb_att"),
            done: ctx.var("done"),
            burn: ctx.var("burn"),
            x: ctx.var("x"),
            y: ctx.var("y"),
            sx: ctx.var("sx"),
            sy: ctx.var("sy"),
            rot: ctx.var("rot"),
            flipx: ctx.var("flipx"),
            flipy: ctx.var("flipy"),
            repeatx: ctx.var("repeatx"),
            repeaty: ctx.var("repeaty"),
            blendmode: ctx.var("blendmode"),
            r: ctx.var("r"),
            g: ctx.var("g"),
            b: ctx.var("b"),
            a: ctx.var("a"),
        }
    }
}

struct UserSprite {
    ctx: Context,
    vars: Vars,
    /// `None` when the section has no `code_N` lines: the per-frame step is skipped entirely, inputs included.
    prog: Option<Program>,
    /// The decoded image, `Media::Still`; a missing image is a 1x1 transparent texel (projectM's placeholder).
    image: Arc<Media>,
    /// Renderer image id of this sprite, never reused.
    key: u32,
}

impl UserSprite {
    fn pixels(&self) -> Option<&Image> {
        match &*self.image {
            Media::Still(img) => Some(img),
            Media::Animated(_) => None,
        }
    }

    /// Run the code and build this frame's draw; the flag is `done`.
    fn frame(&mut self, inp: &SpriteInputs, screen: (u32, u32)) -> (SpriteDraw, bool) {
        let size = self.pixels().map_or((1, 1), |i| (i.width, i.height));
        let v = &self.vars;
        let ctx = &mut self.ctx;
        if let Some(p) = &self.prog {
            for (var, value) in [
                (v.time, inp.time),
                (v.frame, f64::from(inp.frame)),
                (v.fps, f64::from(inp.fps)),
                (v.progress, f64::from(inp.progress)),
                (v.bass, f64::from(inp.bass)),
                (v.mid, f64::from(inp.mid)),
                (v.treb, f64::from(inp.treb)),
                (v.bass_att, f64::from(inp.bass_att)),
                (v.mid_att, f64::from(inp.mid_att)),
                (v.treb_att, f64::from(inp.treb_att)),
            ] {
                ctx.set(var, value);
            }
            ctx.run(p);
        }
        let get = |var: Var| ctx.get(var) as f32;
        let done = ctx.get(v.done) != 0.0;
        let burn = ctx.get(v.burn) != 0.0;
        let quad = sprite_quad(
            clampf(get(v.x) * 2.0 - 1.0, -1000.0, 1000.0),
            clampf(get(v.y) * 2.0 - 1.0, -1000.0, 1000.0),
            clampf(get(v.sx), -1000.0, 1000.0),
            clampf(get(v.sy), -1000.0, 1000.0),
            get(v.rot),
            ctx.get(v.flipx) != 0.0,
            ctx.get(v.flipy) != 0.0,
            clampf(get(v.repeatx), 0.01, 100.0),
            clampf(get(v.repeaty), 0.01, 100.0),
            size,
            screen,
        );
        let draw = SpriteDraw {
            layer: 0,
            image: self.key,
            frame: 0,
            blend: (ctx.get(v.blendmode) as i32).clamp(0, 4),
            rgba: [
                clampf(get(v.r), 0.0, 1.0),
                clampf(get(v.g), 0.0, 1.0),
                clampf(get(v.b), 0.0, 1.0),
                clampf(get(v.a), 0.0, 1.0),
            ],
            quad,
            burn,
        };
        (draw, done)
    }
}

/// `MilkdropSprite::Draw`'s geometry: the quad in OpenGL space (y up), then flipped to the renderer's sprite space
/// (y down). `x`, `y` are the centre in clip units; the image's own aspect fits it inside `sx`/`sy`, the viewport's
/// aspect then keeps the width at screen width.
#[allow(clippy::too_many_arguments)]
fn sprite_quad(
    x: f32,
    y: f32,
    sx: f32,
    sy: f32,
    rot: f32,
    flip_x: bool,
    flip_y: bool,
    repeat_x: f32,
    repeat_y: f32,
    image: (u32, u32),
    screen: (u32, u32),
) -> [SpriteVertex; 4] {
    let (fx, fy) = (usize::from(flip_x), usize::from(flip_y));
    let mut p = [[0.0f32; 2]; 4];
    p[fx][0] = -sx;
    p[1 - fx][0] = sx;
    p[2 + fx][0] = -sx;
    p[3 - fx][0] = sx;
    p[2 * fy][1] = -sy;
    p[1 + 2 * fy][1] = -sy;
    p[2 - 2 * fy][1] = sy;
    p[3 - 2 * fy][1] = sy;

    let image_aspect = image.1 as f32 / image.0 as f32;
    let (cos, sin) = (rot.cos(), rot.sin());
    let screen_aspect = screen.0 as f32 / screen.1 as f32;
    for v in &mut p {
        if image_aspect < 1.0 {
            v[1] *= image_aspect;
        } else {
            v[0] /= image_aspect;
        }
        let (rx, ry) = (v[0] * cos - v[1] * sin, v[0] * sin + v[1] * cos);
        v[0] = rx + x;
        v[1] = ry + y;
        if screen_aspect > 1.0 {
            v[1] *= screen_aspect;
        } else {
            v[0] /= screen_aspect;
        }
    }
    let uv = [(-0.5f32, 0.5f32), (0.5, 0.5), (-0.5, -0.5), (0.5, -0.5)];
    std::array::from_fn(|i| SpriteVertex {
        pos: [p[i][0], -p[i][1]],
        uv: [uv[i].0 * repeat_x + 0.5, uv[i].1 * repeat_y + 0.5],
    })
}

/// What creating a sprite needs from the engine.
pub(crate) struct CreateEnv<'a> {
    pub provider: Option<&'a mut Box<dyn TextureProvider>>,
    pub media_cache: &'a mut MediaCache,
    pub seed: u64,
    /// Expression memory sharing the engine's `rand()` stream.
    pub gm: Arc<GlobalMemory>,
    /// Time and frame of the last rendered frame (the only inputs the init code sees).
    pub time: f64,
    pub frame: u32,
}

/// `SpriteManager`: the live sprites, oldest first.
pub(crate) struct UserSprites {
    sprites: Vec<(u32, UserSprite)>,
    max: u32,
    next_key: u32,
    /// Image ids of removed sprites the renderers may still hold.
    retired: Vec<u32>,
}

impl UserSprites {
    pub(crate) fn new() -> Self {
        Self {
            sprites: Vec::new(),
            max: DEFAULT_MAX_SPRITES,
            next_key: 0,
            retired: Vec::new(),
        }
    }

    /// `SpriteManager::Spawn`: the new id, or 0 when the type is unknown, the limit is 0, the section cannot be
    /// read or its code does not compile. At the limit the oldest sprite makes room.
    pub(crate) fn create(&mut self, kind: &str, data: &[u8], env: CreateEnv) -> u32 {
        if self.max == 0 || !kind.eq_ignore_ascii_case("milkdrop") {
            return 0;
        }
        let Some(src) = parse_user_sprite(data) else {
            return 0;
        };
        let mut ctx = Context::new(env.gm);
        ctx.reset_vars();
        let vars = Vars::register(&mut ctx);
        for (var, value) in [
            (vars.x, 0.5),
            (vars.y, 0.5),
            (vars.sx, 1.0),
            (vars.sy, 1.0),
            (vars.repeatx, 1.0),
            (vars.repeaty, 1.0),
            (vars.r, 1.0),
            (vars.g, 1.0),
            (vars.b, 1.0),
            (vars.a, 1.0),
            (vars.burn, 1.0),
        ] {
            ctx.set(var, value);
        }
        if !src.init.is_empty() {
            ctx.set(vars.time, env.time);
            ctx.set(vars.frame, f64::from(env.frame));
            match ctx.compile(&src.init) {
                Ok(p) => {
                    ctx.run(&p);
                }
                Err(_) => return 0,
            }
        }
        let prog = if src.code.is_empty() {
            None
        } else {
            match ctx.compile(&src.code) {
                Ok(p) => Some(p),
                Err(_) => return 0,
            }
        };
        let image = load_image(
            &src.img.to_ascii_lowercase(),
            env.provider,
            env.media_cache,
            env.seed,
        );

        let id = self.lowest_free_id();
        if self.sprites.len() as u32 == self.max {
            self.destroy(self.sprites[0].0);
        }
        let key = IMAGE_KEY_BASE | (self.next_key & !IMAGE_KEY_BASE);
        self.next_key = self.next_key.wrapping_add(1);
        self.sprites.push((
            id,
            UserSprite {
                ctx,
                vars,
                prog,
                image,
                key,
            },
        ));
        id
    }

    /// `GetLowestFreeIdentifier`.
    fn lowest_free_id(&self) -> u32 {
        let ids: BTreeSet<u32> = self.sprites.iter().map(|(id, _)| *id).collect();
        let mut lowest = 0;
        for id in ids {
            if id > lowest + 1 {
                return lowest + 1;
            }
            lowest = id;
        }
        lowest + 1
    }

    pub(crate) fn destroy(&mut self, id: u32) {
        if let Some(i) = self.sprites.iter().position(|(sid, _)| *sid == id) {
            let (_, s) = self.sprites.remove(i);
            self.retired.push(s.key);
        }
    }

    pub(crate) fn destroy_all(&mut self) {
        self.retired
            .extend(self.sprites.drain(..).map(|(_, s)| s.key));
    }

    pub(crate) fn count(&self) -> u32 {
        self.sprites.len() as u32
    }

    /// Ids, oldest first.
    pub(crate) fn ids(&self) -> Vec<u32> {
        self.sprites.iter().map(|(id, _)| *id).collect()
    }

    pub(crate) fn max_sprites(&self) -> u32 {
        self.max
    }

    /// `SpriteSlots(n)`: lowering the limit drops the oldest sprites.
    pub(crate) fn set_max_sprites(&mut self, max: u32) {
        self.max = max;
        while self.sprites.len() as u32 > max {
            self.destroy(self.sprites[0].0);
        }
    }

    /// The value of `name` in sprite `id`'s context; 0 for an unknown sprite or variable.
    pub(crate) fn var(&self, id: u32, name: &str) -> f64 {
        self.sprites
            .iter()
            .find(|(sid, _)| *sid == id)
            .and_then(|(_, s)| s.ctx.find_var(name).map(|v| s.ctx.get(v)))
            .unwrap_or(0.0)
    }

    /// Set `name` (registered when new) in sprite `id`; no-op for an unknown sprite.
    pub(crate) fn set_var(&mut self, id: u32, name: &str, value: f64) {
        if let Some((_, s)) = self.sprites.iter_mut().find(|(sid, _)| *sid == id) {
            let v = s.ctx.var(name);
            s.ctx.set(v, value);
        }
    }

    /// Run every sprite for this frame, oldest first; the draws go over the finished image. Sprites that set `done`
    /// are removed after the frame (they are still drawn in it).
    pub(crate) fn frame(&mut self, inp: &SpriteInputs, screen: (u32, u32)) -> Vec<SpriteDraw> {
        let mut draws = Vec::with_capacity(self.sprites.len());
        let mut finished = Vec::new();
        for (id, s) in &mut self.sprites {
            let (draw, done) = s.frame(inp, screen);
            draws.push(draw);
            if done {
                finished.push(*id);
            }
        }
        for id in finished {
            self.destroy(id);
        }
        draws
    }

    /// Image ids removed since the last call; every renderer must forget them.
    pub(crate) fn take_retired(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.retired)
    }

    /// Register the images of the live sprites a renderer does not hold yet (renderers drop their sprite images when
    /// a preset is loaded into them).
    pub(crate) fn sync_images(&self, renderer: &mut Renderer) {
        for (_, s) in &self.sprites {
            if renderer.has_sprite_image(s.key) {
                continue;
            }
            if let Some(img) = s.pixels() {
                // an image the GPU refuses (too large) simply is not drawn
                let _ =
                    renderer.set_sprite_image(s.key, img.width, img.height, &[&img.rgba8], true);
            }
        }
    }
}

/// `MilkdropSprite::Init`'s texture lookup: `randNN` names pick from the provider's pool, any other name is
/// asked for without its quality prefix and then without a file extension.
fn load_image(
    name: &str,
    provider: Option<&mut Box<dyn TextureProvider>>,
    cache: &mut MediaCache,
    seed: u64,
) -> Arc<Media> {
    let placeholder = || {
        Arc::new(Media::Still(Image {
            width: 1,
            height: 1,
            rgba8: vec![0; 4],
        }))
    };
    let Some(provider) = provider else {
        return placeholder();
    };
    // projectM's texture rules, whatever mode the presets run in
    let rules = Compat::ProjectM.rules();
    let (bare, ..) = split_qualified(name);
    let candidates: Vec<String> = if rand_slot(bare, &rules).is_some() {
        pick_for_sampler(seed, &rules, name, bare, &provider.random_pool())
            .into_iter()
            .collect()
    } else {
        let mut v = vec![bare.to_owned()];
        if let Some((stem, _)) = bare.rsplit_once('.') {
            v.push(stem.to_owned());
        }
        v
    };
    for c in candidates {
        if let Some(m) = cache.still(&mut **provider, &c)
            && let Media::Still(img) = &*m
            && img.width > 0
            && img.height > 0
            && img.rgba8.len() == img.width as usize * img.height as usize * 4
        {
            return m;
        }
    }
    placeholder()
}

/// The `projectm_sprite_*` API.
impl Engine {
    /// Create a user sprite (`projectm_sprite_create`). `kind` is the case-insensitive sprite type (`"milkdrop"`),
    /// `code` the text of an `imgNN` section. Returns the sprite id, or 0 for an unknown type, a limit of 0, data that
    /// cannot be read or code that does not compile. At the limit the oldest sprite is destroyed to make room.
    ///
    /// The image comes from the [`TextureProvider`] (`img` without quality prefix and extension, `randNN` names
    /// picked from its pool); a missing one draws as a transparent texel.
    pub fn sprite_create(&mut self, kind: &str, code: &[u8]) -> u32 {
        let env = CreateEnv {
            provider: self.provider.as_mut(),
            media_cache: &mut self.media_cache,
            seed: self.cfg.seed,
            gm: Arc::new(GlobalMemory::sharing_rng_with(&self.rng_owner)),
            time: self.last_time.unwrap_or(0.0),
            frame: self.last_frame_no,
        };
        self.user_sprites.create(kind, code, env)
    }

    /// Destroy one sprite (`projectm_sprite_destroy`); unknown ids are ignored.
    pub fn sprite_destroy(&mut self, id: u32) {
        self.user_sprites.destroy(id);
    }

    /// Destroy every sprite (`projectm_sprite_destroy_all`).
    pub fn sprite_destroy_all(&mut self) {
        self.user_sprites.destroy_all();
    }

    /// Number of live sprites (`projectm_sprite_get_sprite_count`).
    pub fn sprite_count(&self) -> u32 {
        self.user_sprites.count()
    }

    /// Ids of the live sprites, oldest first (`projectm_sprite_get_sprite_ids`).
    pub fn sprite_ids(&self) -> Vec<u32> {
        self.user_sprites.ids()
    }

    /// Limit of concurrent sprites (`projectm_sprite_set_max_sprites`, default 16); lowering it destroys the oldest.
    pub fn set_max_sprites(&mut self, max: u32) {
        self.user_sprites.set_max_sprites(max);
    }

    /// The sprite limit (`projectm_sprite_get_max_sprites`).
    pub fn max_sprites(&self) -> u32 {
        self.user_sprites.max_sprites()
    }

    /// A variable of a sprite's code context (`projectm_sprite_get_var`); 0 for an unknown sprite or variable.
    pub fn sprite_var(&self, id: u32, name: &str) -> f64 {
        self.user_sprites.var(id, name)
    }

    /// Set a variable of a sprite's code context (`projectm_sprite_set_var`); ignored for an unknown sprite.
    pub fn set_sprite_var(&mut self, id: u32, name: &str, value: f64) {
        self.user_sprites.set_var(id, name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_env(cache: &mut MediaCache) -> CreateEnv<'_> {
        CreateEnv {
            provider: None,
            media_cache: cache,
            seed: 1,
            gm: Arc::new(GlobalMemory::new()),
            time: 0.0,
            frame: 0,
        }
    }

    #[test]
    fn quad_of_a_centred_unit_sprite_on_a_square_screen_covers_the_image() {
        let q = sprite_quad(
            0.0,
            0.0,
            0.5,
            0.5,
            0.0,
            false,
            false,
            1.0,
            1.0,
            (64, 64),
            (100, 100),
        );
        // y down: vertex 0 is the top left, its uv the top left of the image
        assert_eq!(q[0].pos, [-0.5, 0.5]);
        assert_eq!(q[3].pos, [0.5, -0.5]);
        assert_eq!(q[0].uv, [0.0, 1.0]);
        assert_eq!(q[3].uv, [1.0, 0.0]);
    }

    #[test]
    fn flip_x_swaps_the_corners_not_the_uvs() {
        let q = sprite_quad(
            0.0,
            0.0,
            0.5,
            0.5,
            0.0,
            true,
            false,
            1.0,
            1.0,
            (64, 64),
            (100, 100),
        );
        assert_eq!(q[0].pos[0], 0.5);
        assert_eq!(q[0].uv, [0.0, 1.0]);
    }

    #[test]
    fn ids_fill_the_lowest_gap_and_the_oldest_sprite_makes_room() {
        let mut us = UserSprites::new();
        let mut cache = MediaCache::new();
        let mut make = |us: &mut UserSprites| {
            us.create(
                "MilkDrop",
                b"img=none\ncode_1=a=1;",
                CreateEnv {
                    provider: None,
                    media_cache: &mut cache,
                    seed: 1,
                    gm: Arc::new(GlobalMemory::new()),
                    time: 0.0,
                    frame: 0,
                },
            )
        };
        assert_eq!([make(&mut us), make(&mut us), make(&mut us)], [1, 2, 3]);
        us.destroy(2);
        assert_eq!(make(&mut us), 2);
        assert_eq!(us.ids(), [1, 3, 2]);
        // at the limit the oldest makes room; the id is picked before it leaves
        us.set_max_sprites(3);
        assert_eq!(make(&mut us), 4);
        assert_eq!(us.ids(), [3, 2, 4]);
        us.set_max_sprites(1);
        assert_eq!(us.ids(), [4]);
    }

    #[test]
    fn done_removes_the_sprite_after_it_drew_and_unknown_type_fails() {
        let mut us = UserSprites::new();
        let mut cache = MediaCache::new();
        assert_eq!(us.create("gif", b"img=a", test_env(&mut cache)), 0);
        assert_eq!(
            us.create("milkdrop", b"img=a\ncode_1=done=1;", test_env(&mut cache)),
            1
        );
        let draws = us.frame(&SpriteInputs::default(), (100, 100));
        assert_eq!(draws.len(), 1);
        assert_eq!(us.count(), 0);
    }
}

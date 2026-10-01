use core::fmt::Write as _;

use gam::menu::*; /* brings in minigfx: Point, Rectangle, Circle, DrawStyle, PixelColor, TextView,
                    * GlyphStyle... */
use gam::*; // Gam, UxRegistration, GamObjectList, GamObjectType, APP_NAME_PAKLMAN, FocusState...

use super::*;

/// Maze dimensions, in cells.
const COLS: i32 = 28;
const ROWS: i32 = 31;

/// The classic 28x31 maze.
///   `#` wall, `.` dot, `o` power pellet, `-` ghost-house door, ` ` empty floor.
/// Row 14 is the wrap-around tunnel. Areas outside the maze proper are empty but unreachable.
const MAZE: [&[u8; COLS as usize]; ROWS as usize] = [
    b"############################",
    b"#............##............#",
    b"#.####.#####.##.#####.####.#",
    b"#o####.#####.##.#####.####o#",
    b"#.####.#####.##.#####.####.#",
    b"#..........................#",
    b"#.####.##.########.##.####.#",
    b"#.####.##.########.##.####.#",
    b"#......##....##....##......#",
    b"######.##### ## #####.######",
    b"     #.##### ## #####.#     ",
    b"     #.##          ##.#     ",
    b"     #.## ###--### ##.#     ",
    b"######.## #      # ##.######",
    b"      .   #      #   .      ",
    b"######.## #      # ##.######",
    b"     #.## ######## ##.#     ",
    b"     #.##          ##.#     ",
    b"     #.## ######## ##.#     ",
    b"######.## ######## ##.######",
    b"#............##............#",
    b"#.####.#####.##.#####.####.#",
    b"#.####.#####.##.#####.####.#",
    b"#o..##.......  .......##..o#",
    b"###.##.##.########.##.##.###",
    b"###.##.##.########.##.##.###",
    b"#......##....##....##......#",
    b"#.##########.##.##########.#",
    b"#.##########.##.##########.#",
    b"#..........................#",
    b"############################",
];

/// Player start cell.
const PAC_START: (i32, i32) = (13, 23);
/// The cell just above the ghost-house door; ghosts leaving the house head here.
const HOUSE_EXIT: (i32, i32) = (13, 11);
/// The middle of the ghost house; eaten ghosts return here to revive.
const HOUSE_CENTER: (i32, i32) = (13, 14);
/// Where bonus fruit appears (just below the ghost house).
const FRUIT_POS: (i32, i32) = (13, 17);
/// Where the "READY!" banner is shown (the open row below the ghost house).
const BANNER_ROW: i32 = 17;

/// Animation frames per second (derived from the pump interval).
const SEC: u32 = (1000 / PAKLMAN_TICK_MS) as u32;

/// Entities accumulate `speed` per frame and take one step each time the accumulator
/// passes this threshold. All speeds are kept below it, so at most one step per frame.
const STEP: u32 = 100;
const PAC_SPEED: u32 = 24;
const GHOST_SPEED: u32 = 22;
const GHOST_SLOW_SPEED: u32 = 12; // frightened, in the tunnel, or inside the house
const EYES_SPEED: u32 = 60;

/// Scatter/chase schedule, in seconds. Even indices are scatter, odd are chase; after the
/// last entry the ghosts chase forever.
const MODE_SCHEDULE: [u32; 7] = [7, 20, 7, 20, 5, 20, 5];

/// Dots eaten before each ghost leaves the house: at the start of a level, and after a death.
const RELEASE_DOTS: [u32; 4] = [0, 0, 30, 60];
const RELEASE_DOTS_AFTER_DEATH: [u32; 4] = [0, 7, 17, 32];
/// If the player goes this long without eating a dot, the next ghost is released anyway.
const RELEASE_IDLE_FRAMES: u32 = 4 * SEC;

/// Bonus fruit appears after this many dots have been eaten in a level.
const FRUIT_DOTS: [u32; 2] = [70, 170];
const FRUIT_SCORE: [u32; 8] = [100, 300, 500, 700, 1000, 2000, 3000, 5000];
const EXTRA_LIFE_SCORE: u32 = 10_000;

const READY_FRAMES: u32 = 2 * SEC;
const EAT_PAUSE_FRAMES: u32 = SEC / 2;
const DEATH_FRAMES: u32 = 2 * SEC;
const LEVEL_CLEAR_FRAMES: u32 = 2 * SEC;

/// Vertical space reserved above the maze for the score line and below it for lives.
const HEADER_H: isize = 18;
const FOOTER_H: isize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tile {
    Wall,
    Door,
    Empty,
    Dot,
    Power,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dir {
    Up,
    Left,
    Down,
    Right,
}

/// Directions in the classic tie-break order used by the ghost AI.
const DIRS: [Dir; 4] = [Dir::Up, Dir::Left, Dir::Down, Dir::Right];

impl Dir {
    fn delta(self) -> (i32, i32) {
        match self {
            Dir::Up => (0, -1),
            Dir::Left => (-1, 0),
            Dir::Down => (0, 1),
            Dir::Right => (1, 0),
        }
    }

    fn reverse(self) -> Dir {
        match self {
            Dir::Up => Dir::Down,
            Dir::Left => Dir::Right,
            Dir::Down => Dir::Up,
            Dir::Right => Dir::Left,
        }
    }
}

/// The cell one step from (c, r) in direction `d`, wrapping horizontally through the tunnel.
fn step(pos: (i32, i32), d: Dir) -> (i32, i32) {
    let (dx, dy) = d.delta();
    ((pos.0 + dx).rem_euclid(COLS), pos.1 + dy)
}

fn dist2(a: (i32, i32), b: (i32, i32)) -> i32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GhostState {
    /// bobbing inside the house, waiting to be released
    InHouse,
    /// walking out through the door
    Leaving,
    /// roaming the maze
    Active,
    /// eaten: only the eyes remain, racing back to the house
    Eaten,
}

struct Ghost {
    pos: (i32, i32),
    prev: (i32, i32),
    dir: Dir,
    state: GhostState,
    frightened: bool,
    /// set when the ghosts switch modes; the next step reverses direction
    reverse: bool,
    acc: u32,
    /// scatter-mode target, outside the maze near this ghost's home corner
    corner: (i32, i32),
}

impl Ghost {
    fn new(index: usize) -> Self {
        // Blinky, Pinky, Inky, Clyde
        let (pos, dir, state, corner) = match index {
            0 => ((13, 11), Dir::Left, GhostState::Active, (25, -3)),
            1 => ((13, 14), Dir::Down, GhostState::InHouse, (2, -3)),
            2 => ((11, 14), Dir::Up, GhostState::InHouse, (27, 31)),
            _ => ((16, 14), Dir::Up, GhostState::InHouse, (0, 31)),
        };
        Ghost { pos, prev: pos, dir, state, frightened: false, reverse: false, acc: 0, corner }
    }
}

struct Pac {
    pos: (i32, i32),
    prev: (i32, i32),
    /// current heading; kept even while stopped against a wall
    dir: Dir,
    /// buffered turn request, taken as soon as the maze allows it
    want: Dir,
    acc: u32,
    /// advances every step to animate the mouth
    anim: u32,
}

impl Pac {
    fn new() -> Self {
        Pac { pos: PAC_START, prev: PAC_START, dir: Dir::Left, want: Dir::Left, acc: 0, anim: 0 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// "READY!" countdown before play (re)starts
    Ready(u32),
    Playing,
    /// brief freeze after eating a ghost
    EatPause(u32),
    /// death animation
    Dying(u32),
    /// maze flashes before the next level
    LevelClear(u32),
}

/// Accumulates draw objects and ships them to the GAM 32 at a time.
struct Batch<'a> {
    gam: &'a Gam,
    gid: Gid,
    list: GamObjectList,
    count: usize,
}

impl<'a> Batch<'a> {
    fn new(gam: &'a Gam, gid: Gid) -> Self { Batch { gam, gid, list: GamObjectList::new(gid), count: 0 } }

    fn push(&mut self, obj: GamObjectType) {
        if let Err(obj) = self.list.push(obj) {
            self.flush();
            self.list.push(obj).ok();
        }
        self.count += 1;
    }

    fn rect(&mut self, x0: isize, y0: isize, x1: isize, y1: isize, color: PixelColor) {
        self.push(GamObjectType::Rect(Rectangle::new_coords_with_style(
            x0,
            y0,
            x1,
            y1,
            DrawStyle::new(color, color, 0),
        )));
    }

    fn circle(&mut self, cx: isize, cy: isize, r: isize, color: PixelColor) {
        self.push(GamObjectType::Circ(Circle::new_with_style(
            Point::new(cx, cy),
            r,
            DrawStyle::new(color, color, 1),
        )));
    }

    fn flush(&mut self) {
        if self.count > 0 {
            self.gam.draw_list(self.list).expect("couldn't execute draw list");
        }
        self.list = GamObjectList::new(self.gid);
        self.count = 0;
    }
}

pub(crate) struct Paklman {
    gam: gam::Gam,
    gid: Gid,
    screensize: Point,
    _token: [u32; 4],
    modals: modals::Modals,
    rng: u32,

    // geometry, computed once from the canvas size
    cell: isize,
    /// top-left pixel of the maze
    ox: isize,
    oy: isize,

    // maze state
    tiles: [[Tile; COLS as usize]; ROWS as usize],
    dots_left: u32,

    // actors
    pac: Pac,
    ghosts: [Ghost; 4],

    // game state
    phase: Phase,
    paused: bool,
    frame: u32,
    score: u32,
    hiscore: u32,
    lives: u32,
    level: u32,
    extra_life_awarded: bool,
    /// index into MODE_SCHEDULE, and frames spent in the current mode
    mode_idx: usize,
    mode_frames: u32,
    /// frames of frightened mode remaining
    fright_frames: u32,
    /// ghosts eaten during the current power pellet (doubles the bonus each time)
    eat_chain: u32,
    /// last ghost bonus, shown in the header during the eat pause
    last_bonus: u32,
    dots_eaten: u32,
    dots_since_life: u32,
    died_this_level: bool,
    idle_frames: u32,
    fruit_frames: u32,
    fruit_shown: [bool; 2],
    hide_ghosts: bool,

    /// cells whose background must be repainted on the next frame
    dirty: Vec<(i32, i32)>,
    shown_score: Option<(u32, u32)>,
}

impl Paklman {
    pub(crate) fn new(sid: xous::SID) -> Self {
        let xns = xous_names::XousNames::new().expect("couldn't connect to Xous Namespace Server");
        let gam = gam::Gam::new(&xns).expect("can't connect to Graphical Abstraction Manager");

        let token = gam
            .register_ux(UxRegistration {
                app_name: String::from(gam::APP_NAME_PAKLMAN),
                ux_type: gam::UxType::Framebuffer,
                predictor: None,
                listener: sid.to_array(),
                redraw_id: AppOp::Redraw.to_u32().unwrap(),
                gotinput_id: None,
                audioframe_id: None,
                focuschange_id: Some(AppOp::FocusChange.to_u32().unwrap()),
                rawkeys_id: Some(AppOp::Rawkeys.to_u32().unwrap()),
            })
            .expect("couldn't register Ux context for paklman")
            .unwrap();

        let gid = gam.request_content_canvas(token).expect("couldn't get content canvas");
        let screensize = gam.get_canvas_bounds(gid).expect("couldn't get dimensions of content canvas");

        // Fit the maze to the canvas, leaving room for the score above and lives below.
        let cell = core::cmp::min(
            screensize.x / COLS as isize,
            (screensize.y - HEADER_H - FOOTER_H) / ROWS as isize,
        )
        .max(4);
        let maze_w = cell * COLS as isize;
        let maze_h = cell * ROWS as isize;
        let ox = (screensize.x - maze_w) / 2;
        let top = ((screensize.y - (HEADER_H + maze_h + FOOTER_H)) / 2).max(0);
        let oy = top + HEADER_H;

        let trng = trng::Trng::new(&xns).unwrap();
        let rng = trng.get_u32().unwrap() | 1;
        let modals = modals::Modals::new(&xns).unwrap();

        let mut game = Paklman {
            gam,
            gid,
            screensize,
            _token: token,
            modals,
            rng,
            cell,
            ox,
            oy,
            tiles: [[Tile::Empty; COLS as usize]; ROWS as usize],
            dots_left: 0,
            pac: Pac::new(),
            ghosts: [Ghost::new(0), Ghost::new(1), Ghost::new(2), Ghost::new(3)],
            phase: Phase::Ready(READY_FRAMES),
            paused: false,
            frame: 0,
            score: 0,
            hiscore: 0,
            lives: 3,
            level: 1,
            extra_life_awarded: false,
            mode_idx: 0,
            mode_frames: 0,
            fright_frames: 0,
            eat_chain: 0,
            last_bonus: 0,
            dots_eaten: 0,
            dots_since_life: 0,
            died_this_level: false,
            idle_frames: 0,
            fruit_frames: 0,
            fruit_shown: [false; 2],
            hide_ghosts: false,
            dirty: Vec::new(),
            shown_score: None,
        };
        game.new_game();
        game
    }

    fn random(&mut self) -> u32 {
        // xorshift32
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    // ---- game lifecycle -----------------------------------------------------------

    fn new_game(&mut self) {
        self.score = 0;
        self.lives = 3;
        self.level = 1;
        self.extra_life_awarded = false;
        self.start_level();
    }

    fn start_level(&mut self) {
        self.dots_left = 0;
        for r in 0..ROWS as usize {
            for c in 0..COLS as usize {
                self.tiles[r][c] = match MAZE[r][c] {
                    b'#' => Tile::Wall,
                    b'-' => Tile::Door,
                    b'.' => Tile::Dot,
                    b'o' => Tile::Power,
                    _ => Tile::Empty,
                };
                if matches!(self.tiles[r][c], Tile::Dot | Tile::Power) {
                    self.dots_left += 1;
                }
            }
        }
        self.dots_eaten = 0;
        self.died_this_level = false;
        self.fruit_shown = [false; 2];
        self.reset_actors();
    }

    /// Put everyone back at their start positions (new level, or after losing a life).
    fn reset_actors(&mut self) {
        self.pac = Pac::new();
        for (i, g) in self.ghosts.iter_mut().enumerate() {
            *g = Ghost::new(i);
        }
        self.mode_idx = 0;
        self.mode_frames = 0;
        self.fright_frames = 0;
        self.eat_chain = 0;
        self.dots_since_life = 0;
        self.idle_frames = 0;
        self.fruit_frames = 0;
        self.hide_ghosts = false;
        self.phase = Phase::Ready(READY_FRAMES);
    }

    fn lose_life(&mut self) {
        self.lives = self.lives.saturating_sub(1);
        if self.lives == 0 {
            self.game_over();
            return;
        }
        self.reset_actors();
        self.died_this_level = true;
        self.redraw_all();
    }

    fn game_over(&mut self) {
        self.hiscore = self.hiscore.max(self.score);
        let mut msg = String::new();
        write!(
            msg,
            "Game over!\n\nLevel: {}\nScore: {}\nHigh score: {}",
            self.level, self.score, self.hiscore
        )
        .ok();
        self.modals.show_notification(&msg, None).ok();
        self.new_game();
        self.redraw_all();
    }

    fn add_score(&mut self, points: u32) {
        self.score += points;
        self.hiscore = self.hiscore.max(self.score);
        if !self.extra_life_awarded && self.score >= EXTRA_LIFE_SCORE {
            self.extra_life_awarded = true;
            self.lives += 1;
            self.draw_footer();
        }
    }

    // ---- maze queries -------------------------------------------------------------

    fn tile(&self, pos: (i32, i32)) -> Tile {
        if pos.1 < 0 || pos.1 >= ROWS || pos.0 < 0 || pos.0 >= COLS {
            return Tile::Wall;
        }
        self.tiles[pos.1 as usize][pos.0 as usize]
    }

    fn is_wall(&self, pos: (i32, i32)) -> bool { self.tile(pos) == Tile::Wall }

    fn pac_can_enter(&self, pos: (i32, i32)) -> bool { !matches!(self.tile(pos), Tile::Wall | Tile::Door) }

    fn ghost_can_enter(&self, g: &Ghost, pos: (i32, i32)) -> bool {
        match self.tile(pos) {
            Tile::Wall => false,
            // only ghosts entering or leaving the house may pass the door
            Tile::Door => matches!(g.state, GhostState::Leaving | GhostState::Eaten),
            _ => true,
        }
    }

    fn in_tunnel(pos: (i32, i32)) -> bool { pos.1 == 14 && (pos.0 <= 5 || pos.0 >= COLS - 6) }

    fn level_bonus(&self) -> u32 { (self.level - 1).min(4) }

    fn fright_duration(&self) -> u32 { (7u32.saturating_sub(self.level)).max(1) * SEC }

    fn scatter_mode(&self) -> bool { self.mode_idx < MODE_SCHEDULE.len() && self.mode_idx % 2 == 0 }

    // ---- ghost AI -----------------------------------------------------------------

    /// Where ghost `i` is headed right now.
    fn ghost_target(&self, i: usize) -> (i32, i32) {
        let g = &self.ghosts[i];
        match g.state {
            GhostState::Leaving => return HOUSE_EXIT,
            GhostState::Eaten => return HOUSE_CENTER,
            _ => {}
        }
        if self.scatter_mode() {
            return g.corner;
        }
        let p = self.pac.pos;
        let (dx, dy) = self.pac.dir.delta();
        match i {
            // Blinky: straight for the player
            0 => p,
            // Pinky: ambush four cells ahead of the player
            1 => (p.0 + 4 * dx, p.1 + 4 * dy),
            // Inky: double the vector from Blinky to two cells ahead of the player
            2 => {
                let v = (p.0 + 2 * dx, p.1 + 2 * dy);
                let b = self.ghosts[0].pos;
                (2 * v.0 - b.0, 2 * v.1 - b.1)
            }
            // Clyde: chase when far, retreat to his corner when close
            _ => {
                if dist2(g.pos, p) > 64 {
                    p
                } else {
                    g.corner
                }
            }
        }
    }

    /// Choose ghost `i`'s next direction: never reverse unless it's the only way out; at
    /// junctions take the exit closest to the target (or a random one when frightened).
    fn choose_dir(&mut self, i: usize) -> Option<Dir> {
        let g = &self.ghosts[i];
        let rev = g.dir.reverse();
        let mut options = [Dir::Up; 4];
        let mut n = 0;
        for &d in DIRS.iter() {
            if d != rev && self.ghost_can_enter(g, step(g.pos, d)) {
                options[n] = d;
                n += 1;
            }
        }
        if n == 0 {
            return if self.ghost_can_enter(g, step(g.pos, rev)) { Some(rev) } else { None };
        }
        if g.frightened && g.state == GhostState::Active {
            let pick = (self.random() as usize) % n;
            return Some(options[pick]);
        }
        let target = self.ghost_target(i);
        let pos = g.pos;
        let mut best = options[0];
        let mut best_d = i32::MAX;
        for &d in options[..n].iter() {
            let dd = dist2(step(pos, d), target);
            if dd < best_d {
                best_d = dd;
                best = d;
            }
        }
        Some(best)
    }

    fn move_ghost(&mut self, i: usize) {
        if self.ghosts[i].state == GhostState::InHouse {
            // bob up and down until released
            let g = &mut self.ghosts[i];
            let next = step(g.pos, g.dir);
            if self.tiles[next.1 as usize][next.0 as usize] == Tile::Empty {
                g.pos = next;
            } else {
                g.dir = g.dir.reverse();
            }
            return;
        }

        let dir = if self.ghosts[i].reverse {
            self.ghosts[i].reverse = false;
            Some(self.ghosts[i].dir.reverse())
        } else {
            self.choose_dir(i)
        };
        let g = &mut self.ghosts[i];
        if let Some(d) = dir {
            g.dir = d;
            g.pos = step(g.pos, d);
        }
        match g.state {
            GhostState::Leaving if g.pos == HOUSE_EXIT => {
                g.state = GhostState::Active;
                g.dir = Dir::Left;
            }
            GhostState::Eaten if g.pos == HOUSE_CENTER => {
                // revived; head straight back out
                g.state = GhostState::Leaving;
            }
            _ => {}
        }
    }

    fn ghost_speed(&self, g: &Ghost) -> u32 {
        match g.state {
            GhostState::Eaten => EYES_SPEED,
            GhostState::InHouse | GhostState::Leaving => GHOST_SLOW_SPEED,
            GhostState::Active if g.frightened || Self::in_tunnel(g.pos) => GHOST_SLOW_SPEED,
            GhostState::Active => GHOST_SPEED + self.level_bonus(),
        }
    }

    /// Let the next waiting ghost out of the house once enough dots have been eaten, or if
    /// the player has been avoiding dots for too long.
    fn release_ghosts(&mut self) {
        let limits = if self.died_this_level { RELEASE_DOTS_AFTER_DEATH } else { RELEASE_DOTS };
        for i in 1..4 {
            if self.ghosts[i].state == GhostState::InHouse {
                if self.dots_since_life >= limits[i] || self.idle_frames >= RELEASE_IDLE_FRAMES {
                    self.ghosts[i].state = GhostState::Leaving;
                    self.idle_frames = 0;
                }
                // ghosts leave strictly in order
                break;
            }
        }
    }

    // ---- player -------------------------------------------------------------------

    /// Returns true if the player moved.
    fn move_pac(&mut self) -> bool {
        if self.pac_can_enter(step(self.pac.pos, self.pac.want)) {
            self.pac.dir = self.pac.want;
        }
        let next = step(self.pac.pos, self.pac.dir);
        if self.pac_can_enter(next) {
            self.pac.pos = next;
            self.pac.anim = self.pac.anim.wrapping_add(1);
            true
        } else {
            false
        }
    }

    /// Eat whatever is under the player.
    fn eat(&mut self) {
        let (c, r) = self.pac.pos;
        match self.tiles[r as usize][c as usize] {
            Tile::Dot => {
                self.tiles[r as usize][c as usize] = Tile::Empty;
                self.add_score(10);
                self.ate_dot();
            }
            Tile::Power => {
                self.tiles[r as usize][c as usize] = Tile::Empty;
                self.add_score(50);
                self.ate_dot();
                self.frighten();
            }
            _ => {}
        }
        if self.fruit_frames > 0 && self.pac.pos == FRUIT_POS {
            self.fruit_frames = 0;
            let bonus = FRUIT_SCORE[((self.level - 1) as usize).min(FRUIT_SCORE.len() - 1)];
            self.add_score(bonus);
        }
    }

    fn ate_dot(&mut self) {
        self.dots_left -= 1;
        self.dots_eaten += 1;
        self.dots_since_life += 1;
        self.idle_frames = 0;
        for (i, &n) in FRUIT_DOTS.iter().enumerate() {
            if self.dots_eaten == n && !self.fruit_shown[i] {
                self.fruit_shown[i] = true;
                self.fruit_frames = 9 * SEC + SEC / 2;
                self.dirty.push(FRUIT_POS);
            }
        }
    }

    fn frighten(&mut self) {
        self.fright_frames = self.fright_duration();
        self.eat_chain = 0;
        for g in self.ghosts.iter_mut() {
            if g.state != GhostState::Eaten {
                g.frightened = true;
                if g.state == GhostState::Active {
                    g.reverse = true;
                }
            }
        }
    }

    /// Handle player/ghost contact. Returns true if play was interrupted (eat pause or death).
    fn check_collisions(&mut self) -> bool {
        for i in 0..4 {
            let g = &self.ghosts[i];
            if !matches!(g.state, GhostState::Active | GhostState::Leaving) {
                continue;
            }
            // same cell, or the two swapped cells this frame
            let touching = g.pos == self.pac.pos || (g.pos == self.pac.prev && g.prev == self.pac.pos);
            if !touching {
                continue;
            }
            if g.frightened {
                let bonus = 200 << self.eat_chain.min(3);
                self.eat_chain += 1;
                self.last_bonus = bonus;
                let g = &mut self.ghosts[i];
                g.state = GhostState::Eaten;
                g.frightened = false;
                g.reverse = false;
                self.add_score(bonus);
                self.phase = Phase::EatPause(EAT_PAUSE_FRAMES);
            } else {
                self.phase = Phase::Dying(DEATH_FRAMES);
            }
            return true;
        }
        false
    }

    // ---- frame logic --------------------------------------------------------------

    fn play_frame(&mut self) {
        // frightened timer; the scatter/chase clock pauses while it runs
        if self.fright_frames > 0 {
            self.fright_frames -= 1;
            if self.fright_frames == 0 {
                for g in self.ghosts.iter_mut() {
                    g.frightened = false;
                }
            }
        } else if self.mode_idx < MODE_SCHEDULE.len() {
            self.mode_frames += 1;
            if self.mode_frames >= MODE_SCHEDULE[self.mode_idx] * SEC {
                self.mode_idx += 1;
                self.mode_frames = 0;
                for g in self.ghosts.iter_mut() {
                    if g.state == GhostState::Active {
                        g.reverse = true;
                    }
                }
            }
        }

        if self.fruit_frames > 0 {
            self.fruit_frames -= 1;
            if self.fruit_frames == 0 {
                self.dirty.push(FRUIT_POS);
            }
        }

        self.idle_frames += 1;
        self.release_ghosts();

        // move everyone, remembering where they were so we can erase and detect crossings
        self.pac.prev = self.pac.pos;
        for g in self.ghosts.iter_mut() {
            g.prev = g.pos;
        }

        self.pac.acc += PAC_SPEED + self.level_bonus();
        if self.pac.acc >= STEP {
            self.pac.acc -= STEP;
            if self.move_pac() {
                self.eat();
            }
        }

        for i in 0..4 {
            let speed = self.ghost_speed(&self.ghosts[i]);
            self.ghosts[i].acc += speed;
            if self.ghosts[i].acc >= STEP {
                self.ghosts[i].acc -= STEP;
                self.move_ghost(i);
            }
        }

        self.check_collisions();

        if self.dots_left == 0 && self.phase == Phase::Playing {
            self.phase = Phase::LevelClear(LEVEL_CLEAR_FRAMES);
        }

        if self.pac.prev != self.pac.pos {
            self.dirty.push(self.pac.prev);
        }
        for i in 0..4 {
            if self.ghosts[i].prev != self.ghosts[i].pos {
                self.dirty.push(self.ghosts[i].prev);
            }
        }
        if self.frame % 8 == 0 {
            // blink the power pellets
            for r in 0..ROWS {
                for c in 0..COLS {
                    if self.tiles[r as usize][c as usize] == Tile::Power {
                        self.dirty.push((c, r));
                    }
                }
            }
        }

        // Only talk to the GAM when something on screen actually changed.
        let flashing = self.fright_frames > 0 && self.fright_frames <= 2 * SEC && self.fright_frames % 6 == 0;
        if !self.dirty.is_empty() || flashing || self.phase != Phase::Playing {
            self.draw_frame();
        }
    }

    // ---- pixel geometry & sprites -------------------------------------------------

    fn cell_origin(&self, pos: (i32, i32)) -> (isize, isize) {
        (self.ox + pos.0 as isize * self.cell, self.oy + pos.1 as isize * self.cell)
    }

    fn cell_center(&self, pos: (i32, i32)) -> (isize, isize) {
        let (x0, y0) = self.cell_origin(pos);
        (x0 + self.cell / 2, y0 + self.cell / 2)
    }

    /// Sprite radius: sprites fill their cell with a 1px margin.
    fn radius(&self) -> isize { (self.cell / 2 - 1).max(2) }

    fn blink_on(&self) -> bool { (self.frame / 8) % 2 == 0 }

    /// Repaint a floor cell's static contents: blank, dot, power pellet, door, or fruit.
    fn push_cell_bg(&self, b: &mut Batch, pos: (i32, i32)) {
        let tile = self.tile(pos);
        if tile == Tile::Wall {
            return;
        }
        let (x0, y0) = self.cell_origin(pos);
        let (cx, cy) = self.cell_center(pos);
        let cell = self.cell;
        b.rect(x0, y0, x0 + cell - 1, y0 + cell - 1, PixelColor::Light);
        match tile {
            Tile::Dot => b.rect(cx - 1, cy - 1, cx + 1, cy + 1, PixelColor::Dark),
            Tile::Power => {
                if self.blink_on() || self.phase != Phase::Playing {
                    b.circle(cx, cy, cell / 3, PixelColor::Dark);
                }
            }
            Tile::Door => b.rect(x0, cy - 1, x0 + cell - 1, cy, PixelColor::Dark),
            _ => {}
        }
        if self.fruit_frames > 0 && pos == FRUIT_POS {
            // a little pair of cherries
            b.push(GamObjectType::Line(Line::new_with_style(
                Point::new(cx - 2, cy + 1),
                Point::new(cx + 2, cy - 4),
                DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1),
            )));
            b.push(GamObjectType::Line(Line::new_with_style(
                Point::new(cx + 3, cy + 1),
                Point::new(cx + 2, cy - 4),
                DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 1),
            )));
            b.circle(cx - 2, cy + 2, 2, PixelColor::Dark);
            b.circle(cx + 3, cy + 2, 2, PixelColor::Dark);
        }
    }

    /// Outline every wall block. Each wall cell draws a thick edge on every side that faces
    /// open space, plus a corner patch where two such edges meet at an inside corner.
    fn push_walls(&self, b: &mut Batch, color: PixelColor) {
        let cell = self.cell;
        let t = 2; // line thickness
        for r in 0..ROWS {
            for c in 0..COLS {
                if !self.is_wall((c, r)) {
                    continue;
                }
                // treat off-maze as open so the outer border gets a double line
                let open = |dc: i32, dr: i32| {
                    let (nc, nr) = (c + dc, r + dr);
                    nc < 0 || nc >= COLS || nr < 0 || nr >= ROWS || !self.is_wall((nc, nr))
                };
                let (x0, y0) = self.cell_origin((c, r));
                let (x1, y1) = (x0 + cell - 1, y0 + cell - 1);
                if open(0, -1) {
                    b.rect(x0, y0, x1, y0 + t - 1, color);
                }
                if open(0, 1) {
                    b.rect(x0, y1 - t + 1, x1, y1, color);
                }
                if open(-1, 0) {
                    b.rect(x0, y0, x0 + t - 1, y1, color);
                }
                if open(1, 0) {
                    b.rect(x1 - t + 1, y0, x1, y1, color);
                }
                // inside corners: both orthogonal neighbours are wall, the diagonal is open
                for &(dc, dr) in [(-1, -1), (1, -1), (-1, 1), (1, 1)].iter() {
                    if !open(dc, 0) && !open(0, dr) && open(dc, dr) {
                        let x = if dc < 0 { x0 } else { x1 - t + 1 };
                        let y = if dr < 0 { y0 } else { y1 - t + 1 };
                        b.rect(x, y, x + t - 1, y + t - 1, color);
                    }
                }
            }
        }
    }

    /// Draw the player with a chomping mouth, or shrunk to `radius` during the death animation.
    fn push_pac(&self, b: &mut Batch, radius: isize) {
        if radius <= 0 {
            return;
        }
        let (cx, cy) = self.cell_center(self.pac.pos);
        b.circle(cx, cy, radius, PixelColor::Dark);
        if radius < self.radius() {
            return; // no mouth while dying
        }
        // mouth half-opening at the lips, cycling closed -> half -> open -> half
        let open = [0, radius / 2, radius * 3 / 4, radius / 2][(self.pac.anim % 4) as usize];
        if open == 0 {
            return;
        }
        let (dx, dy) = self.pac.dir.delta();
        let (dx, dy) = (dx as isize, dy as isize);
        // carve a wedge, one pixel-wide slice at a time, widening towards the lips
        for k in 0..=radius {
            let h = (k * open + radius / 2) / radius;
            let (px, py) = (cx + dx * k, cy + dy * k);
            if dx != 0 {
                b.rect(px, py - h, px, py + h, PixelColor::Light);
            } else {
                b.rect(px - h, py, px + h, py, PixelColor::Light);
            }
        }
    }

    fn push_ghost(&self, b: &mut Batch, i: usize) {
        let g = &self.ghosts[i];
        let (cx, cy) = self.cell_center(g.pos);
        let r = self.radius();
        let (dx, dy) = g.dir.delta();
        let (dx, dy) = (dx as isize, dy as isize);
        let dark = PixelColor::Dark;
        let light = PixelColor::Light;

        if g.state == GhostState::Eaten {
            // just the eyes, outlined so they show on the light floor
            for &ex in [cx - r + 1, cx + 1].iter() {
                b.push(GamObjectType::Rect(Rectangle::new_coords_with_style(
                    ex,
                    cy - 3,
                    ex + 3,
                    cy,
                    DrawStyle::new(light, dark, 1),
                )));
                b.rect(ex + 1 + dx.max(0), cy - 2 + dy.max(0), ex + 1 + dx.max(0), cy - 2 + dy.max(0), dark);
            }
            return;
        }

        // body: a dome on top of a skirt with little feet
        let body = |b: &mut Batch, inset: isize, color: PixelColor| {
            b.circle(cx, cy, r - inset, color);
            b.rect(cx - r + inset, cy, cx + r - inset, cy + r - inset, color);
        };

        let flashing = self.fright_frames <= 2 * SEC && (self.fright_frames / 6) % 2 == 0;
        if g.frightened && !flashing {
            // hollow ghost with a worried little face
            body(b, 0, dark);
            body(b, 1, light);
            b.rect(cx - 3, cy - 2, cx - 2, cy - 1, dark);
            b.rect(cx + 2, cy - 2, cx + 3, cy - 1, dark);
            for k in -1..=1 {
                b.rect(cx + 2 * k - 1, cy + 2, cx + 2 * k - 1, cy + 2, dark);
                b.rect(cx + 2 * k, cy + 3, cx + 2 * k, cy + 3, dark);
            }
            for &fx in [cx - r + 2, cx, cx + r - 2].iter() {
                b.rect(fx, cy + r, fx, cy + r, light);
            }
        } else if g.frightened {
            // flashing: solid body, blank face
            body(b, 0, dark);
            b.rect(cx - 3, cy - 2, cx - 2, cy - 1, light);
            b.rect(cx + 2, cy - 2, cx + 3, cy - 1, light);
            b.rect(cx - 3, cy + 2, cx + 3, cy + 2, light);
            for &fx in [cx - r + 2, cx, cx + r - 2].iter() {
                b.rect(fx, cy + r, fx, cy + r, light);
            }
        } else {
            body(b, 0, dark);
            for &fx in [cx - r + 2, cx, cx + r - 2].iter() {
                b.rect(fx, cy + r, fx, cy + r, light);
            }
            // whites of the eyes, with pupils looking where the ghost is going
            for &ex in [cx - r + 1, cx + 1].iter() {
                b.rect(ex, cy - 3, ex + 3, cy, light);
                let px = ex + 1 + dx;
                let py = cy - 2 + dy;
                b.rect(px, py, px + 1, py + 1, dark);
            }
            // tell the four apart in monochrome: Pinky has a bow, Inky a stripe, Clyde a notch
            match i {
                1 => b.rect(cx - 1, cy - r - 1, cx + 1, cy - r, dark),
                2 => b.rect(cx - r + 1, cy + 2, cx + r - 1, cy + 2, light),
                3 => b.rect(cx - 1, cy + 2, cx + 1, cy + 3, light),
                _ => {}
            }
        }
    }

    fn push_actors(&self, b: &mut Batch) {
        if !self.hide_ghosts {
            for i in 0..4 {
                self.push_ghost(b, i);
            }
        }
        self.push_pac(b, self.radius());
    }

    fn draw_frame(&mut self) {
        let mut b = Batch::new(&self.gam, self.gid);
        for &pos in self.dirty.iter() {
            self.push_cell_bg(&mut b, pos);
        }
        self.push_actors(&mut b);
        b.flush();
        drop(b);
        self.dirty.clear();
        self.draw_header(false);
        self.gam.redraw().unwrap();
    }

    fn draw_header(&mut self, force: bool) {
        let shown = (self.score, if matches!(self.phase, Phase::EatPause(_)) { self.last_bonus } else { 0 });
        if !force && self.shown_score == Some(shown) {
            return;
        }
        self.shown_score = Some(shown);
        let maze_w = self.cell * COLS as isize;
        let mut tv = TextView::new(
            self.gid,
            TextBounds::BoundingBox(Rectangle::new(
                Point::new(self.ox, self.oy - HEADER_H),
                Point::new(self.ox + maze_w - 1, self.oy - 2),
            )),
        );
        tv.draw_border = false;
        tv.clear_area = true;
        tv.margin = Point::new(2, 0);
        tv.style = GlyphStyle::Regular;
        write!(tv.text, "SCORE {}   HIGH {}", self.score, self.hiscore).ok();
        if shown.1 != 0 {
            write!(tv.text, "   +{}", shown.1).ok();
        }
        self.gam.post_textview(&mut tv).ok();
    }

    /// Remaining lives as little Paklmen, and the level number, below the maze.
    fn draw_footer(&mut self) {
        let maze_w = self.cell * COLS as isize;
        let y0 = self.oy + self.cell * ROWS as isize + 1;
        let mut b = Batch::new(&self.gam, self.gid);
        b.rect(self.ox, y0, self.ox + maze_w - 1, y0 + FOOTER_H - 2, PixelColor::Light);
        let r = self.radius();
        let cy = y0 + FOOTER_H / 2 - 1;
        // the life currently in play isn't shown, as in the arcade
        for k in 0..self.lives.saturating_sub(1).min(8) as isize {
            let cx = self.ox + 4 + r + k * (2 * r + 5);
            b.circle(cx, cy, r, PixelColor::Dark);
            for j in 0..=r + 1 {
                let h = (j * (r * 3 / 4) + r / 2) / r;
                b.rect(cx + j, cy - h, cx + j, cy + h, PixelColor::Light);
            }
        }
        b.flush();
        drop(b);

        let mut tv = TextView::new(
            self.gid,
            TextBounds::GrowableFromTr(Point::new(self.ox + maze_w - 1, y0 - 1), (maze_w / 2) as u16),
        );
        tv.draw_border = false;
        tv.clear_area = false;
        tv.margin = Point::new(2, 0);
        tv.style = GlyphStyle::Small;
        write!(tv.text, "LEVEL {}", self.level).ok();
        self.gam.post_textview(&mut tv).ok();
    }

    /// Centered text in the open row below the ghost house.
    fn draw_banner(&mut self, text: &str) {
        let (x0, y0) = self.cell_origin((0, BANNER_ROW));
        let mut tv = TextView::new(
            self.gid,
            TextBounds::CenteredTop(Rectangle::new(
                Point::new(x0, y0 - 2),
                Point::new(x0 + self.cell * COLS as isize - 1, y0 + self.cell + 4),
            )),
        );
        tv.draw_border = false;
        tv.clear_area = false;
        tv.margin = Point::new(0, 0);
        tv.style = GlyphStyle::Bold;
        tv.text.push_str(text);
        self.gam.post_textview(&mut tv).ok();
    }

    // ---- event entry points -------------------------------------------------------

    /// Pause, if a round is in progress (e.g. when the app loses focus).
    pub(crate) fn pause(&mut self) {
        if self.phase == Phase::Playing {
            self.paused = true;
        }
    }

    fn resume(&mut self) {
        self.paused = false;
        self.redraw_all();
    }

    /// Advance one animation frame. Called from the pump thread.
    pub(crate) fn tick(&mut self) {
        if self.paused {
            return;
        }
        self.frame = self.frame.wrapping_add(1);
        match self.phase {
            Phase::Playing => self.play_frame(),
            Phase::Ready(n) => {
                if n == 0 {
                    self.phase = Phase::Playing;
                    self.redraw_all(); // clears the banner
                } else {
                    self.phase = Phase::Ready(n - 1);
                }
            }
            Phase::EatPause(n) => {
                if n == 0 {
                    self.phase = Phase::Playing;
                    self.draw_frame(); // drops the bonus from the header
                } else {
                    self.phase = Phase::EatPause(n - 1);
                }
            }
            Phase::Dying(n) => {
                let elapsed = DEATH_FRAMES - n;
                let freeze = SEC / 2;
                if elapsed == freeze {
                    self.hide_ghosts = true;
                    self.redraw_all();
                } else if elapsed > freeze && (elapsed - freeze) % 3 == 0 {
                    // shrink away to nothing
                    let radius = self.radius() - ((elapsed - freeze) / 3) as isize;
                    let mut b = Batch::new(&self.gam, self.gid);
                    self.push_cell_bg(&mut b, self.pac.pos);
                    self.push_pac(&mut b, radius);
                    b.flush();
                    drop(b);
                    self.gam.redraw().unwrap();
                }
                if n == 0 {
                    self.lose_life();
                } else {
                    self.phase = Phase::Dying(n - 1);
                }
            }
            Phase::LevelClear(n) => {
                if n == 0 {
                    self.level += 1;
                    self.start_level();
                    self.redraw_all();
                    return;
                }
                if n % 6 == 0 {
                    if n == LEVEL_CLEAR_FRAMES - LEVEL_CLEAR_FRAMES % 6 {
                        self.hide_ghosts = true;
                        self.redraw_all();
                    }
                    // flash the maze walls
                    let color = if (n / 6) % 2 == 0 { PixelColor::Dark } else { PixelColor::Light };
                    let mut b = Batch::new(&self.gam, self.gid);
                    self.push_walls(&mut b, color);
                    b.flush();
                    drop(b);
                    self.gam.redraw().unwrap();
                }
                self.phase = Phase::LevelClear(n - 1);
            }
        }
    }

    /// Handle a single keypress.
    pub(crate) fn key(&mut self, k: char) {
        let dir = match k {
            '←' | 'a' | 'A' => Some(Dir::Left),
            '→' | 'd' | 'D' => Some(Dir::Right),
            '↑' | 'w' | 'W' => Some(Dir::Up),
            '↓' | 's' | 'S' => Some(Dir::Down),
            _ => None,
        };
        if let Some(d) = dir {
            self.pac.want = d;
            if self.paused {
                self.resume();
            }
            return;
        }
        match k {
            'p' | 'P' | ' ' => {
                if self.paused {
                    self.resume();
                } else if self.phase == Phase::Playing {
                    self.paused = true;
                    self.draw_banner("PAUSED");
                    self.gam.redraw().unwrap();
                }
            }
            _ => {}
        }
    }

    /// Full redraw of the whole screen. Used on focus, level start, death, and unpause.
    pub(crate) fn redraw_all(&mut self) {
        self.gam
            .draw_rectangle(
                self.gid,
                Rectangle::new_coords_with_style(
                    0,
                    0,
                    self.screensize.x,
                    self.screensize.y,
                    DrawStyle::new(PixelColor::Light, PixelColor::Light, 0),
                ),
            )
            .expect("couldn't clear screen");

        let mut b = Batch::new(&self.gam, self.gid);
        self.push_walls(&mut b, PixelColor::Dark);
        for r in 0..ROWS {
            for c in 0..COLS {
                match self.tile((c, r)) {
                    Tile::Dot | Tile::Power | Tile::Door => self.push_cell_bg(&mut b, (c, r)),
                    _ => {}
                }
            }
        }
        if self.fruit_frames > 0 {
            self.push_cell_bg(&mut b, FRUIT_POS);
        }
        self.push_actors(&mut b);
        b.flush();
        drop(b);
        self.dirty.clear();

        self.draw_header(true);
        self.draw_footer();
        if self.paused {
            self.draw_banner("PAUSED");
        } else if let Phase::Ready(_) = self.phase {
            self.draw_banner("READY!");
        }
        self.gam.redraw().unwrap();
    }
}

pub(crate) fn paklman_pump_thread(cid_to_main: xous::CID, pump_sid: xous::SID) {
    let _ = std::thread::spawn({
        let cid_to_main = cid_to_main;
        let sid = pump_sid;
        move || {
            let tt = ticktimer_server::Ticktimer::new().unwrap();
            let cid_to_self = xous::connect(sid).unwrap();
            let mut run = true;
            loop {
                let msg = xous::receive_message(sid).unwrap();
                match FromPrimitive::from_usize(msg.body.id()) {
                    Some(PumpOp::Run) => {
                        run = true;
                        xous::send_message(
                            cid_to_self,
                            Message::new_scalar(PumpOp::Pump.to_usize().unwrap(), 0, 0, 0, 0),
                        )
                        .expect("couldn't pump the main loop event thread");
                    }
                    Some(PumpOp::Stop) => run = false,
                    Some(PumpOp::Pump) => {
                        xous::send_message(
                            cid_to_main,
                            Message::new_blocking_scalar(AppOp::Pump.to_usize().unwrap(), 0, 0, 0, 0),
                        )
                        .expect("couldn't pump the main loop event thread");
                        if run {
                            tt.sleep_ms(PAKLMAN_TICK_MS).unwrap();
                            xous::send_message(
                                cid_to_self,
                                Message::new_scalar(PumpOp::Pump.to_usize().unwrap(), 0, 0, 0, 0),
                            )
                            .expect("couldn't pump the main loop event thread");
                        }
                    }
                    Some(PumpOp::Quit) => {
                        xous::return_scalar(msg.sender, 1).expect("couldn't ack the quit message");
                        break;
                    }
                    _ => log::error!("Got unrecognized message: {:?}", msg),
                }
            }
            xous::destroy_server(sid).ok();
        }
    });
}

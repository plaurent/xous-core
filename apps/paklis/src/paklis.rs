use core::fmt::Write as _;

use gam::menu::*; // brings in minigfx: Point, Rectangle, DrawStyle, PixelColor, TextView, GlyphStyle...
use gam::*; // Gam, UxRegistration, GamObjectList, GamObjectType, APP_NAME_PAKLIS, FocusState...

use super::*;

/// Playfield dimensions, in cells.
const COLS: i32 = 10;
const ROWS: i32 = 20;

/// The seven tetromino shapes, each described as four (x, y) cell offsets relative
/// to the piece's pivot. Rotation is computed by rotating these offsets about (0, 0).
/// The `O` piece is special-cased to never rotate.
const SHAPES: [[(i32, i32); 4]; 7] = [
    [(-1, 0), (0, 0), (1, 0), (2, 0)],  // I
    [(0, 0), (1, 0), (0, 1), (1, 1)],   // O
    [(-1, 0), (0, 0), (1, 0), (0, 1)],  // T
    [(0, 0), (1, 0), (-1, 1), (0, 1)],  // S
    [(-1, 0), (0, 0), (0, 1), (1, 1)],  // Z
    [(-1, 0), (0, 0), (1, 0), (-1, 1)], // J
    [(-1, 0), (0, 0), (1, 0), (1, 1)],  // L
];

/// Points awarded for clearing 0..=4 lines at once (classic scoring).
const LINE_SCORE: [u32; 5] = [0, 100, 300, 500, 800];

struct Piece {
    kind: usize,
    /// current rotated offsets from the pivot
    offsets: [(i32, i32); 4],
    /// pivot position in board (column, row) coordinates
    col: i32,
    row: i32,
}

impl Piece {
    fn spawn(kind: usize) -> Self { Piece { kind, offsets: SHAPES[kind], col: COLS / 2, row: 0 } }

    /// Absolute cell coordinates occupied by this piece.
    fn cells(&self) -> [(i32, i32); 4] {
        let mut out = [(0, 0); 4];
        for (i, (dx, dy)) in self.offsets.iter().enumerate() {
            out[i] = (self.col + dx, self.row + dy);
        }
        out
    }

    /// Cells this piece would occupy if its offsets were `offsets` and pivot were (col, row).
    fn cells_at(offsets: &[(i32, i32); 4], col: i32, row: i32) -> [(i32, i32); 4] {
        let mut out = [(0, 0); 4];
        for (i, (dx, dy)) in offsets.iter().enumerate() {
            out[i] = (col + dx, row + dy);
        }
        out
    }

    /// Offsets after a 90-degree clockwise rotation. The `O` piece is left unrotated.
    fn rotated(&self) -> [(i32, i32); 4] {
        if self.kind == 1 {
            return self.offsets;
        }
        let mut out = [(0, 0); 4];
        for (i, (x, y)) in self.offsets.iter().enumerate() {
            // (x, y) -> (-y, x)
            out[i] = (-*y, *x);
        }
        out
    }
}

pub(crate) struct Paklis {
    gam: gam::Gam,
    gid: Gid,
    screensize: Point,
    _token: [u32; 4],
    trng: trng::Trng,
    modals: modals::Modals,

    // geometry, computed once from the canvas size
    cell: isize,
    ox: isize,
    oy: isize,

    // game state
    board: [[bool; COLS as usize]; ROWS as usize],
    piece: Piece,
    lines: u32,
    score: u32,
    over: bool,
}

impl Paklis {
    pub(crate) fn new(sid: xous::SID) -> Self {
        let xns = xous_names::XousNames::new().expect("couldn't connect to Xous Namespace Server");
        let gam = gam::Gam::new(&xns).expect("can't connect to Graphical Abstraction Manager");

        let token = gam
            .register_ux(UxRegistration {
                app_name: String::from(gam::APP_NAME_PAKLIS),
                ux_type: gam::UxType::Framebuffer,
                predictor: None,
                listener: sid.to_array(),
                redraw_id: AppOp::Redraw.to_u32().unwrap(),
                gotinput_id: None,
                audioframe_id: None,
                focuschange_id: Some(AppOp::FocusChange.to_u32().unwrap()),
                rawkeys_id: Some(AppOp::Rawkeys.to_u32().unwrap()),
            })
            .expect("couldn't register Ux context for paklis")
            .unwrap();

        let gid = gam.request_content_canvas(token).expect("couldn't get content canvas");
        let screensize = gam.get_canvas_bounds(gid).expect("couldn't get dimensions of content canvas");

        // Size the cells to fit the canvas, reserving a row of height at the top for the score.
        let cell = core::cmp::min(screensize.x / (COLS as isize + 1), screensize.y / (ROWS as isize + 2));
        let board_w = cell * COLS as isize;
        let board_h = cell * ROWS as isize;
        let ox = (screensize.x - board_w) / 2;
        // leave a little headroom at the top for the score readout
        let oy = ((screensize.y - board_h) / 2).max(cell + 2);

        let trng = trng::Trng::new(&xns).unwrap();
        let modals = modals::Modals::new(&xns).unwrap();

        let mut game = Paklis {
            gam,
            gid,
            screensize,
            _token: token,
            trng,
            modals,
            cell,
            ox,
            oy,
            board: [[false; COLS as usize]; ROWS as usize],
            piece: Piece::spawn(0),
            lines: 0,
            score: 0,
            over: false,
        };
        game.piece = Piece::spawn(game.random_kind());
        game
    }

    fn random_kind(&self) -> usize { (self.trng.get_u32().unwrap() % 7) as usize }

    // ---- collision & board helpers ------------------------------------------------

    /// True if any of `cells` is out of bounds (sides/bottom) or overlaps a locked cell.
    /// Cells above the top of the field (row < 0) are treated as empty space.
    fn collides(&self, cells: &[(i32, i32); 4]) -> bool {
        for &(c, r) in cells.iter() {
            if c < 0 || c >= COLS || r >= ROWS {
                return true;
            }
            if r >= 0 && self.board[r as usize][c as usize] {
                return true;
            }
        }
        false
    }

    // ---- pixel geometry -----------------------------------------------------------

    fn cell_rect(&self, c: i32, r: i32, filled: bool) -> Rectangle {
        let x0 = self.ox + c as isize * self.cell;
        let y0 = self.oy + r as isize * self.cell;
        if filled {
            // inset by 1px so adjacent blocks show a thin light seam between them
            Rectangle::new_coords_with_style(
                x0 + 1,
                y0 + 1,
                x0 + self.cell - 1,
                y0 + self.cell - 1,
                DrawStyle::new(PixelColor::Dark, PixelColor::Dark, 0),
            )
        } else {
            // erase the whole cell back to background
            Rectangle::new_coords_with_style(
                x0,
                y0,
                x0 + self.cell,
                y0 + self.cell,
                DrawStyle::new(PixelColor::Light, PixelColor::Light, 0),
            )
        }
    }

    /// Fast incremental redraw: erase the `old` cells, then draw the `new` cells filled.
    fn blit(&self, old: &[(i32, i32)], new: &[(i32, i32)]) {
        let mut list = GamObjectList::new(self.gid);
        for &(c, r) in old.iter() {
            if r >= 0 {
                list.push(GamObjectType::Rect(self.cell_rect(c, r, false))).ok();
            }
        }
        for &(c, r) in new.iter() {
            if r >= 0 {
                list.push(GamObjectType::Rect(self.cell_rect(c, r, true))).ok();
            }
        }
        self.gam.draw_list(list).expect("couldn't execute draw list");
        self.gam.redraw().unwrap();
    }

    // ---- movement -----------------------------------------------------------------

    fn try_shift(&mut self, dc: i32, dr: i32) -> bool {
        let old = self.piece.cells();
        let candidate = Piece::cells_at(&self.piece.offsets, self.piece.col + dc, self.piece.row + dr);
        if self.collides(&candidate) {
            return false;
        }
        self.piece.col += dc;
        self.piece.row += dr;
        self.blit(&old, &self.piece.cells());
        true
    }

    fn try_rotate(&mut self) {
        let old = self.piece.cells();
        let offsets = self.piece.rotated();
        let candidate = Piece::cells_at(&offsets, self.piece.col, self.piece.row);
        if !self.collides(&candidate) {
            self.piece.offsets = offsets;
            self.blit(&old, &self.piece.cells());
        }
    }

    /// Lock the current piece into the board, clear any full lines, and spawn the next
    /// piece. Triggers a full redraw. Handles game-over.
    fn lock_and_next(&mut self) {
        let cells = self.piece.cells();
        // if any part locks above the top of the field, the game is over
        let mut topped_out = false;
        for &(c, r) in cells.iter() {
            if r < 0 {
                topped_out = true;
            } else {
                self.board[r as usize][c as usize] = true;
            }
        }
        let cleared = self.clear_lines();
        self.score += LINE_SCORE[cleared as usize];
        self.lines += cleared;

        if topped_out {
            self.game_over();
            return;
        }

        // spawn the next piece; if it can't fit, that's game over too
        self.piece = Piece::spawn(self.random_kind());
        if self.collides(&self.piece.cells()) {
            self.game_over();
            return;
        }
        self.redraw_all();
    }

    /// Remove any completely-filled rows, shifting everything above them down.
    /// Returns the number of rows cleared.
    fn clear_lines(&mut self) -> u32 {
        let mut cleared = 0;
        let mut r = ROWS - 1;
        while r >= 0 {
            let full = (0..COLS).all(|c| self.board[r as usize][c as usize]);
            if full {
                // shift every row above `r` down by one
                for src in (1..=r).rev() {
                    self.board[src as usize] = self.board[(src - 1) as usize];
                }
                self.board[0] = [false; COLS as usize];
                cleared += 1;
                // re-examine the same row index, now filled by the row that fell into it
            } else {
                r -= 1;
            }
        }
        cleared
    }

    fn game_over(&mut self) {
        let mut msg = String::new();
        write!(msg, "Game over!\n\nLines: {}\nScore: {}", self.lines, self.score).ok();
        self.modals.show_notification(&msg, None).ok();
        // reset for another round
        self.board = [[false; COLS as usize]; ROWS as usize];
        self.lines = 0;
        self.score = 0;
        self.over = false;
        self.piece = Piece::spawn(self.random_kind());
        self.redraw_all();
    }

    // ---- event entry points -------------------------------------------------------

    /// Advance one gravity step. Called from the pump thread.
    pub(crate) fn tick(&mut self) {
        if self.over {
            return;
        }
        if !self.try_shift(0, 1) {
            self.lock_and_next();
        }
    }

    /// Handle a single keypress.
    pub(crate) fn key(&mut self, k: char) {
        if self.over {
            return;
        }
        match k {
            '←' => {
                self.try_shift(-1, 0);
            }
            '→' => {
                self.try_shift(1, 0);
            }
            '↓' => {
                // soft drop: down one, locking if it can't move
                if !self.try_shift(0, 1) {
                    self.lock_and_next();
                }
            }
            '↑' | '∴' => self.try_rotate(),
            ' ' => {
                // hard drop: fall until blocked, then lock
                while self.try_shift(0, 1) {}
                self.lock_and_next();
            }
            _ => {}
        }
    }

    /// Full redraw of the whole play area. Used on focus, line clears, lock, and reset.
    pub(crate) fn redraw_all(&mut self) {
        // background
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

        // playfield border, drawn just outside the cell grid (no fill so we don't wipe cells)
        let board_w = self.cell * COLS as isize;
        let board_h = self.cell * ROWS as isize;
        self.gam
            .draw_rectangle(
                self.gid,
                Rectangle::new_with_style(
                    Point::new(self.ox - 2, self.oy - 2),
                    Point::new(self.ox + board_w + 1, self.oy + board_h + 1),
                    DrawStyle { fill_color: None, stroke_color: Some(PixelColor::Dark), stroke_width: 2 },
                ),
            )
            .expect("couldn't draw border");

        // locked cells
        for r in 0..ROWS {
            for c in 0..COLS {
                if self.board[r as usize][c as usize] {
                    self.gam.draw_rectangle(self.gid, self.cell_rect(c, r, true)).ok();
                }
            }
        }

        // active piece
        for &(c, r) in self.piece.cells().iter() {
            if r >= 0 {
                self.gam.draw_rectangle(self.gid, self.cell_rect(c, r, true)).ok();
            }
        }

        // score readout above the field
        let mut tv = TextView::new(
            self.gid,
            TextBounds::GrowableFromTl(Point::new(self.ox - 2, 2), (board_w + 4) as u16),
        );
        tv.draw_border = false;
        tv.clear_area = true;
        tv.style = GlyphStyle::Regular;
        write!(tv.text, "Lines {}   Score {}", self.lines, self.score).ok();
        self.gam.post_textview(&mut tv).ok();

        self.gam.redraw().unwrap();
    }
}

pub(crate) fn paklis_pump_thread(cid_to_main: xous::CID, pump_sid: xous::SID) {
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
                            tt.sleep_ms(PAKLIS_TICK_MS).unwrap();
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

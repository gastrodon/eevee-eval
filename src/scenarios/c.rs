use crate::{
    draw_output, render_frame,
    tetris::{draw_footer, next_seed, update_watch, TetrisEngine, TetrisScenario, BOARD_SIZE},
    CommonArgs, Hook, Stats, WatchFn,
};
use clap::Parser;
use core::ops::ControlFlow;
use eevee::{
    activate::relu,
    genome::{Recurrent, WConnection},
    network::{Continuous, FromGenome, ToNetwork},
    Connection, Genome, Network,
};
use std::ffi::c_void;

// ---------------------------------------------------------------------------
// Scenario-specific CLI args
// ---------------------------------------------------------------------------

#[derive(Parser)]
struct TetrisArgs {
    /// Number of games per evaluation (fitness = average score)
    #[arg(long, default_value_t = 1)]
    games: usize,
    /// Starting level (0–29; affects drop speed)
    #[arg(long, default_value_t = 0)]
    level: u8,
}

// ---------------------------------------------------------------------------
// FFI
// ---------------------------------------------------------------------------

type GameState = c_void;

#[link(name = "tetris")]
unsafe extern "C" {
    fn tetris_new(seed: u16) -> *mut GameState;
    fn tetris_free(s: *mut GameState);
    fn tetris_tick(s: *mut GameState, input: i32) -> i32;
    fn tetris_sense(s: *mut GameState, out: *mut f64);
    fn tetris_score(s: *mut GameState) -> i64;
    fn tetris_piece_count(s: *mut GameState) -> i64;
    fn tetris_set_level(s: *mut GameState, level: i32);
}

const TICK_END: i32 = 2;
const ROT_CW: i32 = 0x10;
const ROT_ACW: i32 = 0x20;
const LEFT: i32 = 0x04;
const RIGHT: i32 = 0x08;
const DOWN: i32 = 0x02;
const UP: i32 = 0x01;

const OUTPUT_SIZE: usize = 6;
const ACTIONS: [i32; OUTPUT_SIZE] = [ROT_CW, ROT_ACW, LEFT, RIGHT, DOWN, UP];
const OUTPUT_LABELS: [char; OUTPUT_SIZE] = ['⟳', '⟲', '<', '>', 'v', '^'];

// ---------------------------------------------------------------------------
// Safe wrapper
// ---------------------------------------------------------------------------

struct TetrisGame(*mut GameState);

unsafe impl Send for TetrisGame {}

impl TetrisGame {
    fn new(seed: u16) -> Self {
        Self(unsafe { tetris_new(seed) })
    }
}

impl Drop for TetrisGame {
    fn drop(&mut self) {
        unsafe { tetris_free(self.0) }
    }
}

// ---------------------------------------------------------------------------
// TetrisEngine impl
// ---------------------------------------------------------------------------

struct CEngine {
    game: TetrisGame,
}

impl TetrisEngine for CEngine {
    fn new_game(seed: u16, level: u8) -> Self {
        let game = TetrisGame::new(seed);
        unsafe { tetris_set_level(game.0, level as i32) };
        Self { game }
    }

    fn outputs() -> usize {
        OUTPUT_SIZE
    }

    fn sense(&self, buf: &mut [f64; BOARD_SIZE]) {
        unsafe { tetris_sense(self.game.0, buf.as_mut_ptr()) }
    }

    fn tick(&mut self, outputs: &[f64]) -> bool {
        let input = outputs
            .iter()
            .zip(ACTIONS.iter())
            .filter(|(x, _)| **x >= 0.5)
            .fold(0i32, |acc, (_, flag)| acc | flag);
        unsafe { tetris_tick(self.game.0, input) == TICK_END }
    }

    fn score(&self) -> f64 {
        let points = unsafe { tetris_score(self.game.0) };
        let pieces = unsafe { tetris_piece_count(self.game.0) };
        (points * 256 + pieces) as f64
    }
}

// ---------------------------------------------------------------------------
// Exhibition game (--watch)
// ---------------------------------------------------------------------------

fn run_exhibition_game<C: Connection, G: Genome<C>, NN: Network + FromGenome<C, G>>(genome: G, seed: u16, level: u8) {
    let mut engine = CEngine::new_game(seed, level);
    let mut network: NN = genome.network();
    let mut sense = [0.0f64; BOARD_SIZE];

    loop {
        engine.sense(&mut sense);
        network.step(1, &sense, &relu);
        let outputs = network.output().to_vec();

        print!("\x1b[H");
        print!("{}", render_frame(&sense, 10, false));
        draw_footer(engine.score());
        draw_output(&outputs, &OUTPUT_LABELS);

        if engine.tick(&outputs) {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(dir: &str, common: CommonArgs, extra: Vec<String>) {
    let targs =
        TetrisArgs::parse_from(std::iter::once("tetris-c").chain(extra.iter().map(String::as_str)));

    let watch = common.watch;
    let games = targs.games;
    let level = targs.level;

    type C = WConnection;
    type G = Recurrent<C>;
    type N = Continuous;

    let watch_hook: Hook<C, G> = Box::new(|stats: &mut Stats<C, G>| {
        let max = stats.fittest().map(|(_, f)| *f).unwrap_or(0.0);
        update_watch(stats.generation, max);
        ControlFlow::Continue(())
    });

    let watch_fn: Option<Box<WatchFn<G>>> = if watch {
        Some(Box::new(move |genome| {
            run_exhibition_game::<C, G, N>(genome.clone(), next_seed(), level)
        }))
    } else {
        None
    };

    crate::run(
        TetrisScenario::<CEngine>::new(level, games),
        dir,
        common,
        watch_fn,
        if watch { vec![watch_hook] } else { vec![] },
    );
}

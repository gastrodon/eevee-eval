use crate::{
    draw_output, render_frame,
    tetris::{draw_footer, update_watch, TetrisEngine, TetrisScenario, BOARD_SIZE},
    CommonArgs, Hook, Stats,
};
use clap::Parser;
use core::ops::ControlFlow;
use eevee::{
    activate::relu,
    genome::{Recurrent, WConnection},
    network::{Continuous, ToNetwork},
    Network,
};
use nes_rust_slim::{
    button::Button, default_audio::DefaultAudio, default_display::DefaultDisplay,
    default_input::DefaultInput, rom::Rom, Nes,
};

// ---------------------------------------------------------------------------
// Scenario-specific CLI args
// ---------------------------------------------------------------------------

#[derive(Parser)]
struct TetrisArgs {
    /// RNG seed (0 = random)
    #[arg(long, default_value_t = 0)]
    seed: u16,
    /// Starting level (0–19; affects speed)
    #[arg(long, default_value_t = 0)]
    level: u8,
}

// ---------------------------------------------------------------------------
// NES RAM addresses
// ---------------------------------------------------------------------------

#[rustfmt::skip]
mod v {
    pub const X: usize            = 0x40;
    pub const Y: usize            = 0x41;
    pub const ID: usize           = 0x42;
    pub const GAME_MODE: usize    = 0xc0;
    pub const GAME_OVER: usize    = 0x58;
    pub const SEED_L: usize       = 0x17;
    pub const SEED_R: usize       = 0x18;
    pub const SCORE_1: usize      = 0x53;
    pub const SCORE_2: usize      = 0x54;
    pub const SCORE_3: usize      = 0x55;
    pub const PIECE_COUNT: usize  = 0x1a;
    pub const BOARD_OFFSET: usize = 0x400;
    pub const LEVEL: usize        = 0x64;
}
use v::*;

// ---------------------------------------------------------------------------
// Piece shapes for the falling-piece sense overlay
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const PIECE_SHAPE: [[(u8, u8); 4]; 19] = [
    [(3, 2), (4, 1), (4, 2), (4, 3)], // T_UP
    [(1, 2), (2, 2), (2, 3), (3, 2)], // T_RIGHT
    [(2, 1), (2, 2), (2, 3), (3, 2)], // T_DOWN
    [(1, 2), (2, 1), (2, 2), (3, 2)], // T_LEFT
    [(1, 2), (2, 2), (3, 1), (3, 2)], // J_UP
    [(2, 1), (3, 1), (3, 2), (3, 3)], // J_RIGHT
    [(1, 2), (1, 3), (2, 2), (3, 2)], // J_DOWN
    [(2, 1), (2, 2), (2, 3), (3, 3)], // J_LEFT
    [(2, 1), (2, 2), (3, 2), (3, 3)], // Z_HORIZONTAL
    [(1, 3), (2, 2), (2, 3), (3, 2)], // Z_VERTICAL
    [(2, 1), (2, 2), (3, 1), (3, 2)], // O
    [(2, 2), (2, 3), (3, 1), (3, 2)], // S_HORIZONTAL
    [(1, 2), (2, 2), (2, 3), (3, 3)], // S_VERTICAL
    [(1, 2), (2, 2), (3, 2), (3, 3)], // L_UP
    [(2, 1), (2, 2), (2, 3), (3, 1)], // L_RIGHT
    [(1, 1), (1, 2), (2, 2), (3, 2)], // L_DOWN
    [(2, 3), (3, 1), (3, 2), (3, 3)], // L_LEFT
    [(0, 2), (1, 2), (2, 2), (3, 2)], // I_VERTICAL
    [(3, 0), (3, 1), (3, 2), (3, 3)], // I_HORIZONTAL
];

// joypad order: 0=A 1=B 2=Sel(skip) 3=Start(skip) 4=Up 5=Down 6=Left 7=Right
const OUTPUT_LABELS: [char; 8] = ['a', 'b', 's', 'S', '^', 'v', '<', '>'];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sense_board(ram: &[u8], sense: &mut [f64; BOARD_SIZE]) {
    *sense = [0.; BOARD_SIZE];
    for (idx, _) in ram[BOARD_OFFSET..BOARD_OFFSET + BOARD_SIZE]
        .iter()
        .enumerate()
        .filter(|(_, b)| **b != 0xef)
    {
        sense[idx] = 1.;
    }
    if (0..19).contains(&(ram[ID] as usize)) {
        for index in PIECE_SHAPE[ram[ID] as usize]
            .iter()
            .filter_map(|(row, col)| {
                let row = row + ram[Y];
                let col = col + ram[X];
                (row >= 2 && col >= 2)
                    .then(|| ((row - 2) as usize * 10) + (col - 2) as usize)
            })
            .filter(|i| *i < BOARD_SIZE)
        {
            sense[index] = -1.;
        }
    }
}

fn nes_score(ram: &[u8]) -> f64 {
    (((ram[SCORE_1] as usize) << 8)
        | ((ram[SCORE_2] as usize) << 16)
        | ((ram[SCORE_3] as usize) << 24)
        | (ram[PIECE_COUNT] as usize)) as f64
}

fn make_nes(seed: u16, level: u8) -> Nes {
    let mut nes = Nes::new(
        Box::new(DefaultInput::new()),
        Box::new(DefaultDisplay::new()),
        Box::new(DefaultAudio::new()),
    );
    nes.set_rom(Rom::new(include_bytes!("../../nes-tetris/src/data/tetris.nes").to_vec()));
    nes.bootup();
    while nes.get_cpu().get_ram().data[0xc3] == 0 {
        nes.step_frame();
    }
    nes.get_mut_cpu().get_mut_ram().data[0xc3] = 0;
    while nes.get_cpu().get_ram().data[GAME_MODE] == 0 {
        nes.step_frame();
    }
    while nes.get_cpu().get_ram().data[GAME_MODE] != 4 {
        nes.press_button(Button::Start);
        nes.step_frame();
        nes.release_button(Button::Start);
        nes.step_frame();
    }
    let (lo, hi) = if seed == 0 { (0, 0) } else { (seed as u8, (seed >> 8) as u8) };
    nes.get_mut_cpu().get_mut_ram().data[SEED_L] = lo;
    nes.get_mut_cpu().get_mut_ram().data[SEED_R] = hi;
    nes.get_mut_cpu().get_mut_ram().data[LEVEL] = level.min(19);
    nes
}

// ---------------------------------------------------------------------------
// TetrisEngine impl
// ---------------------------------------------------------------------------

struct NesEngine {
    nes: Nes,
}

impl TetrisEngine for NesEngine {
    fn new_game(seed: u16, level: u8) -> Self {
        Self { nes: make_nes(seed, level) }
    }

    fn outputs() -> usize {
        8
    }

    fn sense(&self, buf: &mut [f64; BOARD_SIZE]) {
        sense_board(&self.nes.get_cpu().get_ram().data, buf);
    }

    fn tick(&mut self, outputs: &[f64]) -> bool {
        for (idx, x) in outputs.iter().enumerate() {
            if idx == 2 || idx == 3 {
                continue;
            }
            self.nes.get_mut_cpu().joypad1.buttons[idx] = *x >= 0.5;
        }
        self.nes.step_frame();
        let done = self.nes.get_cpu().get_ram().data[GAME_OVER] != 0;
        self.nes.get_mut_cpu().joypad1.buttons = [false; 8];
        done
    }

    fn score(&self) -> f64 {
        nes_score(&self.nes.get_cpu().get_ram().data)
    }
}

// ---------------------------------------------------------------------------
// Exhibition game (--watch)
// ---------------------------------------------------------------------------

fn run_exhibition_game(genome: &Recurrent<WConnection>, seed: u16, level: u8) {
    let mut engine = NesEngine::new_game(seed, level);
    let mut network: Continuous = genome.network();
    let mut sense = [0.; BOARD_SIZE];

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
    let targs = TetrisArgs::parse_from(
        std::iter::once("nes-tetris").chain(extra.iter().map(String::as_str)),
    );

    type C = WConnection;
    type G = Recurrent<C>;

    let watch = common.watch;
    let seed = targs.seed;
    let level = targs.level;

    let watch_hook: Hook<C, G> = Box::new(|stats: &mut Stats<C, G>| {
        let max = stats.fittest().map(|(_, f)| *f).unwrap_or(0.0);
        update_watch(stats.generation, max);
        ControlFlow::Continue(())
    });

    let watch_fn: Option<Box<dyn Fn(&G) + Send + 'static>> = if watch {
        Some(Box::new(move |genome| run_exhibition_game(genome, seed, level)))
    } else {
        None
    };

    crate::run(
        TetrisScenario::<NesEngine>::new(seed, level),
        dir,
        common,
        watch_fn,
        if watch { vec![watch_hook] } else { vec![] },
    );
}

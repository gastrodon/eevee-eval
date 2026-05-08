use crate::{draw_output, render_frame, tetris::update_watch, CommonArgs, Hook, Stats, WatchFn};
use clap::Parser;
use core::ops::ControlFlow;
use eevee::{
    activate::relu,
    genome::{connection::BWConnection, Genome, Recurrent, WConnection},
    network::{Continuous, FromGenome, Simple, ToNetwork},
    Connection, Network, Scenario,
};
use nes_rust_slim::{
    button::Button, default_audio::DefaultAudio, default_display::DefaultDisplay,
    default_input::DefaultInput, rom::Rom, Nes,
};

// ---------------------------------------------------------------------------
// Scenario-specific CLI args
// ---------------------------------------------------------------------------

#[derive(Parser)]
struct MarioArgs {
    /// Maximum frames per evaluation episode (~30 s at 60 fps)
    #[arg(long, default_value_t = 1800)]
    max_frames: usize,
}

// ---------------------------------------------------------------------------
// NES RAM addresses
// ---------------------------------------------------------------------------

#[rustfmt::skip]
mod v {
    pub const PLAYER_STATUS: usize = 0x0772; // 3=in-level playing, 1=dying/transition, 0/2=other
    pub const SCREEN_PAGE:   usize = 0x071A; // increments as Mario moves right
    pub const COL_OFFSET:    usize = 0x071C; // column within page
    pub const MARIO_X:       usize = 0x00CE; // screen X pixel
    pub const MARIO_Y:       usize = 0x00B5; // world Y (not the enemy-Y-aliased $00CF)
    pub const XVEL:          usize = 0x009F; // X velocity (signed)
    pub const YVEL:          usize = 0x009D; // Y velocity (signed)
    pub const POWERUP:       usize = 0x0756; // 0=small, 1=big, 2=fire
    pub const AIRBORNE:      usize = 0x001C; // nonzero = in air
    pub const ENEMY_TYPE:    usize = 0x000F; // [5] type per slot, 0=empty
    pub const ENEMY_X:       usize = 0x0087; // [5] enemy X pixel
    pub const ENEMY_Y:       usize = 0x00CF; // [5] enemy Y pixel (slot 0)
}
use v::*;

// 4 outputs: A (jump), B (run), Left, Right — joypad button indices
const BTN_INDICES: [usize; 4] = [0, 1, 6, 7];
const OUTPUT_LABELS: [char; 4] = ['a', 'b', '<', '>'];

// 6 self-state inputs + 5 enemy slots × 3 = 21 total
pub const NUM_INPUTS: usize = 21;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mario_progress(ram: &[u8]) -> f64 {
    (ram[SCREEN_PAGE] as usize * 256 + ram[COL_OFFSET] as usize) as f64
}

fn fill_sense(ram: &[u8], sense: &mut [f64; NUM_INPUTS]) {
    let mx = ram[MARIO_X] as f64;
    let my = ram[MARIO_Y] as f64;
    sense[0] = mx / 255.0;
    sense[1] = my / 255.0;
    sense[2] = (ram[XVEL] as i8 as f64) / 64.0;
    sense[3] = (ram[YVEL] as i8 as f64) / 64.0;
    sense[4] = ram[POWERUP] as f64 / 2.0;
    sense[5] = if ram[AIRBORNE] != 0 { 1.0 } else { 0.0 };
    for i in 0..5 {
        let b = 6 + i * 3;
        let alive = ram[ENEMY_TYPE + i] != 0;
        sense[b] = if alive { 1.0 } else { 0.0 };
        if alive {
            sense[b + 1] = ((ram[ENEMY_X + i] as f64 - mx) / 255.0).clamp(-1.0, 1.0);
            sense[b + 2] = ((ram[ENEMY_Y + i] as f64 - my) / 255.0).clamp(-1.0, 1.0);
        } else {
            sense[b + 1] = 0.0;
            sense[b + 2] = 0.0;
        }
    }
}

fn make_nes() -> Nes {
    let mut nes = Nes::new(
        Box::new(DefaultInput::new()),
        Box::new(DefaultDisplay::new()),
        Box::new(DefaultAudio::new()),
    );
    nes.set_rom(Rom::new(include_bytes!("../../super-mario.nes").to_vec()));
    nes.bootup();

    // ~35 frames to reach the title screen ($0772 settles to 3 at ~frame 31).
    for _ in 0..35 {
        nes.step_frame();
    }

    // Press Start to begin the 1-player game.
    nes.press_button(Button::Start);
    nes.step_frame();
    nes.release_button(Button::Start);

    // Step through:
    //   ~155 frames – World 1-1 intro card ($0772=1)
    //   ~25 frames  – Level-load freeze (Mario placed but input frozen)
    //   ~40 frames  – Safety margin
    // After 220 frames Mario is standing at the start of 1-1, fully controllable.
    for _ in 0..220 {
        nes.step_frame();
    }

    nes
}

fn apply_outputs(nes: &mut Nes, outputs: &[f64]) {
    for (out_idx, &btn_idx) in BTN_INDICES.iter().enumerate() {
        nes.get_mut_cpu().joypad1.buttons[btn_idx] = outputs[out_idx] >= 0.5;
    }
}

// ---------------------------------------------------------------------------
// Scenario
// ---------------------------------------------------------------------------

pub struct MarioScenario {
    max_frames: usize,
}

impl MarioScenario {
    fn new(max_frames: usize) -> Self {
        Self { max_frames }
    }
}

impl<C, G, A> Scenario<C, G, A> for MarioScenario
where
    C: Connection,
    G: Genome<C> + ToNetwork<Continuous, C>,
    A: Fn(f64) -> f64,
{
    fn io(&self) -> (usize, usize) {
        (NUM_INPUTS, 4)
    }

    fn eval(&self, genome: &G, σ: &A) -> f64 {
        let mut nes = make_nes();
        let mut network = genome.network();
        let mut sense = [0.0f64; NUM_INPUTS];
        let mut last_progress = mario_progress(&nes.get_cpu().get_ram().data);
        let mut stall = 0usize;

        #[cfg(feature = "x11nes")]
        WIN.with(|cell| {
            let mut opt = cell.borrow_mut();
            if opt.is_none() {
                *opt = Some(WinState::new());
            }
        });

        for _ in 0..self.max_frames {
            fill_sense(&nes.get_cpu().get_ram().data, &mut sense);
            network.step(1, &sense, σ);
            let outputs = network.output().to_vec();
            apply_outputs(&mut nes, &outputs);
            nes.step_frame();
            nes.get_mut_cpu().joypad1.buttons = [false; 8];

            #[cfg(feature = "x11nes")]
            WIN.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    if state.window.is_open() {
                        nes.copy_pixels(&mut state.rgba);
                        for (i, px) in state.fb.iter_mut().enumerate() {
                            *px = (state.rgba[i * 4 + 2] as u32) << 16
                                | (state.rgba[i * 4 + 1] as u32) << 8
                                | state.rgba[i * 4 + 0] as u32;
                        }
                        let _ = state.window.update_with_buffer(&state.fb, 256, 240);
                    }
                }
            });

            let ram = &nes.get_cpu().get_ram().data;
            // $0772=3 is in-level play; anything else is dying/transition/game-over
            if ram[PLAYER_STATUS] != 3 {
                break;
            }
            let p = mario_progress(ram);
            if p > last_progress {
                last_progress = p;
                stall = 0;
            } else {
                stall += 1;
                if stall >= 60 {
                    break;
                }
            }
        }
        last_progress
    }
}

// ---------------------------------------------------------------------------
// Exhibition (--watch mode)
// ---------------------------------------------------------------------------

fn draw_mario_footer(progress: f64) {
    let (gen, _) = crate::tetris::read_watch();
    let left = format!("{:.0}", progress);
    let right = format!("gen {}", gen);
    let inner = (left.len() + 1 + right.len()).max(8);
    let spaces = inner - left.len() - right.len();
    println!("|{}|", "-".repeat(inner));
    println!("|{}{}{}|", left, " ".repeat(spaces), right);
}

#[cfg(feature = "x11nes")]
struct WinState {
    window: minifb::Window,
    rgba: Vec<u8>,
    fb: Vec<u32>,
}

#[cfg(feature = "x11nes")]
impl WinState {
    fn new() -> Self {
        Self {
            window: minifb::Window::new(
                "Super Mario Bros (NES)",
                256,
                240,
                minifb::WindowOptions {
                    scale: minifb::Scale::X2,
                    ..Default::default()
                },
            )
            .expect("failed to open NES window"),
            rgba: vec![0u8; 256 * 240 * 4],
            fb: vec![0u32; 256 * 240],
        }
    }
}

#[cfg(feature = "x11nes")]
thread_local! {
    static WIN: std::cell::RefCell<Option<WinState>> = std::cell::RefCell::new(None);
}

fn run_exhibition<C: Connection, G: Genome<C>, NN: Network + FromGenome<C, G>>(genome: G) {
    let mut nes = make_nes();
    let mut network: NN = genome.network();
    let mut sense = [0.0f64; NUM_INPUTS];
    let mut last_progress = mario_progress(&nes.get_cpu().get_ram().data);
    let mut stall = 0usize;

    for _ in 0..3_600 {
        fill_sense(&nes.get_cpu().get_ram().data, &mut sense);
        network.step(1, &sense, &relu);
        let outputs = network.output().to_vec();

        print!("\x1b[H");
        print!("{}", render_frame(&sense, 7, true));
        draw_mario_footer(mario_progress(&nes.get_cpu().get_ram().data));
        draw_output(&outputs, &OUTPUT_LABELS);

        apply_outputs(&mut nes, &outputs);
        nes.step_frame();
        nes.get_mut_cpu().joypad1.buttons = [false; 8];

        #[cfg(feature = "x11nes")]
        WIN.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                if state.window.is_open() {
                    nes.copy_pixels(&mut state.rgba);
                    // copy_to_rgba_pixels produces BGRA; minifb wants 0x00RRGGBB
                    for (i, px) in state.fb.iter_mut().enumerate() {
                        *px = (state.rgba[i * 4 + 2] as u32) << 16
                            | (state.rgba[i * 4 + 1] as u32) << 8
                            | state.rgba[i * 4 + 0] as u32;
                    }
                    let _ = state.window.update_with_buffer(&state.fb, 256, 240);
                }
            }
        });

        let ram = &nes.get_cpu().get_ram().data;
        if ram[PLAYER_STATUS] != 3 {
            break;
        }
        let p = mario_progress(ram);
        if p > last_progress {
            last_progress = p;
            stall = 0;
        } else {
            stall += 1;
        }
        if stall >= 60 {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(dir: &str, common: CommonArgs, extra: Vec<String>) {
    let margs =
        MarioArgs::parse_from(std::iter::once("nes-mario").chain(extra.iter().map(String::as_str)));

    type C = BWConnection;
    type G = Recurrent<C>;
    type NN = Simple<C>;

    let watch = common.watch;
    let max_frames = margs.max_frames;

    let watch_hook: Hook<C, G> = Box::new(|stats: &mut Stats<C, G>| {
        let max = stats.fittest().map(|(_, f)| *f).unwrap_or(0.0);
        update_watch(stats.generation, max);
        ControlFlow::Continue(())
    });

    let watch_fn: Option<Box<WatchFn<G>>> = if watch {
        Some(Box::new(move |genome| run_exhibition::<_, _, NN>(genome.clone())))
    } else {
        None
    };

    crate::run(
        MarioScenario::new(max_frames),
        dir,
        common,
        watch_fn,
        if watch { vec![watch_hook] } else { vec![] },
    );
}

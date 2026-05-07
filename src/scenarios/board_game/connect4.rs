use super::{board_game_run, C, G};
use crate::CommonArgs;
use board_game::board::{Board, Outcome, Player};
use board_game::games::connect4::Connect4;
use eevee::{
    network::{Continuous, Network, ToNetwork},
    Scenario,
};
use rand::{Rng, RngCore};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};

const GAMES_PER_EVAL: usize = 8;
const NETWORK_PREC: usize = 20;

const WIDTH: usize = 7;
const HEIGHT: usize = 6;
const CELLS: usize = WIDTH * HEIGHT;
const INPUT_DIM: usize = CELLS * 2;
const OUTPUT_DIM: usize = WIDTH;

#[derive(Clone)]
struct ShadowBoard {
    cells: [Option<Player>; CELLS],
    heights: [u8; WIDTH],
}

impl Default for ShadowBoard {
    fn default() -> Self {
        Self { cells: [None; CELLS], heights: [0; WIDTH] }
    }
}

impl ShadowBoard {
    fn drop(&mut self, col: u8, player: Player) {
        let row = self.heights[col as usize] as usize;
        self.cells[row * WIDTH + col as usize] = Some(player);
        self.heights[col as usize] += 1;
    }

    fn cell(&self, row: usize, col: usize) -> Option<Player> {
        self.cells[row * WIDTH + col]
    }
}

fn encode_board(shadow: &ShadowBoard, viewpoint: Player) -> [f64; INPUT_DIM] {
    let mut out = [0.0f64; INPUT_DIM];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let i = row * WIDTH + col;
            match shadow.cell(row, col) {
                Some(p) if p == viewpoint => out[i] = 1.0,
                Some(_) => out[i + CELLS] = 1.0,
                None => {}
            }
        }
    }
    out
}

fn legal_columns(board: &Connect4) -> Vec<u8> {
    if board.is_done() {
        return vec![];
    }
    (0..WIDTH as u8)
        .filter(|&c| board.is_available_move(c).unwrap_or(false))
        .collect()
}

fn network_move<A: Fn(f64) -> f64>(
    network: &mut Continuous,
    board: &Connect4,
    shadow: &ShadowBoard,
    viewpoint: Player,
    σ: &A,
) -> Option<u8> {
    let legal = legal_columns(board);
    if legal.is_empty() {
        return None;
    }
    let input = encode_board(shadow, viewpoint);
    network.step(NETWORK_PREC, &input, σ);
    let output = network.output();

    let mut best = legal[0];
    let mut best_score = output[best as usize];
    for &col in &legal[1..] {
        let score = output[col as usize];
        if score > best_score {
            best = col;
            best_score = score;
        }
    }
    Some(best)
}

fn random_move<R: RngCore>(board: &Connect4, rng: &mut R) -> Option<u8> {
    let legal = legal_columns(board);
    if legal.is_empty() {
        None
    } else {
        Some(legal[rng.random_range(0..legal.len())])
    }
}

fn play_game<A: Fn(f64) -> f64>(
    learner: &mut Continuous,
    learner_player: Player,
    opponent: Option<&mut Continuous>,
    σ: &A,
    rng: &mut eevee::random::WyRng,
) -> f64 {
    let mut board = Connect4::default();
    let mut shadow = ShadowBoard::default();
    learner.flush();
    let mut opponent = opponent;
    if let Some(o) = opponent.as_deref_mut() {
        o.flush();
    }

    while !board.is_done() {
        let mover = board.next_player();
        let mv = if mover == learner_player {
            network_move(learner, &board, &shadow, learner_player, σ)
        } else {
            match opponent.as_deref_mut() {
                Some(o) => network_move(o, &board, &shadow, mover, σ),
                None => random_move(&board, rng),
            }
        };
        match mv {
            Some(col) => {
                board.play(col).expect("legal move");
                shadow.drop(col, mover);
            }
            None => break,
        }
    }

    match board.outcome() {
        Some(Outcome::WonBy(p)) if p == learner_player => 1.0,
        Some(Outcome::Draw) => 0.5,
        _ => 0.0,
    }
}

struct Connect4Scenario {
    pool: Arc<RwLock<Vec<G>>>,
    seed_counter: AtomicU64,
}

impl Connect4Scenario {
    fn new(pool: Arc<RwLock<Vec<G>>>, base_seed: u64) -> Self {
        Self { pool, seed_counter: AtomicU64::new(base_seed) }
    }
}

impl<A: Fn(f64) -> f64> Scenario<C, G, A> for Connect4Scenario {
    fn io(&self) -> (usize, usize) {
        (INPUT_DIM, OUTPUT_DIM)
    }

    fn eval(&self, genome: &G, σ: &A) -> f64 {
        use eevee::random::WyRng;
        let seed = self.seed_counter.fetch_add(1, Ordering::Relaxed);
        let mut rng = WyRng::seeded(seed);

        let opponents: Vec<G> = {
            let pool = self.pool.read().unwrap();
            if pool.is_empty() {
                vec![]
            } else {
                (0..GAMES_PER_EVAL)
                    .map(|_| pool[rng.random_range(0..pool.len())].clone())
                    .collect()
            }
        };

        let mut learner = genome.network();
        let mut total = 0.0;

        for i in 0..GAMES_PER_EVAL {
            let learner_player = if i % 2 == 0 { Player::A } else { Player::B };
            let score = if opponents.is_empty() {
                play_game(&mut learner, learner_player, None, σ, &mut rng)
            } else {
                let mut opp = opponents[i % opponents.len()].network();
                play_game(&mut learner, learner_player, Some(&mut opp), σ, &mut rng)
            };
            total += score;
        }

        total / GAMES_PER_EVAL as f64
    }
}

pub fn run(dir: &str, common: CommonArgs, _extra: Vec<String>) {
    use eevee::random::seed_urandom;
    let base_seed = seed_urandom().unwrap();
    let pool = Arc::new(RwLock::new(vec![]));
    let scenario = Connect4Scenario::new(Arc::clone(&pool), base_seed);
    board_game_run(scenario, pool, dir, common);
}

use super::{board_game_run, CoEvolGame, CoEvolScenario, C, G};
use crate::CommonArgs;
use board_game::board::{Board, BoardMoves, Outcome, Player};
use board_game::games::ataxx::{AtaxxBoard, Move};
use board_game::util::coord::Coord8;
use eevee::{
    network::{Continuous, FromGenome, Network, ToNetwork},
    random::WyRng,
};
use internal_iterator::InternalIterator;
use rand::{Rng, RngCore};
use std::sync::{Arc, RwLock};

const NETWORK_PREC: usize = 20;

const BOARD_SIZE: u8 = 7;
const CELLS: usize = (BOARD_SIZE as usize) * (BOARD_SIZE as usize);
const INPUT_DIM: usize = CELLS * 2;
const OUTPUT_DIM: usize = CELLS;

const JUMP_PENALTY: f64 = 0.01;

fn cell_index(coord: Coord8) -> usize {
    coord.dense_index(BOARD_SIZE)
}

fn encode_board(board: &AtaxxBoard, viewpoint: Player) -> [f64; INPUT_DIM] {
    let mut out = [0.0f64; INPUT_DIM];
    for y in 0..BOARD_SIZE {
        for x in 0..BOARD_SIZE {
            let coord = Coord8::from_xy(x, y);
            if !board.valid_coord(coord) {
                continue;
            }
            let idx = cell_index(coord);
            match board.tile(coord) {
                Some(p) if p == viewpoint => out[idx] = 1.0,
                Some(_) => out[idx + CELLS] = 1.0,
                None => {}
            }
        }
    }
    out
}

fn legal_moves(board: &AtaxxBoard) -> Vec<Move> {
    if board.is_done() {
        return vec![];
    }
    let iter = match board.available_moves() {
        Ok(it) => it,
        Err(_) => return vec![],
    };
    let mut out = Vec::new();
    iter.for_each(|m| out.push(m));
    out
}

fn score_move(output: &[f64], mv: Move) -> f64 {
    match mv {
        Move::Pass => f64::NEG_INFINITY,
        Move::Copy { to } => output[cell_index(to)],
        Move::Jump { to, .. } => output[cell_index(to)] - JUMP_PENALTY,
    }
}

fn network_move<NN: Network, A: Fn(f64) -> f64>(
    network: &mut NN,
    board: &AtaxxBoard,
    viewpoint: Player,
    σ: &A,
) -> Option<Move> {
    let legal = legal_moves(board);
    if legal.is_empty() {
        return None;
    }
    if legal.len() == 1 {
        return Some(legal[0]);
    }
    let input = encode_board(board, viewpoint);
    network.step(NETWORK_PREC, &input, σ);
    let output = network.output();

    let mut best = legal[0];
    let mut best_score = score_move(output, best);
    for &m in &legal[1..] {
        let s = score_move(output, m);
        if s > best_score {
            best = m;
            best_score = s;
        }
    }
    Some(best)
}

fn random_move<R: RngCore>(board: &AtaxxBoard, rng: &mut R) -> Option<Move> {
    let legal = legal_moves(board);
    if legal.is_empty() {
        None
    } else {
        Some(legal[rng.random_range(0..legal.len())])
    }
}

fn play_game<NN: Network, A: Fn(f64) -> f64>(
    learner: &mut NN,
    learner_player: Player,
    opponent: Option<&mut NN>,
    σ: &A,
    rng: &mut eevee::random::WyRng,
) -> f64 {
    let mut board = AtaxxBoard::diagonal(BOARD_SIZE);
    learner.flush();
    let mut opponent = opponent;
    if let Some(o) = opponent.as_deref_mut() {
        o.flush();
    }

    while !board.is_done() {
        let mover = board.next_player();
        let mv = if mover == learner_player {
            network_move(learner, &board, learner_player, σ)
        } else {
            match opponent.as_deref_mut() {
                Some(o) => network_move(o, &board, mover, σ),
                None => random_move(&board, rng),
            }
        };
        match mv {
            Some(m) => {
                board.play(m).expect("legal move");
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

pub struct AtaxxGame;

impl CoEvolGame for AtaxxGame {
    const GAMES_PER_EVAL: usize = 2;
    fn io() -> (usize, usize) { (INPUT_DIM, OUTPUT_DIM) }
    fn play<NN: Network, A: Fn(f64) -> f64>(
        learner: &mut NN,
        learner_player: Player,
        opponent: Option<&mut NN>,
        σ: &A,
        rng: &mut WyRng,
    ) -> f64 {
        play_game(learner, learner_player, opponent, σ, rng)
    }
}

fn render_ataxx(board: &AtaxxBoard) {
    print!("\x1b[H");
    for y in 0..BOARD_SIZE {
        for x in 0..BOARD_SIZE {
            if x > 0 {
                print!(" ");
            }
            let coord = Coord8::from_xy(x, y);
            let ch = if !board.valid_coord(coord) {
                '■'
            } else {
                match board.tile(coord) {
                    Some(p) if p == Player::A => '░',
                    Some(_) => '▓',
                    None => '·',
                }
            };
            print!("{}", ch);
        }
        println!();
    }
    println!();
}

fn run_exhibition_game<NN: Network + FromGenome<C, G>>(genome: &G) {
    use eevee::network::activate::steep_sigmoid;
    let mut board = AtaxxBoard::diagonal(BOARD_SIZE);
    let mut net_a: NN = genome.network();
    let mut net_b: NN = genome.network();
    net_a.flush();
    net_b.flush();
    render_ataxx(&board);
    let mut plies = 0usize;
    while !board.is_done() && plies < 200 {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let mover = board.next_player();
        let net = if mover == Player::A { &mut net_a } else { &mut net_b };
        match network_move(net, &board, mover, &steep_sigmoid) {
            Some(mv) => {
                board.play(mv).ok();
                plies += 1;
            }
            None => break,
        }
        render_ataxx(&board);
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
}

pub fn run(dir: &str, common: CommonArgs, _extra: Vec<String>) {
    use eevee::random::seed_urandom;
    type N = Continuous;
    let base_seed = seed_urandom().unwrap();
    let pool = Arc::new(RwLock::new(vec![]));
    let scenario = CoEvolScenario::<AtaxxGame, N>::new(Arc::clone(&pool), base_seed);
    let watch_fn: Option<Box<dyn Fn(&G) + Send + 'static>> = if common.watch {
        Some(Box::new(|genome: &G| run_exhibition_game::<N>(genome)))
    } else {
        None
    };
    board_game_run(scenario, pool, dir, common, watch_fn);
}

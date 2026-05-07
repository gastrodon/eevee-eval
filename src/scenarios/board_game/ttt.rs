use super::{board_game_run, C, G};
use crate::CommonArgs;
use board_game::board::{Board, Outcome, Player};
use board_game::games::ttt::TTTBoard;
use board_game::util::coord::Coord3;
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

fn coord(i: usize) -> Coord3 {
    Coord3::from_xy((i % 3) as u8, (i / 3) as u8)
}

fn encode_board(board: &TTTBoard, viewpoint: Player) -> [f64; 18] {
    let mut out = [0.0f64; 18];
    for i in 0..9 {
        match board.tile(coord(i)) {
            Some(p) if p == viewpoint => out[i] = 1.0,
            Some(_) => out[i + 9] = 1.0,
            None => {}
        }
    }
    out
}

fn legal_moves(board: &TTTBoard) -> Vec<Coord3> {
    if board.is_done() {
        return vec![];
    }
    (0..9)
        .map(coord)
        .filter(|c| board.is_available_move(*c).unwrap_or(false))
        .collect()
}

fn network_move<A: Fn(f64) -> f64>(
    network: &mut Continuous,
    board: &TTTBoard,
    viewpoint: Player,
    σ: &A,
) -> Option<Coord3> {
    let legal = legal_moves(board);
    if legal.is_empty() {
        return None;
    }
    let input = encode_board(board, viewpoint);
    network.step(NETWORK_PREC, &input, σ);
    let output = network.output();

    let mut best = legal[0];
    let mut best_score = output[(best.x() + 3 * best.y()) as usize];
    for &c in &legal[1..] {
        let score = output[(c.x() + 3 * c.y()) as usize];
        if score > best_score {
            best = c;
            best_score = score;
        }
    }
    Some(best)
}

fn random_move<R: RngCore>(board: &TTTBoard, rng: &mut R) -> Option<Coord3> {
    let legal = legal_moves(board);
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
    let mut board = TTTBoard::default();
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

struct TttScenario {
    pool: Arc<RwLock<Vec<G>>>,
    seed_counter: AtomicU64,
}

impl TttScenario {
    fn new(pool: Arc<RwLock<Vec<G>>>, base_seed: u64) -> Self {
        Self { pool, seed_counter: AtomicU64::new(base_seed) }
    }
}

impl<A: Fn(f64) -> f64> Scenario<C, G, A> for TttScenario {
    fn io(&self) -> (usize, usize) {
        (18, 9)
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
    let scenario = TttScenario::new(Arc::clone(&pool), base_seed);
    board_game_run(scenario, pool, dir, common);
}

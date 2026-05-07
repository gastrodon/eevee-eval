use super::{board_game_run, C, G};
use crate::CommonArgs;
use board_game::board::{Board, Outcome, Player};
use board_game::games::oware::OwareBoard;
use eevee::{
    network::{Continuous, Network, ToNetwork},
    Scenario,
};
use rand::{Rng, RngCore};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};

const GAMES_PER_EVAL: usize = 6;
const NETWORK_PREC: usize = 20;
const MAX_PLIES: usize = 200;

const PITS: usize = 6;
const TOTAL_SEEDS: f64 = (2 * PITS) as f64 * 4.0;
const INPUT_DIM: usize = 2 * PITS + 2;
const OUTPUT_DIM: usize = PITS;

type Game = OwareBoard<PITS>;

fn encode_board(board: &Game, viewpoint: Player) -> [f64; INPUT_DIM] {
    let mut out = [0.0f64; INPUT_DIM];
    let opp = viewpoint.other();
    for i in 0..PITS {
        out[i] = board.get_seeds(viewpoint, i) as f64 / TOTAL_SEEDS;
        out[i + PITS] = board.get_seeds(opp, i) as f64 / TOTAL_SEEDS;
    }
    out[2 * PITS] = board.score(viewpoint) as f64 / TOTAL_SEEDS;
    out[2 * PITS + 1] = board.score(opp) as f64 / TOTAL_SEEDS;
    out
}

fn legal_pits(board: &Game) -> Vec<usize> {
    if board.is_done() {
        return vec![];
    }
    (0..PITS)
        .filter(|&p| board.is_available_move(p).unwrap_or(false))
        .collect()
}

fn network_move<A: Fn(f64) -> f64>(
    network: &mut Continuous,
    board: &Game,
    viewpoint: Player,
    σ: &A,
) -> Option<usize> {
    let legal = legal_pits(board);
    if legal.is_empty() {
        return None;
    }
    let input = encode_board(board, viewpoint);
    network.step(NETWORK_PREC, &input, σ);
    let output = network.output();

    let mut best = legal[0];
    let mut best_score = output[best];
    for &p in &legal[1..] {
        if output[p] > best_score {
            best = p;
            best_score = output[p];
        }
    }
    Some(best)
}

fn random_move<R: RngCore>(board: &Game, rng: &mut R) -> Option<usize> {
    let legal = legal_pits(board);
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
    let mut board = Game::default();
    learner.flush();
    let mut opponent = opponent;
    if let Some(o) = opponent.as_deref_mut() {
        o.flush();
    }

    let mut plies = 0;
    while !board.is_done() && plies < MAX_PLIES {
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
            Some(p) => {
                board.play(p).expect("legal move");
                plies += 1;
            }
            None => break,
        }
    }

    if let Some(outcome) = board.outcome() {
        match outcome {
            Outcome::WonBy(p) if p == learner_player => 1.0,
            Outcome::Draw => 0.5,
            _ => 0.0,
        }
    } else {
        let own = board.score(learner_player) as f64;
        let opp = board.score(learner_player.other()) as f64;
        let total = own + opp;
        if total == 0.0 { 0.5 } else { own / total }
    }
}

struct OwareScenario {
    pool: Arc<RwLock<Vec<G>>>,
    seed_counter: AtomicU64,
}

impl OwareScenario {
    fn new(pool: Arc<RwLock<Vec<G>>>, base_seed: u64) -> Self {
        Self { pool, seed_counter: AtomicU64::new(base_seed) }
    }
}

impl<A: Fn(f64) -> f64> Scenario<C, G, A> for OwareScenario {
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

const OWARE_DIGITS: [char; 10] = ['🯰', '🯱', '🯲', '🯳', '🯴', '🯵', '🯶', '🯷', '🯸', '🯹'];
const OWARE_OVER: char = '🮯';

fn oware_digit(n: u32) -> char {
    if n <= 9 { OWARE_DIGITS[n as usize] } else { OWARE_OVER }
}

fn render_oware(board: &Game) {
    print!("\x1b[H");
    // B's pits reversed (traditional mancala layout: B faces A across the board)
    print!("B ");
    for i in (0..PITS).rev() {
        print!("{}", oware_digit(board.get_seeds(Player::B, i) as u32));
    }
    println!(" {}", oware_digit(board.score(Player::B) as u32));
    // A's pits forward
    print!("A ");
    for i in 0..PITS {
        print!("{}", oware_digit(board.get_seeds(Player::A, i) as u32));
    }
    println!(" {}", oware_digit(board.score(Player::A) as u32));
    println!();
}

fn run_exhibition_game(genome: &G) {
    use eevee::network::activate::steep_sigmoid;
    let mut board = Game::default();
    let mut net_a: Continuous = genome.network();
    let mut net_b: Continuous = genome.network();
    net_a.flush();
    net_b.flush();
    render_oware(&board);
    let mut plies = 0usize;
    while !board.is_done() && plies < MAX_PLIES {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mover = board.next_player();
        let net = if mover == Player::A { &mut net_a } else { &mut net_b };
        match network_move(net, &board, mover, &steep_sigmoid) {
            Some(pit) => {
                board.play(pit).ok();
                plies += 1;
            }
            None => break,
        }
        render_oware(&board);
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
}

pub fn run(dir: &str, common: CommonArgs, _extra: Vec<String>) {
    use eevee::random::seed_urandom;
    let base_seed = seed_urandom().unwrap();
    let pool = Arc::new(RwLock::new(vec![]));
    let scenario = OwareScenario::new(Arc::clone(&pool), base_seed);
    let watch_fn: Option<Box<dyn Fn(&G) + Send + 'static>> = if common.watch {
        Some(Box::new(|genome: &G| run_exhibition_game(genome)))
    } else {
        None
    };
    board_game_run(scenario, pool, dir, common, watch_fn);
}

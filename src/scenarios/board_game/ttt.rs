use super::{board_game_run, CoEvolGame, CoEvolScenario, C, G};
use crate::WatchFn;
use crate::CommonArgs;
use board_game::board::{Board, Outcome, Player};
use board_game::games::ttt::TTTBoard;
use board_game::util::coord::Coord3;
use eevee::{
    network::{Continuous, FromGenome, Network, Realtime, ToNetwork},
    random::WyRng,
};
use rand::{Rng, RngCore};
use std::sync::{Arc, RwLock};

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

fn network_move<NN: Network, A: Fn(f64) -> f64>(
    network: &mut NN,
    board: &TTTBoard,
    viewpoint: Player,
    σ: &A,
) -> Option<Coord3> {
    let legal = legal_moves(board);
    if legal.is_empty() {
        return None;
    }
    let input = encode_board(board, viewpoint);
    network.step(&input, σ);
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

fn play_game<NN: Network + Continuous, A: Fn(f64) -> f64>(
    learner: &mut NN,
    learner_player: Player,
    opponent: Option<&mut NN>,
    σ: &A,
    rng: &mut eevee::random::WyRng,
) -> f64 {
    let mut board = TTTBoard::default();
    learner.reset();
    let mut opponent = opponent;
    if let Some(o) = opponent.as_deref_mut() {
        o.reset();
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

pub struct TttGame;

impl CoEvolGame for TttGame {
    const GAMES_PER_EVAL: usize = 8;
    fn io() -> (usize, usize) { (18, 9) }
    fn play<NN: Network + Continuous, A: Fn(f64) -> f64>(
        learner: &mut NN,
        learner_player: Player,
        opponent: Option<&mut NN>,
        σ: &A,
        rng: &mut WyRng,
    ) -> f64 {
        play_game(learner, learner_player, opponent, σ, rng)
    }
}

fn render_ttt(board: &TTTBoard) {
    print!("\x1b[H");
    for row in 0u8..3 {
        for col in 0u8..3 {
            if col > 0 {
                print!(" ");
            }
            let ch = match board.tile(Coord3::from_xy(col, row)) {
                Some(p) if p == Player::A => '░',
                Some(_) => '▓',
                None => '·',
            };
            print!("{}", ch);
        }
        println!();
    }
    println!();
}

fn run_exhibition_game<NN: Network + Continuous + FromGenome<C, G>>(genome: &G) {
    use eevee::network::activate::steep_sigmoid;
    let mut board = TTTBoard::default();
    let mut net_a: NN = genome.network();
    let mut net_b: NN = genome.network();
    net_a.reset();
    net_b.reset();
    render_ttt(&board);
    while !board.is_done() {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let mover = board.next_player();
        let net = if mover == Player::A { &mut net_a } else { &mut net_b };
        match network_move(net, &board, mover, &steep_sigmoid) {
            Some(mv) => { board.play(mv).ok(); }
            None => break,
        }
        render_ttt(&board);
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
}

pub fn run(dir: &str, common: CommonArgs, _extra: Vec<String>) {
    use eevee::random::seed_urandom;
    type N = Realtime;
    let base_seed = seed_urandom().unwrap();
    let pool = Arc::new(RwLock::new(vec![]));
    let scenario = CoEvolScenario::<TttGame, N>::new(Arc::clone(&pool), base_seed);
    let watch_fn: Option<Box<WatchFn<G>>> = if common.watch {
        Some(Box::new(|genome: &G| run_exhibition_game::<N>(genome)))
    } else {
        None
    };
    board_game_run(scenario, pool, dir, common, watch_fn);
}

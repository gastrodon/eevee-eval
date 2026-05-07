fn main() {
    eevee_eval::cli_run(&[
        ("nes-tetris", eevee_eval::scenarios::nes::run),
        ("tetris-c", eevee_eval::scenarios::c::run),
        ("tic-tac-toe", eevee_eval::scenarios::board_game::ttt::run),
        ("connect4", eevee_eval::scenarios::board_game::connect4::run),
        ("ataxx", eevee_eval::scenarios::board_game::ataxx::run),
        ("oware", eevee_eval::scenarios::board_game::oware::run),
    ]);
}

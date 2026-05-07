/*
 * tetris_ffi.c — thread-safe FFI shim for Rust/eevee NEAT integration
 *
 * Game engine source: cb2078/tetris-ai
 * Vendored: board.c (shape data, collision, line-clear)
 *
 * All mutable state lives in tetris_ctx — no globals written at runtime.
 * Safe to call tetris_new / tetris_tick / tetris_sense from multiple threads
 * simultaneously (required for eevee's `parallel` feature).
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "board.c"

/* -------------------------------------------------------------------------
 * NES-accurate drop speed table (frames per gravity tick)
 * Source: cb2078/tetris-ai tetris.c drop_speed()
 * ---------------------------------------------------------------------- */
static int drop_speed(int level)
{
	static const int t[] = {48,43,38,33,28,23,18,13,8,6,5,5,5,4,4,4,3,3,3,
	                         2, 2, 2, 2, 2, 2, 2, 2, 2, 2};
	if (level < 29) return t[level];
	return 1;
}

/* -------------------------------------------------------------------------
 * 16-bit Galois LFSR (matches tetris.c random_int)
 * ---------------------------------------------------------------------- */
static unsigned short lfsr_step(unsigned short *rng)
{
	unsigned short x = (unsigned short)(1u & (*rng >> 9));
	unsigned short y = (unsigned short)(1u & (*rng >> 1));
	return *rng = (unsigned short)((x ^ y) << 15 | *rng >> 1);
}

/* -------------------------------------------------------------------------
 * Per-game context — owns everything, zero shared mutable state
 * ---------------------------------------------------------------------- */
#define QUEUE_LEN 20000

struct tetris_ctx {
	board_t board;
	unsigned char queue[QUEUE_LEN]; /* per-game piece sequence */
	int queue_i;                    /* index of current piece in queue */

	int shape, next_shape;
	int x, y, r;
	int frames;
	int last_input;
	bool spawn; /* true: piece just locked, next tick will spawn */

	int level;
	int lines;
	int points;
	int done;
};

/* Fill queue with NES-authentic piece sequence seeded deterministically.
 * Replicates random_shape() from tetris.c without thread_local state. */
static void fill_queue(struct tetris_ctx *ctx, uint16_t seed)
{
	unsigned short rng = seed ? seed : 1;
	unsigned char last = (unsigned char)(-1); /* matches initial last=-1 in original */
	unsigned char count = 0;

	for (int i = 0; i < QUEUE_LEN; ++i) {
		unsigned char idx = (unsigned char)(lfsr_step(&rng) >> 8);
		idx += ++count;   /* unsigned char: wraps at 256 like the original */
		idx &= 7;
		if (idx == 7 || idx == last) {
			idx  = (unsigned char)(lfsr_step(&rng) >> 8);
			idx &= 7;
			idx += last;  /* unsigned char addition — wraps same as original */
			idx %= 7;
		}
		ctx->queue[i] = last = idx;
	}
}

static bool ctx_spawn(struct tetris_ctx *ctx)
{
	ctx->shape    = ctx->next_shape;
	ctx->queue_i  = (ctx->queue_i + 1) % QUEUE_LEN;
	ctx->next_shape = ctx->queue[ctx->queue_i];
	ctx->x = 3;
	ctx->y = 1;
	ctx->r = 0;
	ctx->frames = 0;
	return !collides(ctx->board, ctx->shape, ctx->x, ctx->y, ctx->r);
}

/* Returns false when the piece can't move (locks if dy!=0). */
static bool ctx_move(struct tetris_ctx *ctx, int dx, int dy, int dr)
{
	static const int pts[] = {0, 40, 100, 300, 1200};

	int nr = (ctx->r + dr + 4) % 4;
	if (collides(ctx->board, ctx->shape, ctx->x + dx, ctx->y + dy, nr)) {
		if (dy) {
			int cleared   = board_write(ctx->board, ctx->shape, ctx->x, ctx->y, ctx->r);
			ctx->points  += (1 + ctx->level) * pts[cleared];
			ctx->lines   += cleared;
			/* NES level progression */
			if (ctx->lines >= ctx->level * 10 + 10 ||
			    (ctx->lines >= 100 && ctx->lines >= ctx->level * 10 - 50))
				ctx->level += ctx->lines % 10 == 0;
			ctx->spawn = true;
		}
		return false;
	}
	ctx->x += dx;
	ctx->y += dy;
	ctx->r  = nr;
	return true;
}

/* -------------------------------------------------------------------------
 * Exported API (opaque pointer — Rust side uses *mut c_void)
 * ---------------------------------------------------------------------- */

struct tetris_ctx *tetris_new(uint16_t seed)
{
	struct tetris_ctx *ctx = calloc(1, sizeof(*ctx));
	if (!ctx) return NULL;
	fill_queue(ctx, seed);
	ctx->next_shape = ctx->queue[0];
	ctx_spawn(ctx);
	return ctx;
}

void tetris_free(struct tetris_ctx *ctx)
{
	free(ctx);
}

/*
 * Advance one frame with the given input bitmask.
 * Flags: UP=0x01 DOWN=0x02 LEFT=0x04 RIGHT=0x08 ROT_CW=0x10 ROT_ACW=0x20
 * Returns: 0 = normal, 1 = piece locked (WRITE), 2 = game over (END)
 */
int tetris_tick(struct tetris_ctx *ctx, int input)
{
	if (ctx->done) return 2;

	++ctx->frames;

	if (ctx->spawn) {
		if (!ctx_spawn(ctx)) {
			ctx->done = 1;
			return 2;
		}
		ctx->spawn  = false;
		ctx->frames = 0;
	}

	/* Rotation — only on new press */
	int new_buttons = input & ~ctx->last_input;
	if (new_buttons & 0x10) ctx_move(ctx, 0, 0, -1); /* ROT_CW  */
	if (new_buttons & 0x20) ctx_move(ctx, 0, 0,  1); /* ROT_ACW */

	/* Gravity */
	if (ctx->frames % drop_speed(ctx->level) == 0)
		if (!ctx_move(ctx, 0, 1, 0))
			return 1;

	/* Horizontal movement — only on new press */
	if (new_buttons & 0x04) ctx_move(ctx, -1, 0, 0); /* LEFT  */
	if (new_buttons & 0x08) ctx_move(ctx,  1, 0, 0); /* RIGHT */

	ctx->last_input = input;
	return 0;
}

long tetris_score(struct tetris_ctx *ctx)
{
	return (long)ctx->points;
}

long tetris_piece_count(struct tetris_ctx *ctx)
{
	return (long)(ctx->queue_i - 1);
}

void tetris_set_level(struct tetris_ctx *ctx, int level)
{
	ctx->level = level;
}

/*
 * Fill out[200] with the visible 20×10 board.
 * board[0..2] are hidden spawn rows; board[2..22] are the visible field.
 *   0.0 = empty
 *   1.0 = placed cell
 *  -1.0 = current falling piece cell
 */
void tetris_sense(struct tetris_ctx *ctx, double *out)
{
	for (int row = 2; row < HEIGHT; ++row)
		for (int col = 0; col < WIDTH; ++col)
			out[(row - 2) * WIDTH + col] = ctx->board[row][col] ? 1.0 : 0.0;

	/* Overlay the falling piece (only when a piece is active) */
	if (!ctx->spawn && ctx->shape >= 0 && ctx->shape < N_SHAPES) {
		for (int i = 0; i < 4; ++i)
			for (int j = 0; j < 4; ++j) {
				if (!shapes[ctx->shape][ctx->r][i][j]) continue;
				int vis = (ctx->y + i) - 2;
				int col = ctx->x + j;
				if (vis >= 0 && vis < 20 && col >= 0 && col < WIDTH)
					out[vis * WIDTH + col] = -1.0;
			}
	}
}

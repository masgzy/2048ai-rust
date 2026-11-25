//! Terminal rendering of boards (classic 2048 colour palette, 24-bit ANSI).

use crate::board;

/// Classic 2048 tile background colours indexed by rank (0 = empty).
const TILE_BG: [(u8, u8, u8); 16] = [
    (187, 173, 160), // empty
    (238, 228, 218), // 2
    (237, 224, 200), // 4
    (242, 177, 121), // 8
    (245, 149, 99),  // 16
    (246, 124, 95),  // 32
    (246, 94, 59),   // 64
    (237, 207, 114), // 128
    (237, 204, 97),  // 256
    (237, 200, 80),  // 512
    (237, 197, 63),  // 1024
    (237, 194, 46),  // 2048
    (190, 90, 200),  // 4096
    (160, 60, 220),  // 8192
    (110, 30, 230),  // 16384
    (60, 15, 180),   // 32768
];

/// Render the board as a 4x4 grid. When `color`, use ANSI backgrounds.
pub fn render_board(b: u64, color: bool) -> String {
    let vals = board::board_to_values(b);
    let mut out = String::with_capacity(1024);
    out.push_str("    +-------+-------+-------+-------+\n");
    for r in 0..4 {
        out.push_str("    ");
        for c in 0..4 {
            let v = vals[4 * r + c];
            let rank = if v == 0 {
                0
            } else {
                v.trailing_zeros() as usize
            };
            let text = if v == 0 {
                ".".to_string()
            } else {
                v.to_string()
            };
            if color {
                let (br, bg, bb) = TILE_BG[rank];
                // Dark text on the pale small tiles, white on saturated ones.
                let fg = if rank <= 2 { "30" } else { "97" };
                out.push_str(&format!(
                    "|\x1b[48;2;{br};{bg};{bb};{fg};1m{txt:^6}\x1b[0m",
                    txt = text
                ));
            } else {
                out.push_str(&format!("|{txt:^6}", txt = text));
            }
        }
        out.push_str("|\n    +-------+-------+-------+-------+\n");
    }
    out
}

/// One status line under the board.
pub fn status_line(score: u32, moves: u64, max_tile: u32) -> String {
    format!("score: {score}  moves: {moves}  max tile: {max_tile}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn render_contains_tiles() {
        let b = crate::board::board_from_values(&[
            2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 0,
        ])
        .unwrap();
        let s = super::render_board(b, false);
        for v in [
            2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768,
        ] {
            assert!(s.contains(&v.to_string()), "missing {v}");
        }
        assert!(!s.contains('\x1b'));
        let colored = super::render_board(b, true);
        assert!(colored.contains("\x1b[48;2;"));
    }
}

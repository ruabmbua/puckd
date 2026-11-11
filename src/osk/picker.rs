//! Layout of the button-mapping target picker: options grouped like a controller, in two
//! columns of rows ("chips"). Shared by navigation (model) and drawing (render).
//!
//! Option indices follow the mapping entry's choice list: 0 = itself, 1 = nothing,
//! 2.. = the buttons in `menu::MAPPABLE` order.

use crate::osk::menu::MAPPABLE;

pub struct Group {
    pub title: &'static str,
    /// (option index, chip label)
    pub chips: Vec<(usize, &'static str)>,
}

fn opt(name: &str) -> usize {
    MAPPABLE.iter().position(|(n, _)| *n == name).map_or(0, |i| i + 2)
}

/// Two columns of groups.
pub fn columns() -> [Vec<Group>; 2] {
    let g = |title, chips: &[(&str, &'static str)]| Group {
        title,
        chips: chips
            .iter()
            .map(|&(n, l)| {
                (
                    if n == "itself" {
                        0
                    } else if n == "nothing" {
                        1
                    } else {
                        opt(n)
                    },
                    l,
                )
            })
            .collect(),
    };
    [
        vec![
            g("Default", &[("itself", "Itself"), ("nothing", "Nothing")]),
            g("Face buttons", &[("a", "A"), ("b", "B"), ("x", "X"), ("y", "Y")]),
            g("Shoulders & triggers", &[("lb", "LB"), ("rb", "RB"), ("lt_click", "LT full"), ("rt_click", "RT full")]),
            g("Sticks", &[("l3", "L3"), ("r3", "R3")]),
        ],
        vec![
            g("D-pad", &[("dpad_up", "Up"), ("dpad_down", "Down"), ("dpad_left", "Left"), ("dpad_right", "Right")]),
            g("Back paddles", &[("l4", "L4"), ("l5", "L5"), ("r4", "R4"), ("r5", "R5")]),
            g("System", &[("view", "View"), ("menu", "Menu"), ("steam", "Steam")]),
            g("Trackpads", &[("lpad_click", "Left pad"), ("rpad_click", "Right pad")]),
        ],
    ]
}

/// (column, row, chip) of an option.
pub fn locate(cols: &[Vec<Group>; 2], o: usize) -> (usize, usize, usize) {
    for (c, col) in cols.iter().enumerate() {
        for (r, g) in col.iter().enumerate() {
            if let Some(i) = g.chips.iter().position(|(x, _)| *x == o) {
                return (c, r, i);
            }
        }
    }
    (0, 0, 0)
}

/// Move the focus through the grid.
pub fn step(o: usize, dx: i32, dy: i32) -> usize {
    let cols = columns();
    let (mut c, mut r, mut i) = locate(&cols, o);
    if dy != 0 {
        let rows = cols[c].len() as i32;
        r = (r as i32 + dy).clamp(0, rows - 1) as usize;
        i = i.min(cols[c][r].chips.len() - 1);
    } else if dx != 0 {
        let n = cols[c][r].chips.len() as i32;
        let ni = i as i32 + dx;
        if ni < 0 && c == 1 {
            c = 0;
            r = r.min(cols[0].len() - 1);
            i = cols[0][r].chips.len() - 1;
        } else if ni >= n && c == 0 {
            c = 1;
            r = r.min(cols[1].len() - 1);
            i = 0;
        } else {
            i = ni.clamp(0, n - 1) as usize;
        }
    }
    cols[c][r].chips[i].0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_every_option_once() {
        let cols = columns();
        let mut all: Vec<usize> = cols.iter().flatten().flat_map(|g| g.chips.iter().map(|c| c.0)).collect();
        all.sort();
        assert_eq!(all, (0..MAPPABLE.len() + 2).collect::<Vec<_>>());
    }

    #[test]
    fn navigation() {
        let a = opt("a");
        assert_eq!(step(a, 1, 0), opt("b"));
        assert_eq!(step(a, 0, -1), 0); // up to "Itself"
        assert_eq!(step(opt("y"), 1, 0), opt("l4")); // across to the right column
        assert_eq!(step(opt("l4"), -1, 0), opt("y"));
        assert_eq!(step(opt("l3"), 0, 1), opt("l3")); // bottom stays
    }
}

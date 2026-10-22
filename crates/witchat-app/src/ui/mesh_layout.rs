//! Positions for the mesh map: a spring layout (linked nodes pull together,
//! all nodes push apart) with node 0, us, pinned in the middle. Then no node
//! may sit on a line it is not an end of, or the map hides that link. The
//! start positions are fixed, so the same mesh draws the same way each time.

/// Golden angle: start positions spiral out without lining up.
const GOLDEN: f32 = 2.399_963;
/// Closest a node may sit to a link it is not part of, in layout units.
pub const CLEARANCE: f32 = 0.12;
/// Closest two nodes may sit, in layout units.
const SEPARATION: f32 = 0.2;

type At = (f32, f32);

/// Positions in [-1, 1] on both axes for `count` nodes; node 0 at the center.
pub fn spring(count: usize, edges: &[(usize, usize)]) -> Vec<At> {
    let mut at: Vec<At> = (0..count).map(|i| start(i, count)).collect();
    if count < 2 {
        return at;
    }
    let iterations = if count <= 20 { 300 } else { 120 };
    let k = 0.7 * (4.0 / count as f32).sqrt();
    for step in 0..iterations {
        let temperature = 0.1 * (1.0 - step as f32 / iterations as f32) + 0.002;
        let push = forces(&at, edges, k);
        for (p, f) in at.iter_mut().zip(&push).skip(1) {
            let len = f.0.hypot(f.1).max(1e-6);
            let s = len.min(temperature) / len;
            p.0 += f.0 * s;
            p.1 += f.1 * s;
        }
    }
    fit(&mut at);
    for _ in 0..30 {
        let moved = separate(&mut at) | clear_links(&mut at, edges);
        fit(&mut at);
        if !moved {
            break;
        }
    }
    at
}

/// Pushes apart nodes closer than `SEPARATION`; node 0 stays put.
fn separate(at: &mut [At]) -> bool {
    let mut moved = false;
    for i in 0..at.len() {
        for j in i + 1..at.len() {
            let (dx, dy, d) = apart(at[j], at[i]);
            if d >= SEPARATION {
                continue;
            }
            let (ux, uy) = if dx.hypot(dy) > 1e-6 {
                (dx / d, dy / d)
            } else {
                let angle = j as f32 * GOLDEN;
                (angle.cos(), angle.sin())
            };
            let shift = SEPARATION - d + 0.02;
            let share = if i == 0 { 1.0 } else { 0.5 };
            at[j].0 += ux * shift * share;
            at[j].1 += uy * shift * share;
            if i != 0 {
                at[i].0 -= ux * shift * share;
                at[i].1 -= uy * shift * share;
            }
            moved = true;
        }
    }
    moved
}

fn start(i: usize, count: usize) -> At {
    if i == 0 {
        return (0.0, 0.0);
    }
    let angle = i as f32 * GOLDEN;
    let radius = 0.3 + 0.6 * (i as f32 / count as f32).sqrt();
    (radius * angle.cos(), radius * angle.sin())
}

fn forces(at: &[At], edges: &[(usize, usize)], k: f32) -> Vec<At> {
    let mut push = vec![(0.0f32, 0.0f32); at.len()];
    for i in 0..at.len() {
        for j in i + 1..at.len() {
            let (dx, dy, d) = apart(at[i], at[j]);
            let f = k * k / d;
            push[i].0 += dx / d * f;
            push[i].1 += dy / d * f;
            push[j].0 -= dx / d * f;
            push[j].1 -= dy / d * f;
        }
    }
    for &(a, b) in edges.iter().filter(|(a, b)| a != b && *a < at.len() && *b < at.len()) {
        let (dx, dy, d) = apart(at[a], at[b]);
        let f = d * d / k;
        push[a].0 -= dx / d * f;
        push[a].1 -= dy / d * f;
        push[b].0 += dx / d * f;
        push[b].1 += dy / d * f;
    }
    push
}

fn apart(a: At, b: At) -> (f32, f32, f32) {
    let (dx, dy) = (a.0 - b.0, a.1 - b.1);
    (dx, dy, dx.hypot(dy).max(0.01))
}

/// Scale so the farthest node sits on the unit circle.
fn fit(at: &mut [At]) {
    let far = at.iter().map(|p| p.0.hypot(p.1)).fold(0.0, f32::max);
    if far > 1e-6 {
        for p in at.iter_mut() {
            p.0 /= far;
            p.1 /= far;
        }
    }
}

/// Moves nodes off links they do not belong to; true if anything moved.
/// Node 0 stays put, so a link crossing it moves its own ends instead.
fn clear_links(at: &mut [At], edges: &[(usize, usize)]) -> bool {
    let mut moved = false;
    for &(a, b) in edges {
        for v in 0..at.len() {
            if v == a || v == b || a >= at.len() || b >= at.len() {
                continue;
            }
            let (d, normal) = off_segment(at[v], at[a], at[b]);
            if d >= CLEARANCE {
                continue;
            }
            let shift = CLEARANCE - d + 0.02;
            if v == 0 {
                for end in [a, b].into_iter().filter(|e| *e != 0) {
                    at[end].0 -= normal.0 * shift;
                    at[end].1 -= normal.1 * shift;
                }
            } else {
                at[v].0 += normal.0 * shift;
                at[v].1 += normal.1 * shift;
            }
            moved = true;
        }
    }
    moved
}

/// Distance from `p` to segment `a`-`b`, and the unit direction from the
/// segment toward `p` (perpendicular to it when `p` lies on it).
pub fn off_segment(p: At, a: At, b: At) -> (f32, At) {
    let (sx, sy) = (b.0 - a.0, b.1 - a.1);
    let len2 = sx * sx + sy * sy;
    let t = if len2 < 1e-9 { 0.0 } else { (((p.0 - a.0) * sx + (p.1 - a.1) * sy) / len2).clamp(0.0, 1.0) };
    let closest = (a.0 + t * sx, a.1 + t * sy);
    let (dx, dy) = (p.0 - closest.0, p.1 - closest.1);
    let d = dx.hypot(dy);
    let normal = if d > 1e-6 {
        (dx / d, dy / d)
    } else {
        let len = len2.sqrt().max(1e-6);
        (-sy / len, sx / len)
    };
    (d, normal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(count: usize, edges: &[(usize, usize)]) -> Vec<At> {
        let at = spring(count, edges);
        assert_eq!(at, spring(count, edges), "same mesh, same drawing");
        assert_eq!(at.first().copied(), (count > 0).then_some((0.0, 0.0)), "us in the middle");
        for (i, p) in at.iter().enumerate() {
            assert!(p.0.is_finite() && p.1.is_finite() && p.0.abs() <= 1.0 && p.1.abs() <= 1.0, "node {i} at {p:?}");
            for q in &at[i + 1..] {
                assert!(apart(*p, *q).2 > 0.1, "nodes overlap: {p:?} {q:?}");
            }
        }
        for &(a, b) in edges {
            for (v, p) in at.iter().enumerate().filter(|(v, _)| *v != a && *v != b) {
                let (d, _) = off_segment(*p, at[a], at[b]);
                assert!(d >= CLEARANCE * 0.5, "node {v} sits on link {a}-{b} (distance {d})");
            }
        }
        at
    }

    #[test]
    fn small_and_empty_meshes() {
        check(0, &[]);
        check(1, &[]);
        check(2, &[(0, 1)]);
    }

    #[test]
    fn the_straight_line_map_gets_its_hidden_link_back() {
        // 19 Oct 2026 lab: us, two phones, and a tablet linked to both phones.
        check(4, &[(0, 1), (0, 2), (2, 3), (1, 3)]);
    }

    #[test]
    fn a_dense_mesh_keeps_every_link_visible() {
        // Same lab after an Android tablet joined: 5 nodes, 8 links.
        check(5, &[(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3), (2, 4), (3, 4)]);
        let all: Vec<(usize, usize)> = (0..6).flat_map(|a| (a + 1..6).map(move |b| (a, b))).collect();
        check(6, &all);
    }

    #[test]
    fn stars_chains_and_strangers() {
        let star: Vec<(usize, usize)> = (1..9).map(|i| (0, i)).collect();
        check(9, &star);
        check(5, &[(0, 1), (1, 2), (2, 3), (3, 4)]);
        check(5, &[(0, 1), (0, 2), (3, 4)]);
    }
}

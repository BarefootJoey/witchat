//! The mesh topology drawing in the debug tab: us in the middle, everyone
//! else placed by a spring layout (`mesh_layout`). Solid lines are links both
//! ends claim (or we observe); dashed arrows are one-sided claims from
//! announces, pointing away from the peer that makes the claim.

use super::mesh_layout;
use super::Message;
use crate::theme;
use iced::widget::canvas::{self, Frame, Geometry, LineDash, Path, Stroke, Text};
use iced::{mouse, Color, Pixels, Point, Rectangle, Renderer, Theme, Vector};
use std::collections::{HashMap, VecDeque};
use witchat_core::mesh_diag::Topology;

const DASH: [f32; 2] = [5.0, 4.0];

pub(super) struct Node {
    pub label: String,
    /// Position in [-1, 1] on both axes.
    pub at: (f32, f32),
    /// Hops from us: 0 for us, 1 for neighbors, 2 for everyone further.
    pub ring: u8,
    pub id: String,
}

struct Link {
    a: usize,
    b: usize,
    confirmed: bool,
    /// For a one-sided claim, the node the claim points at.
    toward: Option<usize>,
}

pub(super) struct MeshMap {
    nodes: Vec<Node>,
    links: Vec<Link>,
}

impl MeshMap {
    /// `names` maps routing ids to nicknames; unknown ids show 8 hex digits.
    pub fn new(topology: &Topology, names: &HashMap<String, String>) -> Self {
        let ids = topology.nodes();
        let index: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
        let links: Vec<Link> = topology
            .edges
            .iter()
            .map(|e| {
                let (a, b) = (index[e.a.as_str()], index[e.b.as_str()]);
                let toward = e.claimed_by.as_deref().map(|by| if by == e.a { b } else { a });
                Link { a, b, confirmed: e.confirmed, toward }
            })
            .collect();
        let pairs: Vec<(usize, usize)> = links.iter().map(|l| (l.a, l.b)).collect();
        let rings = hops_from_us(ids.len(), &pairs);
        let positions = mesh_layout::spring(ids.len(), &pairs);
        let nodes = ids
            .iter()
            .zip(rings)
            .zip(positions)
            .map(|((id, ring), at)| Node { label: label(id, names), at, ring, id: id.clone() })
            .collect();
        Self { nodes, links }
    }

    #[cfg(test)]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
}

fn label(id: &str, names: &HashMap<String, String>) -> String {
    names.get(id).cloned().unwrap_or_else(|| format!("{}…", id.chars().take(8).collect::<String>()))
}

/// Breadth-first hops from node 0 (us) over any link, capped at 2.
fn hops_from_us(count: usize, links: &[(usize, usize)]) -> Vec<u8> {
    let mut hops = vec![u8::MAX; count];
    if count == 0 {
        return hops;
    }
    hops[0] = 0;
    let mut queue = VecDeque::from([0usize]);
    while let Some(at) = queue.pop_front() {
        for &(a, b) in links {
            let next = if a == at {
                b
            } else if b == at {
                a
            } else {
                continue;
            };
            if hops[next] == u8::MAX {
                hops[next] = hops[at] + 1;
                queue.push_back(next);
            }
        }
    }
    hops.into_iter().map(|h| h.min(2)).collect()
}

fn radius(ring: u8) -> f32 {
    match ring {
        0 => 9.0,
        1 => 7.0,
        _ => 5.0,
    }
}

/// A filled arrowhead whose tip stops at the edge of the node it points at.
fn arrowhead(from: Point, to: Point, gap: f32) -> Option<Path> {
    let v = Vector::new(to.x - from.x, to.y - from.y);
    let len = v.x.hypot(v.y);
    if len <= gap + 8.0 {
        return None;
    }
    let (ux, uy) = (v.x / len, v.y / len);
    let tip = Point::new(to.x - ux * gap, to.y - uy * gap);
    let back = Point::new(tip.x - ux * 8.0, tip.y - uy * 8.0);
    Some(Path::new(|p| {
        p.move_to(tip);
        p.line_to(Point::new(back.x - uy * 4.0, back.y + ux * 4.0));
        p.line_to(Point::new(back.x + uy * 4.0, back.y - ux * 4.0));
        p.close();
    }))
}

impl canvas::Program<Message> for MeshMap {
    type State = ();

    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let center = frame.center();
        let scale = (bounds.width.min(bounds.height) / 2.0 - 28.0).max(10.0);
        let point = |n: &Node| Point::new(center.x + n.at.0 * scale, center.y + n.at.1 * scale);
        for link in &self.links {
            let (from, to) = (point(&self.nodes[link.a]), point(&self.nodes[link.b]));
            if link.confirmed {
                frame.stroke(&Path::line(from, to), Stroke::default().with_color(theme::GREEN_DIM).with_width(2.0));
                continue;
            }
            let dashed = Stroke {
                line_dash: LineDash { segments: &DASH, offset: 0 },
                ..Stroke::default().with_color(theme::GRAY).with_width(1.2)
            };
            frame.stroke(&Path::line(from, to), dashed);
            if let Some(target) = link.toward {
                let source = if target == link.a { link.b } else { link.a };
                let (tail, tip) = (point(&self.nodes[source]), point(&self.nodes[target]));
                if let Some(head) = arrowhead(tail, tip, radius(self.nodes[target].ring) + 2.0) {
                    frame.fill(&head, theme::GRAY);
                }
            }
        }
        for node in &self.nodes {
            let color = match node.ring {
                0 => theme::GREEN,
                1 => theme::peer_color(&node.id),
                _ => Color { a: 0.8, ..theme::peer_color(&node.id) },
            };
            let at = point(node);
            let r = radius(node.ring);
            frame.fill(&Path::circle(at, r), color);
            frame.fill_text(Text {
                content: node.label.clone(),
                position: Point::new(at.x + r + 3.0, at.y - 7.0),
                color: if node.ring == 0 { theme::GREEN } else { theme::GREEN_SOFT },
                size: Pixels(11.0),
                ..Text::default()
            });
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchat_core::mesh_diag::Edge;

    #[test]
    fn rings_labels_and_claim_direction() {
        let edge = |a: &str, b: &str| Edge { a: a.into(), b: b.into(), confirmed: true, claimed_by: None };
        let claim = Edge { a: "cc".into(), b: "dd".into(), confirmed: false, claimed_by: Some("dd".into()) };
        let topology = Topology {
            me: "aa".into(),
            edges: vec![edge("aa", "bb"), edge("bb", "cc"), claim, edge("ee", "ff0123456789")],
        };
        let names = HashMap::from([("bb".to_string(), "bob".to_string())]);
        let map = MeshMap::new(&topology, &names);
        let node = |id: &str| map.nodes().iter().position(|n| n.id == id).unwrap();
        let ring = |id: &str| map.nodes()[node(id)].ring;
        assert_eq!((ring("aa"), ring("bb"), ring("cc")), (0, 1, 2));
        assert_eq!(ring("dd"), 2, "deeper peers share the outer style");
        assert_eq!(ring("ee"), 2, "unreachable claims still show");
        assert_eq!(map.nodes()[node("bb")].label, "bob");
        assert_eq!(map.nodes()[node("ff0123456789")].label, "ff012345…", "unknown peers show a short id");
        let one_sided: Vec<_> = map.links.iter().filter(|l| !l.confirmed).collect();
        assert_eq!(one_sided.len(), 1);
        assert_eq!(one_sided[0].toward, Some(node("cc")), "dd claims cc, so the arrow points at cc");
        assert!(map.nodes().iter().all(|n| n.at.0.abs() <= 1.0 && n.at.1.abs() <= 1.0));
    }
}

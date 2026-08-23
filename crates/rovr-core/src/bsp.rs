#![allow(dead_code)]
use rovr_types::{Rect, WindowId};
use serde::{Deserialize, Serialize};

use crate::layout_state::Axis;

/// Persistent BSP node. `Leaf` holds a tiled window; `Split` holds axis, ratio and two children.
/// Ratio is always clamped to 0.1..=0.9; persistence serializes the tree so topology survives restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BspNode {
    Leaf(WindowId),
    Split {
        axis: Axis,
        ratio: f64,
        left: Box<BspNode>,
        right: Box<BspNode>,
    },
}

impl BspNode {
    fn is_leaf(&self) -> bool {
        matches!(self, BspNode::Leaf(_))
    }

    fn ratio(&self) -> Option<f64> {
        match self {
            BspNode::Split { ratio, .. } => Some(*ratio),
            _ => None,
        }
    }

    fn clamp_ratio(r: f64) -> f64 {
        r.clamp(0.1, 0.9)
    }

    fn leaves(&self, out: &mut Vec<WindowId>) {
        match self {
            BspNode::Leaf(w) => out.push(*w),
            BspNode::Split { left, right, .. } => {
                left.leaves(out);
                right.leaves(out);
            }
        }
    }

    fn contains(&self, win: WindowId) -> bool {
        match self {
            BspNode::Leaf(w) => *w == win,
            BspNode::Split { left, right, .. } => left.contains(win) || right.contains(win),
        }
    }

    fn remove(&mut self, win: WindowId) -> Option<BspNode> {
        match self {
            BspNode::Leaf(w) if *w == win => None,
            BspNode::Leaf(_) => Some(self.clone()),
            BspNode::Split { left, right, .. } => {
                // Try remove from children
                let left_removed = left.remove(win);
                let right_removed = right.remove(win);
                match (left_removed, right_removed) {
                    (None, None) => {
                        // Both children removed? Should not happen because win is unique
                        None
                    }
                    (None, Some(r)) => Some(r),
                    (Some(l), None) => Some(l),
                    (Some(l), Some(r)) => {
                        // Neither child was the target, rebuild
                        Some(BspNode::Split {
                            axis: match self {
                                BspNode::Split { axis, .. } => *axis,
                                _ => Axis::Vertical,
                            },
                            ratio: self.ratio().unwrap_or(0.5),
                            left: Box::new(l),
                            right: Box::new(r),
                        })
                    }
                }
            }
        }
    }

    fn swap(&mut self, a: WindowId, b: WindowId) -> bool {
        match self {
            BspNode::Leaf(w) => {
                if *w == a {
                    *w = b;
                    true
                } else if *w == b {
                    *w = a;
                    true
                } else {
                    false
                }
            }
            BspNode::Split { left, right, .. } => {
                let l = left.swap(a, b);
                let r = right.swap(a, b);
                l || r
            }
        }
    }

    fn depth_of(&self, win: WindowId, depth: usize) -> Option<usize> {
        match self {
            BspNode::Leaf(w) if *w == win => Some(depth),
            BspNode::Leaf(_) => None,
            BspNode::Split { left, right, .. } => left
                .depth_of(win, depth + 1)
                .or_else(|| right.depth_of(win, depth + 1)),
        }
    }

    fn collect_splits_mut(&mut self, f: &mut dyn FnMut(&mut Axis, &mut f64)) {
        match self {
            BspNode::Leaf(_) => {}
            BspNode::Split {
                axis,
                ratio,
                left,
                right,
            } => {
                f(axis, ratio);
                left.collect_splits_mut(f);
                right.collect_splits_mut(f);
            }
        }
    }

    fn placements(&self, area: Rect, gap: f64, out: &mut Vec<(WindowId, Rect)>) {
        match self {
            BspNode::Leaf(w) => out.push((*w, area)),
            BspNode::Split {
                axis,
                ratio,
                left,
                right,
            } => {
                let (a1, a2) = split(area, gap, *axis, *ratio);
                left.placements(a1, gap, out);
                right.placements(a2, gap, out);
            }
        }
    }

    fn resize_edge(
        &mut self,
        window: WindowId,
        edge: rovr_types::Direction,
        delta: f64,
        area: Rect,
        gap: f64,
    ) -> bool {
        let BspNode::Split {
            axis,
            ratio,
            left,
            right,
        } = self
        else {
            return false;
        };
        let (left_area, right_area) = split(area, gap, *axis, *ratio);
        let in_left = left.contains(window);
        let child_changed = if in_left {
            left.resize_edge(window, edge, delta, left_area, gap)
        } else if right.contains(window) {
            right.resize_edge(window, edge, delta, right_area, gap)
        } else {
            return false;
        };
        if child_changed {
            return true;
        }

        let total = match axis {
            Axis::Vertical => area.width - gap,
            Axis::Horizontal => area.height - gap,
        };
        if total <= 0.0 {
            return false;
        }
        let adjustment = match (*axis, in_left, edge) {
            (Axis::Vertical, true, rovr_types::Direction::East)
            | (Axis::Horizontal, true, rovr_types::Direction::South) => delta / total,
            (Axis::Vertical, false, rovr_types::Direction::West)
            | (Axis::Horizontal, false, rovr_types::Direction::North) => -delta / total,
            _ => return false,
        };
        *ratio = Self::clamp_ratio(*ratio + adjustment);
        true
    }
}

fn split(area: Rect, gap: f64, axis: Axis, ratio: f64) -> (Rect, Rect) {
    let r = BspNode::clamp_ratio(ratio);
    if axis == Axis::Vertical {
        let total = (area.width - gap).max(0.0);
        let first_w = (total * r).max(0.0);
        let second_w = (total - first_w).max(0.0);
        (
            Rect {
                x: area.x,
                y: area.y,
                width: first_w,
                height: area.height,
            },
            Rect {
                x: area.x + first_w + gap,
                y: area.y,
                width: second_w,
                height: area.height,
            },
        )
    } else {
        let total = (area.height - gap).max(0.0);
        let first_h = (total * r).max(0.0);
        let second_h = (total - first_h).max(0.0);
        (
            Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: first_h,
            },
            Rect {
                x: area.x,
                y: area.y + first_h + gap,
                width: area.width,
                height: second_h,
            },
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BspTree {
    pub root: Option<BspNode>,
}

impl BspTree {
    pub fn new() -> Self {
        Self { root: None }
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn len(&self) -> usize {
        let mut v = Vec::new();
        if let Some(r) = &self.root {
            r.leaves(&mut v);
        }
        v.len()
    }

    pub fn contains(&self, w: WindowId) -> bool {
        self.root.as_ref().is_some_and(|n| n.contains(w))
    }

    pub fn leaves(&self) -> Vec<WindowId> {
        let mut v = Vec::new();
        if let Some(r) = &self.root {
            r.leaves(&mut v);
        }
        v
    }

    pub fn insert(&mut self, w: WindowId) -> bool {
        if self.contains(w) {
            return false;
        }
        if self.root.is_none() {
            self.root = Some(BspNode::Leaf(w));
            return true;
        }
        // Insert as sibling of rightmost leaf
        let root = self.root.take().unwrap();
        let new_root = Self::insert_rightmost(root, w, 0);
        self.root = Some(new_root);
        true
    }

    fn insert_rightmost(node: BspNode, w: WindowId, depth: usize) -> BspNode {
        match node {
            BspNode::Leaf(existing) => {
                let axis = if depth % 2 == 0 {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                };
                BspNode::Split {
                    axis,
                    ratio: 0.5,
                    left: Box::new(BspNode::Leaf(existing)),
                    right: Box::new(BspNode::Leaf(w)),
                }
            }
            BspNode::Split {
                axis,
                ratio,
                left,
                right,
            } => {
                // Recurse to rightmost
                let new_right = Self::insert_rightmost(*right, w, depth + 1);
                BspNode::Split {
                    axis,
                    ratio,
                    left,
                    right: Box::new(new_right),
                }
            }
        }
    }

    pub fn remove(&mut self, w: WindowId) -> bool {
        if !self.contains(w) {
            return false;
        }
        let root = self.root.take().unwrap();
        let new_root = {
            match root {
                BspNode::Leaf(_) => None,
                mut node => {
                    // Use remove helper

                    node.remove(w)
                }
            }
        };
        self.root = new_root;
        true
    }

    pub fn swap(&mut self, a: WindowId, b: WindowId) -> bool {
        if a == b || !self.contains(a) || !self.contains(b) {
            return false;
        }
        if let Some(root) = &mut self.root {
            root.swap(a, b);
            true
        } else {
            false
        }
    }

    /// Warp `w` to be sibling of `target`. If `before` true, w becomes left sibling, else right.
    pub fn warp(&mut self, w: WindowId, target: WindowId, before: bool) -> bool {
        if w == target || !self.contains(target) {
            return false;
        }
        // If w already exists, remove it first (reinsert)
        let had_w = self.contains(w);
        if had_w {
            self.remove(w);
            if !self.contains(target) {
                // target was sibling and removal collapsed unexpectedly? reinsert w back
                self.insert(w);
                return false;
            }
        }
        // Now insert w as sibling of target
        let root = self.root.take().unwrap();
        let new_root = Self::insert_sibling(root, w, target, before, 0);
        self.root = Some(new_root);
        true
    }

    fn insert_sibling(
        node: BspNode,
        w: WindowId,
        target: WindowId,
        before: bool,
        depth: usize,
    ) -> BspNode {
        match node {
            BspNode::Leaf(cur) if cur == target => {
                let axis = if depth % 2 == 0 {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                };
                if before {
                    BspNode::Split {
                        axis,
                        ratio: 0.5,
                        left: Box::new(BspNode::Leaf(w)),
                        right: Box::new(BspNode::Leaf(cur)),
                    }
                } else {
                    BspNode::Split {
                        axis,
                        ratio: 0.5,
                        left: Box::new(BspNode::Leaf(cur)),
                        right: Box::new(BspNode::Leaf(w)),
                    }
                }
            }
            BspNode::Leaf(cur) => BspNode::Leaf(cur),
            BspNode::Split {
                axis,
                ratio,
                left,
                right,
            } => {
                if left.contains(target) {
                    let new_left = Self::insert_sibling(*left, w, target, before, depth + 1);
                    BspNode::Split {
                        axis,
                        ratio,
                        left: Box::new(new_left),
                        right,
                    }
                } else if right.contains(target) {
                    let new_right = Self::insert_sibling(*right, w, target, before, depth + 1);
                    BspNode::Split {
                        axis,
                        ratio,
                        left,
                        right: Box::new(new_right),
                    }
                } else {
                    BspNode::Split {
                        axis,
                        ratio,
                        left,
                        right,
                    }
                }
            }
        }
    }

    pub fn balance(&mut self) {
        if let Some(root) = &mut self.root {
            root.collect_splits_mut(&mut |_, ratio| *ratio = 0.5);
        }
    }

    pub fn rotate(&mut self) {
        if let Some(root) = &mut self.root {
            root.collect_splits_mut(&mut |axis, _| {
                *axis = match *axis {
                    Axis::Vertical => Axis::Horizontal,
                    Axis::Horizontal => Axis::Vertical,
                }
            });
        }
    }

    pub fn mirror(&mut self) {
        Self::mirror_node(self.root.as_mut());
    }

    fn mirror_node(node: Option<&mut BspNode>) {
        if let Some(n) = node {
            match n {
                BspNode::Leaf(_) => {}
                BspNode::Split { left, right, .. } => {
                    std::mem::swap(left, right);
                    Self::mirror_node(Some(left));
                    Self::mirror_node(Some(right));
                }
            }
        }
    }

    /// Adjust the nearest BSP split boundary represented by `edge`.
    pub fn resize_edge(
        &mut self,
        window: WindowId,
        edge: rovr_types::Direction,
        delta: f64,
        area: Rect,
        gap: f64,
    ) -> bool {
        self.root
            .as_mut()
            .is_some_and(|root| root.resize_edge(window, edge, delta, area, gap))
    }

    pub fn set_ratio(&mut self, ratio: f64) -> bool {
        let r = BspNode::clamp_ratio(ratio);
        if let Some(root) = &mut self.root {
            match root {
                BspNode::Leaf(_) => return false,
                BspNode::Split { ratio: rr, .. } => {
                    *rr = r;
                    return true;
                }
            }
        }
        false
    }

    /// Set ratio of the split that directly parents `w`, if any.
    pub fn set_ratio_for_window(&mut self, w: WindowId, ratio: f64) -> bool {
        let r = BspNode::clamp_ratio(ratio);
        if let Some(root) = &mut self.root {
            return Self::set_ratio_for_window_recursive(root, w, r);
        }
        false
    }

    fn set_ratio_for_window_recursive(node: &mut BspNode, w: WindowId, r: f64) -> bool {
        match node {
            BspNode::Leaf(_) => false,
            BspNode::Split {
                ratio, left, right, ..
            } => {
                if left.contains(w) || right.contains(w) {
                    // If one child directly contains w as leaf or subtree, set this split's ratio
                    // Prefer the immediate parent: if child is leaf matching w, set this split
                    // else recurse deeper to find closer parent.
                    if matches!(**left, BspNode::Leaf(lw) if lw == w)
                        || matches!(**right, BspNode::Leaf(rw) if rw == w)
                    {
                        *ratio = r;
                        return true;
                    }
                    if left.contains(w) && Self::set_ratio_for_window_recursive(left, w, r) {
                        return true;
                    }
                    if right.contains(w) && Self::set_ratio_for_window_recursive(right, w, r) {
                        return true;
                    }
                    *ratio = r;
                    true
                } else {
                    false
                }
            }
        }
    }

    pub fn placements(&self, area: Rect, gap: f64) -> Vec<(WindowId, Rect)> {
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            r.placements(area, gap, &mut out);
        }
        out
    }

    pub fn sync_with_windows(&mut self, windows: &std::collections::HashSet<WindowId>) {
        // Remove windows no longer present
        let leaves = self.leaves();
        for w in leaves {
            if !windows.contains(&w) {
                self.remove(w);
            }
        }
        // Insert missing windows in deterministic sorted order
        let mut missing: Vec<WindowId> = windows
            .iter()
            .copied()
            .filter(|w| !self.contains(*w))
            .collect();
        missing.sort_unstable();
        for w in missing {
            self.insert(w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovr_types::WindowId;

    #[test]
    fn insert_preserves_topology() {
        let mut t = BspTree::new();
        t.insert(WindowId(1));
        t.insert(WindowId(2));
        let leaves1 = t.leaves();
        assert_eq!(leaves1, vec![WindowId(1), WindowId(2)]);
        // Insert third should not reorder existing
        t.insert(WindowId(3));
        let leaves2 = t.leaves();
        assert_eq!(leaves2[0], WindowId(1));
        assert_eq!(leaves2[1], WindowId(2));
        assert_eq!(leaves2[2], WindowId(3));
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn remove_collapses_parent() {
        let mut t = BspTree::new();
        for id in 1..=4 {
            t.insert(WindowId(id));
        }
        assert_eq!(t.len(), 4);
        t.remove(WindowId(2));
        let leaves = t.leaves();
        assert!(!leaves.contains(&WindowId(2)));
        assert_eq!(t.len(), 3);
        // topology should still be valid and placements work
        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let pls = t.placements(area, 10.0);
        assert_eq!(pls.len(), 3);
    }

    #[test]
    fn swap_windows() {
        let mut t = BspTree::new();
        for id in 1..=3 {
            t.insert(WindowId(id));
        }
        assert!(t.swap(WindowId(1), WindowId(3)));
        let leaves = t.leaves();
        assert_eq!(leaves[0], WindowId(3));
        assert_eq!(leaves[2], WindowId(1));
        assert!(!t.swap(WindowId(1), WindowId(99)));
    }

    #[test]
    fn warp_reinsert() {
        let mut t = BspTree::new();
        for id in 1..=4 {
            t.insert(WindowId(id));
        }
        // warp 4 to before 2
        assert!(t.warp(WindowId(4), WindowId(2), true));
        let leaves = t.leaves();
        // 4 should be adjacent to 2
        let pos4 = leaves.iter().position(|w| *w == WindowId(4)).unwrap();
        let pos2 = leaves.iter().position(|w| *w == WindowId(2)).unwrap();
        assert_eq!(pos4 + 1, pos2);
    }

    #[test]
    fn balance_sets_ratios() {
        let mut t = BspTree::new();
        for id in 1..=3 {
            t.insert(WindowId(id));
        }
        t.set_ratio_for_window(WindowId(1), 0.8);
        // Ensure some ratio !=0.5
        let mut has_non_half = false;
        if let Some(root) = &t.root {
            let mut check = |_: &mut Axis, ratio: &mut f64| {
                if (*ratio - 0.5).abs() > 0.01 {
                    has_non_half = true;
                }
            };
            let mut r = root.clone();
            r.collect_splits_mut(&mut check);
        }
        assert!(has_non_half);
        t.balance();
        if let Some(root) = &t.root {
            let mut all_half = true;
            let mut check = |_: &mut Axis, ratio: &mut f64| {
                if (*ratio - 0.5).abs() > 0.001 {
                    all_half = false;
                }
            };
            let mut r = root.clone();
            r.collect_splits_mut(&mut check);
            assert!(all_half);
        }
    }

    #[test]
    fn rotate_mirror() {
        let mut t = BspTree::new();
        for id in 1..=2 {
            t.insert(WindowId(id));
        }
        let axis_before = match t.root.as_ref().unwrap() {
            BspNode::Split { axis, .. } => *axis,
            _ => Axis::Vertical,
        };
        t.rotate();
        let axis_after = match t.root.as_ref().unwrap() {
            BspNode::Split { axis, .. } => *axis,
            _ => Axis::Vertical,
        };
        assert_ne!(axis_before, axis_after);
        let leaves_before = t.leaves();
        t.mirror();
        let leaves_after = t.leaves();
        assert_eq!(leaves_before[0], leaves_after[1]);
        assert_eq!(leaves_before[1], leaves_after[0]);
    }

    #[test]
    fn placements_cover_area() {
        let mut t = BspTree::new();
        for id in 1..=3 {
            t.insert(WindowId(id));
        }
        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let pls = t.placements(area, 10.0);
        assert_eq!(pls.len(), 3);
        for (_, r) in pls {
            assert!(r.x >= 0.0 && r.y >= 0.0);
            assert!(r.width > 0.0 && r.height > 0.0);
        }
    }

    #[test]
    fn resize_edge_updates_split_ratio_and_survives_relayout() {
        let mut tree = BspTree::new();
        tree.insert(WindowId(1));
        tree.insert(WindowId(2));
        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let before = tree.placements(area, 0.0);
        assert!(tree.resize_edge(WindowId(1), rovr_types::Direction::East, 100.0, area, 0.0));
        let after = tree.placements(area, 0.0);
        assert!(after[0].1.width > before[0].1.width);
        assert!(after[1].1.width < before[1].1.width);
    }

    #[test]
    fn ratio_clamped() {
        let mut t = BspTree::new();
        t.insert(WindowId(1));
        t.insert(WindowId(2));
        assert!(t.set_ratio(0.01)); // clamped to 0.1
        if let Some(BspNode::Split { ratio, .. }) = t.root.as_ref() {
            assert!((*ratio - 0.1).abs() < 0.001);
        }
        assert!(t.set_ratio(5.0)); // clamped to 0.9
        if let Some(BspNode::Split { ratio, .. }) = t.root.as_ref() {
            assert!((*ratio - 0.9).abs() < 0.001);
        }
    }
}

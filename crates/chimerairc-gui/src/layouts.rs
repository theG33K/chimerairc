//! The dock panes, the docks that hold them and the saved layouts (HydraIRC had five). Pure data,
//! unit-tested.
//!
//! There can be up to four docks, one on each edge of the main window; a dock can be split into two halves,
//! each with its own row of tabs. Any pane can instead float in
//! a window of its own. Every pane is in at most one place (a pane in none has its tab switched off) and
//! at least one pane is always on. A layout is
//! the whole state: for every dock whether it is shown, how thick it is, which panes it has and
//! which of them is in front. There are five slots; saving writes the current state into one,
//! loading puts one back.

/// Every pane the dock can show, in tab order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Servers = 0,
    Events = 1,
    Messages = 2,
    Notices = 3,
    Urls = 4,
    Channels = 5,
    Connections = 6,
    Buddies = 7,
    /// Everyone in every channel of every connection.
    Users = 8,
    /// Server and status output of every connection, in one log.
    Output = 9,
}

/// How many panes there are.
pub const PANE_COUNT: usize = 10;

/// A set of panes: a bit for every [`Pane`].
pub type Mask = u16;

pub const PANES: [Pane; PANE_COUNT] = [
    Pane::Servers,
    Pane::Events,
    Pane::Messages,
    Pane::Notices,
    Pane::Urls,
    Pane::Channels,
    Pane::Connections,
    Pane::Buddies,
    Pane::Users,
    Pane::Output,
];

/// A bit for every pane.
pub const ALL_PANES: Mask = 0x3ff;
pub const SLOTS: usize = 5;
pub const MIN_WIDTH: u32 = 120;
pub const MAX_WIDTH: u32 = 800;
/// A split dock gives each half at least this share (percent).
pub const SPLIT_MIN: u8 = 15;
pub const SPLIT_MAX: u8 = 85;

/// The side of the main window a dock sits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Edge {
    #[default]
    Left = 0,
    Right = 1,
    Top = 2,
    Bottom = 3,
}

pub const EDGES: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

impl Edge {
    pub fn from_index(i: u8) -> Option<Edge> {
        EDGES.get(i as usize).copied()
    }
    pub fn title(self) -> &'static str {
        match self {
            Edge::Left => "Left",
            Edge::Right => "Right",
            Edge::Top => "Top",
            Edge::Bottom => "Bottom",
        }
    }
    /// A dock on the top or bottom is a wide strip (its size is its height).
    pub fn horizontal(self) -> bool {
        matches!(self, Edge::Top | Edge::Bottom)
    }
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }
    /// A sensible thickness (96-dpi pixels) for a new dock on this edge.
    pub fn default_size(self) -> u32 {
        if self.horizontal() {
            160
        } else {
            220
        }
    }
}

/// A dock window is a *slot*: the first half of the dock on an edge has the edge's number (0..4), the
/// second half that number plus four.
pub fn slot(e: Edge, half: usize) -> usize {
    half.min(1) * 4 + e as usize
}

pub fn slot_edge(slot: usize) -> Edge {
    Edge::from_index((slot % 4) as u8).unwrap_or_default()
}

pub fn slot_half(slot: usize) -> usize {
    (slot / 4).min(1)
}

/// A rectangle: x, y, width, height.
pub type Rect4 = (i32, i32, i32, i32);

/// Cut a strip of `size` pixels off `edge` of `area`: returns (the strip, the rest). The strip is
/// shortened so that at least `keep` pixels are left for the rest (never below zero).
pub fn split(area: Rect4, edge: Edge, size: i32, keep: i32) -> (Rect4, Rect4) {
    let (x, y, w, h) = area;
    let along = if edge.horizontal() { h } else { w };
    let s = size.clamp(0, (along - keep).max(0));
    match edge {
        Edge::Left => ((x, y, s, h), (x + s, y, w - s, h)),
        Edge::Right => ((x + w - s, y, s, h), (x, y, w - s, h)),
        Edge::Top => ((x, y, w, s), (x, y + s, w, h - s)),
        Edge::Bottom => ((x, y + h - s, w, s), (x, y, w, h - s)),
    }
}

/// Where a dragged tab was let go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropAt {
    /// Into the dock on this edge (a new one when there is none).
    Dock(Edge),
    /// Into a window of its own, with its top left corner here.
    Float(i32, i32),
}

/// Decide what dropping a tab at screen point `pt` means. `frame` is the main window's client area in
/// screen coordinates and `docks` the shown docks' rectangles: over a dock means that dock, near an
/// edge of the window means a dock on that edge, anywhere else (the middle of the window, or outside
/// it) means a floating window.
pub fn drop_target(pt: (i32, i32), frame: Rect4, docks: [Option<Rect4>; 4], margin: i32) -> DropAt {
    let inside = |r: Rect4| pt.0 >= r.0 && pt.0 < r.0 + r.2 && pt.1 >= r.1 && pt.1 < r.1 + r.3;
    for e in EDGES {
        if docks[e as usize].is_some_and(inside) {
            return DropAt::Dock(e);
        }
    }
    if inside(frame) {
        let (x, y, w, h) = frame;
        let near = [
            (Edge::Left, pt.0 - x),
            (Edge::Right, x + w - 1 - pt.0),
            (Edge::Top, pt.1 - y),
            (Edge::Bottom, y + h - 1 - pt.1),
        ];
        if let Some((e, _)) = near.into_iter().filter(|(_, d)| *d < margin).min_by_key(|(_, d)| *d) {
            return DropAt::Dock(e);
        }
    }
    DropAt::Float(pt.0, pt.1)
}

impl Pane {
    pub fn from_index(i: u8) -> Option<Pane> {
        PANES.get(i as usize).copied()
    }
    pub fn bit(self) -> Mask {
        1 << (self as u8)
    }
    /// The label on the tab and in the View > Panes menu.
    pub fn title(self) -> &'static str {
        match self {
            Pane::Servers => "Servers",
            Pane::Events => "Events",
            Pane::Messages => "Messages",
            Pane::Notices => "Notices",
            Pane::Urls => "URLs",
            Pane::Channels => "Channels",
            Pane::Connections => "Connections",
            Pane::Buddies => "Buddies",
            Pane::Users => "Users",
            Pane::Output => "Output",
        }
    }
}

/// The panes in `mask`, in tab order.
pub fn panes_in(mask: Mask) -> Vec<Pane> {
    PANES.iter().copied().filter(|p| mask & p.bit() != 0).collect()
}

/// One dock: the strip of tabbed panes on one edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Dock {
    pub visible: bool,
    /// Thickness in 96-dpi pixels: the width on the left or right, the height on the top or bottom.
    pub size: u32,
    /// Panes that have a tab here (bit per [`Pane`]). Zero means there is no dock on this edge.
    pub panes: Mask,
    /// The pane in front; one of `panes` whenever there are any.
    pub active: u8,
}

impl Dock {
    fn empty(edge: Edge) -> Dock {
        Dock { visible: false, size: edge.default_size(), panes: 0, active: 0 }
    }
    /// Is there a dock here that is shown?
    pub fn shown(&self) -> bool {
        self.visible && self.panes != 0
    }
}

/// A pane in a window of its own: where it is on the screen (device pixels) and whether it is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Float {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub visible: bool,
}

pub const FLOAT_MIN_W: i32 = 120;
pub const FLOAT_MIN_H: i32 = 80;
const FLOAT_LIMIT: i32 = 100_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    /// Indexed by [`Edge`].
    pub docks: [Dock; 4],
    /// The second half of each dock (only its `panes` and `active` count); empty when the dock is not split.
    /// A dock that is split always has a first half too.
    pub second: [Dock; 4],
    /// For a split dock, the share of it (percent) that the first half takes.
    pub split: [u8; 4],
    /// Floating panes, indexed by [`Pane`].
    pub floats: [Option<Float>; PANE_COUNT],
    /// Where each pane stands in the tab order of its dock (smaller is further left); only the order
    /// among the panes of one dock matters, so a number may repeat across docks.
    pub rank: [u8; PANE_COUNT],
}

impl Default for Layout {
    fn default() -> Self {
        let mut l = Layout::blank();
        l.docks[Edge::Left as usize] = Dock { visible: true, size: 220, panes: ALL_PANES, active: 0 };
        l
    }
}

impl Layout {
    /// No docks and no floating panes (not a usable layout until something is put in it).
    fn blank() -> Layout {
        Layout { docks: EDGES.map(Dock::empty), second: EDGES.map(Dock::empty), split: [50; 4], floats: [None; PANE_COUNT], rank: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9] }
    }

    /// A layout with a single dock, as older versions stored it.
    pub fn single(edge: Edge, visible: bool, size: u32, panes: Mask, active: u8) -> Layout {
        let mut l = Layout::blank();
        l.docks[edge as usize] = Dock { visible, size, panes, active };
        l.normalized()
    }

    /// The dock on `e` (its first half: the one that carries the size and whether it is shown).
    pub fn dock(&self, e: Edge) -> &Dock {
        &self.docks[e as usize]
    }

    /// Half `g` (0 or 1) of the dock on `e`.
    pub fn half(&self, e: Edge, g: usize) -> &Dock {
        if g == 0 {
            &self.docks[e as usize]
        } else {
            &self.second[e as usize]
        }
    }

    fn half_mut(&mut self, e: Edge, g: usize) -> &mut Dock {
        if g == 0 {
            &mut self.docks[e as usize]
        } else {
            &mut self.second[e as usize]
        }
    }

    /// Is the dock on `e` split into two halves?
    pub fn is_split(&self, e: Edge) -> bool {
        self.second[e as usize].panes != 0
    }

    /// The edge and half (0 or 1) that hold `p`.
    pub fn half_of(&self, p: Pane) -> Option<(Edge, usize)> {
        for e in EDGES {
            for g in 0..2 {
                if self.half(e, g).panes & p.bit() != 0 {
                    return Some((e, g));
                }
            }
        }
        None
    }

    /// Bring any values into range: sizes clamped, unknown bits dropped, a pane in only one dock (the
    /// first edge wins), at least one pane overall, and every dock's front pane one of its own.
    pub fn normalized(mut self) -> Layout {
        let mut taken: Mask = 0;
        for e in EDGES {
            let d = &mut self.docks[e as usize];
            d.size = d.size.clamp(MIN_WIDTH, MAX_WIDTH);
            d.panes &= ALL_PANES & !taken;
            taken |= d.panes;
            let s = &mut self.second[e as usize];
            s.panes &= ALL_PANES & !taken;
            taken |= s.panes;
            self.split[e as usize] = self.split[e as usize].clamp(SPLIT_MIN, SPLIT_MAX);
        }
        // a split dock always has a first half: the second takes its place when the first is empty
        for e in EDGES {
            if self.docks[e as usize].panes == 0 && self.second[e as usize].panes != 0 {
                self.promote(e);
            }
        }
        // a floating pane is in no dock; its window has a sane size
        for p in PANES {
            let docked = taken & p.bit() != 0;
            if let Some(f) = &mut self.floats[p as usize] {
                if docked {
                    self.floats[p as usize] = None;
                } else {
                    f.width = f.width.clamp(FLOAT_MIN_W, FLOAT_LIMIT);
                    f.height = f.height.clamp(FLOAT_MIN_H, FLOAT_LIMIT);
                    f.left = f.left.clamp(-FLOAT_LIMIT, FLOAT_LIMIT);
                    f.top = f.top.clamp(-FLOAT_LIMIT, FLOAT_LIMIT);
                }
            }
        }
        if taken == 0 && self.floats.iter().all(Option::is_none) {
            self.docks[Edge::Left as usize].panes = Pane::Servers.bit();
            self.docks[Edge::Left as usize].visible = true;
        }
        for d in self.docks.iter_mut().chain(self.second.iter_mut()) {
            if d.panes == 0 {
                d.active = 0;
            } else if !Pane::from_index(d.active).is_some_and(|p| d.panes & p.bit() != 0) {
                d.active = panes_in(d.panes)[0] as u8;
            }
        }
        for e in EDGES {
            if self.docks[e as usize].panes == 0 {
                self.docks[e as usize].visible = false;
            }
            // only the first half carries the size and whether the dock is shown
            self.second[e as usize].visible = false;
            self.second[e as usize].size = e.default_size();
            if self.second[e as usize].panes == 0 {
                self.split[e as usize] = 50;
            }
        }
        self.canon_ranks();
        self
    }

    /// The second half of the dock on `e` becomes its first half (the first being empty).
    fn promote(&mut self, e: Edge) {
        let s = self.second[e as usize];
        let d = &mut self.docks[e as usize];
        d.panes = s.panes;
        d.active = s.active;
        self.second[e as usize] = Dock::empty(e);
        self.split[e as usize] = 50;
    }

    /// Keep the numbers canonical: 0, 1, 2 ... in each dock, and a pane that is in no dock has its own
    /// number (so equal layouts are equal however they came about).
    fn canon_ranks(&mut self) {
        for e in EDGES {
            for g in 0..2 {
                for (i, p) in self.ordered_in(e, g).into_iter().enumerate() {
                    self.rank[p as usize] = i as u8;
                }
            }
        }
        for p in PANES {
            if self.edge_of(p).is_none() {
                self.rank[p as usize] = p as u8;
            }
        }
    }

    /// The panes of the first half of the dock on `e` in tab order.
    #[cfg(test)]
    pub fn ordered(&self, e: Edge) -> Vec<Pane> {
        self.ordered_in(e, 0)
    }

    /// The panes of half `g` of the dock on `e` in tab order.
    pub fn ordered_in(&self, e: Edge, g: usize) -> Vec<Pane> {
        let mut v = panes_in(self.half(e, g).panes);
        v.sort_by_key(|p| (self.rank[*p as usize], *p as u8));
        v
    }

    /// Number the panes of half `g` of the dock on `e` in the given order (panes of the half that are not
    /// named follow in their usual order).
    pub fn set_order_in(&mut self, e: Edge, g: usize, first: &[Pane]) {
        let mut list: Vec<Pane> = Vec::new();
        for p in first {
            if self.half(e, g).panes & p.bit() != 0 && !list.contains(p) {
                list.push(*p);
            }
        }
        for p in panes_in(self.half(e, g).panes) {
            if !list.contains(&p) {
                list.push(p);
            }
        }
        for (i, p) in list.iter().enumerate() {
            self.rank[*p as usize] = i as u8;
        }
        self.canon_ranks();
    }

    /// The edge whose dock holds `p`, if any.
    pub fn edge_of(&self, p: Pane) -> Option<Edge> {
        self.half_of(p).map(|(e, _)| e)
    }

    /// Is `p` on (in a dock or floating)?
    pub fn shows(&self, p: Pane) -> bool {
        self.edge_of(p).is_some() || self.floats[p as usize].is_some()
    }

    /// Is `p` in a window of its own?
    #[cfg(test)]
    pub fn is_floating(&self, p: Pane) -> bool {
        self.floats[p as usize].is_some()
    }

    /// Is `p` in front: the front pane of a dock that is shown, or a floating pane that is shown?
    pub fn in_front(&self, p: Pane) -> bool {
        match self.floats[p as usize] {
            Some(f) => f.visible,
            None => self.half_of(p).is_some_and(|(e, g)| self.dock(e).shown() && self.half(e, g).active == p as u8),
        }
    }

    /// How many panes are on (docked or floating).
    pub fn pane_count(&self) -> u32 {
        self.docks.iter().chain(self.second.iter()).map(|d| d.panes.count_ones()).sum::<u32>() + self.floats.iter().flatten().count() as u32
    }

    /// The only edge that has a dock, when there is just one.
    pub fn single_edge(&self) -> Option<Edge> {
        let mut with = EDGES.iter().copied().filter(|e| self.dock(*e).panes != 0);
        match (with.next(), with.next()) {
            (Some(e), None) => Some(e),
            _ => None,
        }
    }

    /// Where a pane with no tab goes when it is switched on: the first dock that exists, else the left.
    fn default_edge(&self) -> Edge {
        EDGES.iter().copied().find(|e| self.dock(*e).panes != 0).unwrap_or_default()
    }

    fn put(&mut self, p: Pane, to: Edge) {
        self.put_in(p, to, 0);
    }

    /// Put `p` into half `g` of the dock on `to` (a dock without a first half has no second one).
    fn put_in(&mut self, p: Pane, to: Edge, g: usize) {
        self.floats[p as usize] = None;
        let g = if self.docks[to as usize].panes == 0 { 0 } else { g };
        // a pane arriving in a half goes to the end of its tabs
        let last = self.ordered_in(to, g).iter().filter(|q| **q != p).map(|q| self.rank[*q as usize]).max();
        self.rank[p as usize] = last.map_or(0, |r| r.saturating_add(1));
        let was_empty = self.half(to, g).panes == 0;
        let d = self.half_mut(to, g);
        d.panes |= p.bit();
        d.active = p as u8;
        if g == 0 {
            d.visible = true;
            if was_empty {
                d.size = to.default_size();
            }
        } else {
            self.docks[to as usize].visible = true;
            if was_empty {
                self.split[to as usize] = 50;
            }
        }
        self.canon_ranks();
    }

    fn take(&mut self, p: Pane) {
        self.floats[p as usize] = None;
        let Some((e, g)) = self.half_of(p) else { return };
        let was_visible = self.docks[e as usize].visible;
        let d = self.half_mut(e, g);
        d.panes &= !p.bit();
        if d.panes == 0 {
            d.active = 0;
            if g == 0 {
                d.visible = false;
            }
        } else if d.active == p as u8 {
            d.active = panes_in(d.panes)[0] as u8;
        }
        if self.second[e as usize].panes == 0 {
            self.split[e as usize] = 50;
        }
        if self.docks[e as usize].panes == 0 && self.second[e as usize].panes != 0 {
            self.promote(e);
            self.docks[e as usize].visible = was_visible;
        }
        self.canon_ranks();
    }

    /// Give `p` a tab (in `to`, or the first dock that exists) and bring it to the front.
    pub fn add_pane(&mut self, p: Pane, to: Option<Edge>) {
        if self.shows(p) {
            return;
        }
        let to = to.unwrap_or_else(|| self.default_edge());
        self.put(p, to);
    }

    /// Switch a pane's tab off. The last remaining pane cannot be removed (returns false).
    pub fn remove_pane(&mut self, p: Pane) -> bool {
        if !self.shows(p) || self.pane_count() <= 1 {
            return false;
        }
        self.take(p);
        true
    }

    /// Put a pane in a window of its own at this place on the screen (shown).
    pub fn float_pane(&mut self, p: Pane, left: i32, top: i32, width: i32, height: i32) {
        self.take(p);
        self.floats[p as usize] = Some(Float { left, top, width, height, visible: true });
    }

    /// Move a pane's tab to the dock on `to` (made if there is none) and bring it to the front.
    #[cfg(test)]
    pub fn move_pane(&mut self, p: Pane, to: Edge) {
        self.place(p, to, None);
    }

    /// Like [`Layout::move_pane`], and with `index` the tab ends up at that position among the tabs of
    /// the dock (counting the other tabs only; past the end means last). Within one dock this reorders.
    #[cfg(test)]
    pub fn place(&mut self, p: Pane, to: Edge, index: Option<usize>) {
        self.place_in(p, to, 0, index);
    }

    /// [`Layout::place`] into half `group` (0 or 1) of the dock on `to`.
    pub fn place_in(&mut self, p: Pane, to: Edge, group: usize, index: Option<usize>) {
        let group = group.min(1);
        if self.half_of(p) == Some((to, group)) {
            self.half_mut(to, group).active = p as u8;
            self.docks[to as usize].visible = true;
        } else {
            self.take(p);
            self.put_in(p, to, group);
        }
        if let Some(i) = index {
            let g = self.half_of(p).map_or(0, |(_, g)| g);
            let mut order = self.ordered_in(to, g);
            order.retain(|q| *q != p);
            order.insert(i.min(order.len()), p);
            self.set_order_in(to, g, &order);
        }
    }

    /// Move a tab to the other half of its dock. From the first half this splits the dock (when the
    /// first half has another tab to keep it company); from the second half it goes back to the first.
    pub fn other_half(&mut self, p: Pane) {
        let Some((e, g)) = self.half_of(p) else { return };
        if g == 0 {
            if self.dock(e).panes.count_ones() < 2 {
                return;
            }
            self.take(p);
            self.put_in(p, e, 1);
        } else {
            self.take(p);
            self.put_in(p, e, 0);
        }
    }

    /// Join the two halves of the dock on `e` again (the second half's tabs go after the first's).
    pub fn merge(&mut self, e: Edge) {
        if !self.is_split(e) {
            return;
        }
        let mut order = self.ordered_in(e, 0);
        order.extend(self.ordered_in(e, 1));
        let second = self.second[e as usize];
        self.docks[e as usize].panes |= second.panes;
        self.second[e as usize] = Dock::empty(e);
        self.split[e as usize] = 50;
        self.set_order_in(e, 0, &order);
    }

    /// Put every pane that has a tab into the one dock on `to`. A dock that is made takes over the size
    /// of the single dock it replaces when that has the same shape, else the usual size.
    pub fn gather(&mut self, to: Edge) {
        let kept_size = match self.single_edge() {
            Some(e) if e.horizontal() == to.horizontal() => Some(self.dock(e).size),
            _ => None,
        };
        let front = self
            .docks
            .iter()
            .find(|d| d.shown())
            .map(|d| d.active)
            .or_else(|| self.docks.iter().find(|d| d.panes != 0).map(|d| d.active));
        let mask = self.docks.iter().chain(self.second.iter()).fold(0, |m: Mask, d| m | d.panes) | PANES.iter().filter(|p| self.floats[**p as usize].is_some()).fold(0, |m: Mask, p| m | p.bit());
        self.floats = [None; PANE_COUNT];
        let existing = self.dock(to).panes != 0;
        let size = if existing { self.dock(to).size } else { kept_size.unwrap_or(to.default_size()) };
        for e in EDGES {
            self.docks[e as usize] = Dock::empty(e);
            self.second[e as usize] = Dock::empty(e);
        }
        self.split = [50; 4];
        self.docks[to as usize] = Dock { visible: true, size, panes: mask, active: front.unwrap_or(0) };
        *self = self.normalized();
    }

    /// F9 / F8: bring a pane to the front (giving it a tab if it had none), or hide its dock when it
    /// already is the pane in front.
    pub fn toggle(&mut self, p: Pane) {
        if let Some(f) = &mut self.floats[p as usize] {
            f.visible = !f.visible;
            return;
        }
        match self.half_of(p) {
            Some((e, g)) => {
                let shown = self.dock(e).shown();
                let front = self.half(e, g).active == p as u8;
                if shown && front {
                    self.docks[e as usize].visible = false;
                } else {
                    self.docks[e as usize].visible = true;
                    self.half_mut(e, g).active = p as u8;
                }
            }
            None => {
                let to = self.default_edge();
                self.put(p, to);
            }
        }
    }

    // ---- settings

    fn dock_field(&self, e: Edge) -> String {
        let order_of = |g: usize| -> String { self.ordered_in(e, g).iter().map(|p| char::from(b'0' + *p as u8)).collect() };
        let d = self.dock(e);
        let mut f = format!("{},{},{},{},{},{}", e as u8, u8::from(d.visible), d.size, d.panes, d.active, order_of(0));
        if self.is_split(e) {
            let s = self.half(e, 1);
            f.push_str(&format!(",{},{},{},{}", s.panes, s.active, order_of(1), self.split[e as usize]));
        }
        f
    }

    /// The `dock=` values of the settings file: one per dock that exists.
    pub fn dock_fields(&self) -> Vec<String> {
        let mut v: Vec<String> = EDGES.iter().filter(|e| self.dock(**e).panes != 0).map(|e| self.dock_field(*e)).collect();
        for p in PANES {
            if let Some(f) = self.floats[p as usize] {
                v.push(format!("f,{},{},{},{},{},{}", p as u8, u8::from(f.visible), f.left, f.top, f.width, f.height));
            }
        }
        v
    }

    /// Build a layout from `dock=` values (invalid ones are skipped); `None` when there is none.
    pub fn from_dock_fields<'a>(fields: impl IntoIterator<Item = &'a str>) -> Option<Layout> {
        let mut l = Layout::blank();
        let mut any = false;
        let mut orders: Vec<(Edge, usize, Vec<Pane>)> = Vec::new();
        for f in fields {
            if let Some((e, d, order, second)) = parse_dock_field(f) {
                l.docks[e as usize] = d;
                orders.push((e, 0, order));
                if let Some((s, order2, split)) = second {
                    l.second[e as usize] = s;
                    l.split[e as usize] = split;
                    orders.push((e, 1, order2));
                }
                any = true;
            } else if let Some((p, fl)) = parse_float_field(f) {
                l.floats[p as usize] = Some(fl);
                any = true;
            }
        }
        let mut l = l.normalized();
        for (e, g, order) in orders {
            l.set_order_in(e, g, &order);
        }
        any.then_some(l)
    }

    /// A saved-layout value: TAB-separated `slot` then one `edge,visible,size,panes,active` per dock.
    pub fn to_setting(&self, slot: usize) -> String {
        let mut s = slot.to_string();
        for f in self.dock_fields() {
            s.push('\t');
            s.push_str(&f);
        }
        s
    }

    /// Parse a saved layout, in the current format or the older single-dock one
    /// (`slot visible width panes active [edge]`, without commas).
    pub fn from_setting(v: &str) -> Option<(usize, Layout)> {
        let mut p = v.split('\t');
        let slot: usize = p.next()?.trim().parse().ok().filter(|s| *s < SLOTS)?;
        let rest: Vec<&str> = p.collect();
        if rest.first().is_some_and(|f| f.contains(',')) {
            return Layout::from_dock_fields(rest.iter().copied()).map(|l| (slot, l));
        }
        let visible = rest.first()?.trim() == "1";
        let width: u32 = rest.get(1)?.trim().parse().ok()?;
        let panes: Mask = rest.get(2)?.trim().parse().ok()?;
        let active: u8 = rest.get(3)?.trim().parse().ok()?;
        let edge = rest.get(4).and_then(|e| e.trim().parse::<u8>().ok()).and_then(Edge::from_index).unwrap_or_default();
        Some((slot, Layout::single(edge, visible, width, panes, active)))
    }
}

/// `f,pane,visible,left,top,width,height`
fn parse_float_field(f: &str) -> Option<(Pane, Float)> {
    let mut p = f.split(',');
    if p.next()?.trim() != "f" {
        return None;
    }
    let pane = Pane::from_index(p.next()?.trim().parse().ok()?)?;
    let visible = match p.next()?.trim() {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    let mut num = || p.next()?.trim().parse::<i32>().ok().filter(|n| n.abs() <= FLOAT_LIMIT);
    let (left, top, width, height) = (num()?, num()?, num()?, num()?);
    Some((pane, Float { left, top, width, height, visible }))
}

/// The second half of a dock as stored: its panes, front pane and tab order, and the split.
type SecondHalf = (Dock, Vec<Pane>, u8);

/// `edge,visible,size,panes,active[,order[,panes2,active2,order2,split]]`, an order being the pane numbers
/// in tab order.
fn parse_dock_field(f: &str) -> Option<(Edge, Dock, Vec<Pane>, Option<SecondHalf>)> {
    let mut p = f.split(',');
    let edge = Edge::from_index(p.next()?.trim().parse().ok()?)?;
    let visible = match p.next()?.trim() {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    let size: u32 = p.next()?.trim().parse().ok()?;
    let panes: Mask = p.next()?.trim().parse().ok()?;
    let active: u8 = p.next()?.trim().parse().ok()?;
    let order = |o: Option<&str>| -> Vec<Pane> {
        o.map(|o| o.trim().chars().filter_map(|c| c.to_digit(10)).filter_map(|d| Pane::from_index(d as u8)).collect()).unwrap_or_default()
    };
    let first_order = order(p.next());
    // the second half, when there is one
    let second = (|| {
        let panes2: Mask = p.next()?.trim().parse().ok()?;
        let active2: u8 = p.next()?.trim().parse().ok()?;
        let order2 = order(p.next());
        let split: u8 = p.next()?.trim().parse().ok()?;
        (panes2 != 0).then_some((Dock { visible: visible, size, panes: panes2, active: active2 }, order2, split))
    })();
    Some((edge, Dock { visible, size, panes, active }, first_order, second))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_docks() -> Layout {
        // Servers + Events on the left, everything else at the bottom
        let mut l = Layout::default();
        for p in [Pane::Messages, Pane::Notices, Pane::Urls, Pane::Channels, Pane::Connections, Pane::Buddies] {
            l.move_pane(p, Edge::Bottom);
        }
        l
    }

    #[test]
    fn panes_have_distinct_bits_titles_and_a_stable_order() {
        let mut seen: Mask = 0;
        for (i, p) in PANES.iter().enumerate() {
            assert_eq!(*p as usize, i);
            assert_eq!(Pane::from_index(i as u8), Some(*p));
            assert_eq!(seen & p.bit(), 0, "{p:?} shares a bit");
            seen |= p.bit();
            assert!(!p.title().is_empty());
        }
        assert_eq!(seen, ALL_PANES);
        assert_eq!(Pane::from_index(PANE_COUNT as u8), None);
        assert_eq!(panes_in(Pane::Urls.bit() | Pane::Servers.bit()), [Pane::Servers, Pane::Urls]);
        assert!(panes_in(0).is_empty());
    }

    #[test]
    fn the_default_is_one_dock_on_the_left_with_everything() {
        let l = Layout::default();
        assert_eq!(l.single_edge(), Some(Edge::Left));
        assert_eq!(l.pane_count(), PANE_COUNT as u32);
        assert!(l.dock(Edge::Left).shown() && !l.dock(Edge::Top).shown());
        assert_eq!(l.normalized(), l);
    }

    #[test]
    fn normalizing_repairs_sizes_duplicates_and_empty_layouts() {
        let mut l = Layout::default();
        l.docks[0].size = 5;
        l.docks[1] = Dock { visible: true, size: 99_999, panes: 0b0000_0011, active: 9 }; // overlaps the left dock
        let n = l.normalized();
        assert_eq!(n.dock(Edge::Left).size, MIN_WIDTH);
        assert_eq!(n.dock(Edge::Right).panes, 0, "a pane belongs to the first dock that has it");
        assert!(!n.dock(Edge::Right).visible, "a dock without panes is not shown");
        // nothing at all falls back to the Servers pane
        let none = Layout::blank().normalized();
        assert_eq!(none.single_edge(), Some(Edge::Left));
        assert_eq!(none.dock(Edge::Left).panes, Pane::Servers.bit());
        // a front pane that is not in the dock is replaced by one that is
        let mut l = Layout::default();
        l.docks[0].panes = Pane::Urls.bit() | Pane::Channels.bit();
        l.docks[0].active = 0;
        assert_eq!(l.normalized().dock(Edge::Left).active, Pane::Urls as u8);
    }

    #[test]
    fn moving_a_pane_makes_a_dock_and_empties_the_old_one() {
        let mut l = Layout::default();
        l.move_pane(Pane::Buddies, Edge::Right);
        assert_eq!(l.edge_of(Pane::Buddies), Some(Edge::Right));
        assert_eq!(l.dock(Edge::Right).size, 220, "a new dock gets the usual size");
        assert!(l.dock(Edge::Right).shown());
        assert_eq!(l.dock(Edge::Right).active, Pane::Buddies as u8);
        assert_eq!(l.dock(Edge::Left).panes & Pane::Buddies.bit(), 0);
        assert_eq!(l.pane_count(), PANE_COUNT as u32, "no pane is lost or duplicated");
        // moving the last pane out of a dock removes that dock
        let mut l = Layout::single(Edge::Left, true, 300, Pane::Urls.bit(), 4);
        l.move_pane(Pane::Urls, Edge::Top);
        assert_eq!(l.single_edge(), Some(Edge::Top));
        assert!(!l.dock(Edge::Left).visible);
        assert_eq!(l.dock(Edge::Top).size, 160);
        // moving to where it already is only brings it to the front
        let mut l = Layout::default();
        l.docks[0].visible = false;
        l.move_pane(Pane::Urls, Edge::Left);
        assert!(l.dock(Edge::Left).visible);
        assert_eq!(l.dock(Edge::Left).active, Pane::Urls as u8);
    }

    #[test]
    fn the_front_pane_of_a_dock_follows_when_it_leaves() {
        let mut l = Layout::default();
        l.toggle(Pane::Channels); // front on the left
        assert!(l.in_front(Pane::Channels));
        l.move_pane(Pane::Channels, Edge::Bottom);
        assert!(l.in_front(Pane::Channels), "front of its new dock");
        let left = l.dock(Edge::Left);
        assert!(Pane::from_index(left.active).is_some_and(|p| left.panes & p.bit() != 0), "the old dock still has a valid front pane");
    }

    #[test]
    fn panes_can_be_switched_off_but_never_the_last_one() {
        let mut l = two_docks();
        assert!(l.remove_pane(Pane::Urls));
        assert!(!l.shows(Pane::Urls));
        assert!(!l.remove_pane(Pane::Urls), "already off");
        for p in PANES {
            let _ = l.remove_pane(p);
        }
        assert_eq!(l.pane_count(), 1);
        assert!(l.single_edge().is_some(), "an emptied dock disappears, one is left");
        // switching a pane on puts it in the first dock that exists, in front
        let only = PANES.into_iter().find(|p| l.shows(*p)).unwrap();
        let other = PANES.into_iter().find(|p| *p != only).unwrap();
        l.add_pane(other, None);
        assert_eq!(l.edge_of(other), l.edge_of(only));
        assert!(l.in_front(other));
        // ...or in a chosen one
        let third = PANES.into_iter().find(|p| *p != only && *p != other).unwrap();
        l.add_pane(third, Some(Edge::Top));
        assert_eq!(l.edge_of(third), Some(Edge::Top));
    }

    #[test]
    fn gathering_puts_everything_in_one_dock() {
        let mut l = two_docks();
        l.docks[Edge::Left as usize].size = 333;
        l.gather(Edge::Right);
        assert_eq!(l.single_edge(), Some(Edge::Right));
        assert_eq!(l.pane_count(), PANE_COUNT as u32);
        assert!(l.dock(Edge::Right).shown());
        // a single dock keeps its size when it only changes side, and gets a fresh one when it changes shape
        let mut one = Layout::single(Edge::Left, true, 333, ALL_PANES, 0);
        one.gather(Edge::Right);
        assert_eq!(one.dock(Edge::Right).size, 333);
        one.gather(Edge::Bottom);
        assert_eq!(one.dock(Edge::Bottom).size, 160);
        one.gather(Edge::Bottom);
        assert_eq!(one.single_edge(), Some(Edge::Bottom), "gathering where everything already is changes nothing");
    }

    #[test]
    fn toggling_hides_only_the_dock_of_that_pane() {
        let mut l = two_docks();
        assert!(l.in_front(Pane::Servers));
        l.toggle(Pane::Servers);
        assert!(!l.dock(Edge::Left).visible && l.dock(Edge::Bottom).visible, "the other dock stays");
        l.toggle(Pane::Servers);
        assert!(l.in_front(Pane::Servers));
        // a pane in the other dock: brought to the front there
        l.toggle(Pane::Notices);
        assert!(l.in_front(Pane::Notices));
        // a pane with no tab gets one
        let mut l = Layout::single(Edge::Left, true, 220, Pane::Servers.bit(), 0);
        l.toggle(Pane::Events);
        assert!(l.in_front(Pane::Events));
        assert_eq!(l.edge_of(Pane::Events), Some(Edge::Left));
    }

    #[test]
    fn settings_roundtrip_with_several_docks_and_garbage_is_rejected_or_repaired() {
        let mut l = two_docks();
        l.docks[Edge::Bottom as usize].size = 180;
        for slot in [0, 4] {
            assert_eq!(Layout::from_setting(&l.to_setting(slot)), Some((slot, l)));
        }
        assert_eq!(Layout::from_dock_fields(l.dock_fields().iter().map(|s| s.as_str())), Some(l));
        assert_eq!(Layout::from_setting("9\t0,1,200,127,0"), None, "slot out of range");
        assert_eq!(Layout::from_dock_fields(["", "x", "9,1,200,3,0", "0,2,200,3,0", "0,1,200"]), None);
        // an unusable field is skipped but the good ones count
        let ok = Layout::from_dock_fields(["junk", "2,1,150,6,2"]).unwrap();
        assert_eq!(ok.single_edge(), Some(Edge::Top));
        // out-of-range values are repaired
        let (_, fixed) = Layout::from_setting("1\t0,1,20,0,6").unwrap();
        assert_eq!(fixed.dock(Edge::Left).size, MIN_WIDTH);
        assert_eq!(fixed.pane_count(), 1);
    }

    #[test]
    fn older_single_dock_settings_still_load() {
        // `slot visible width panes active` (before docking on other edges)
        let (slot, l) = Layout::from_setting("2\t1\t250\t127\t3").unwrap();
        assert_eq!(slot, 2);
        assert_eq!(l.single_edge(), Some(Edge::Left));
        assert_eq!((l.dock(Edge::Left).size, l.dock(Edge::Left).panes, l.dock(Edge::Left).active), (250, 127, 3));
        // with an edge, hidden
        let (_, l) = Layout::from_setting("0\t0\t300\t255\t7\t3").unwrap();
        assert_eq!(l.single_edge(), Some(Edge::Bottom));
        assert!(!l.dock(Edge::Bottom).visible);
        assert_eq!(Layout::from_setting("2\t1\t250\t127\t3\t9").unwrap().1.single_edge(), Some(Edge::Left));
        for junk in ["", "0", "0\t1", "0\t1\t200\t127", "x\t1\t200\t127\t0", "0\t1\tnope\t127\t0", "0\t1\t200\t70000\t0"] {
            assert_eq!(Layout::from_setting(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn tabs_are_in_pane_order_until_they_are_moved() {
        let l = Layout::default();
        assert_eq!(l.ordered(Edge::Left), PANES.to_vec());
        assert!(l.ordered(Edge::Top).is_empty());
    }

    #[test]
    fn a_tab_can_be_moved_within_its_dock() {
        let mut l = Layout::default();
        // Buddies (last) to the front, Servers (first) to the middle
        l.place(Pane::Buddies, Edge::Left, Some(0));
        assert_eq!(l.ordered(Edge::Left)[..3], [Pane::Buddies, Pane::Servers, Pane::Events]);
        l.place(Pane::Servers, Edge::Left, Some(3));
        let o = l.ordered(Edge::Left);
        assert_eq!(o.iter().position(|p| *p == Pane::Servers), Some(3));
        assert_eq!(o.len(), PANE_COUNT, "nothing lost or doubled");
        assert_eq!(l.dock(Edge::Left).active, Pane::Servers as u8, "the dragged tab is in front");
        // past the end means last
        l.place(Pane::Events, Edge::Left, Some(99));
        assert_eq!(*l.ordered(Edge::Left).last().unwrap(), Pane::Events);
        // no index: the order is left alone
        let before = l.ordered(Edge::Left);
        l.move_pane(Pane::Urls, Edge::Left);
        assert_eq!(l.ordered(Edge::Left), before);
    }

    #[test]
    fn a_tab_dropped_into_another_dock_goes_where_it_was_dropped() {
        let mut l = Layout::default();
        l.move_pane(Pane::Messages, Edge::Right);
        l.move_pane(Pane::Notices, Edge::Right);
        l.move_pane(Pane::Urls, Edge::Right);
        assert_eq!(l.ordered(Edge::Right), [Pane::Messages, Pane::Notices, Pane::Urls], "arrivals go last");
        l.place(Pane::Channels, Edge::Right, Some(1));
        assert_eq!(l.ordered(Edge::Right), [Pane::Messages, Pane::Channels, Pane::Notices, Pane::Urls]);
        assert_eq!(l.edge_of(Pane::Channels), Some(Edge::Right));
        assert!(!l.ordered(Edge::Left).contains(&Pane::Channels));
        // a new dock simply holds the one tab
        l.place(Pane::Buddies, Edge::Top, Some(5));
        assert_eq!(l.ordered(Edge::Top), [Pane::Buddies]);
    }

    #[test]
    fn the_tab_order_is_saved_and_old_files_get_the_usual_order() {
        let mut l = Layout::default();
        l.place(Pane::Buddies, Edge::Left, Some(0));
        l.place(Pane::Events, Edge::Left, Some(4));
        let fields = l.dock_fields();
        assert_eq!(Layout::from_dock_fields(fields.iter().map(|s| s.as_str())), Some(l));
        assert_eq!(Layout::from_setting(&l.to_setting(1)), Some((1, l)));
        // a dock line without an order (older files): panes in their usual order
        let old = Layout::from_dock_fields(["0,1,220,1023,0"]).unwrap();
        assert_eq!(old.ordered(Edge::Left), PANES.to_vec());
        // a damaged order is repaired: unknown or repeated numbers are ignored, missing panes follow
        let odd = Layout::from_dock_fields(["0,1,220,15,0,3x3z9"]).unwrap();
        assert_eq!(odd.ordered(Edge::Left), [Pane::Notices, Pane::Servers, Pane::Events, Pane::Messages]);
    }

    #[test]
    fn a_dock_can_be_split_into_two_halves_and_joined_again() {
        let mut l = Layout::default();
        assert!(!l.is_split(Edge::Left));
        l.other_half(Pane::Buddies);
        assert!(l.is_split(Edge::Left));
        assert_eq!(l.half_of(Pane::Buddies), Some((Edge::Left, 1)));
        assert_eq!(l.ordered_in(Edge::Left, 1), [Pane::Buddies]);
        assert_eq!(l.ordered_in(Edge::Left, 0).len(), PANE_COUNT - 1);
        assert_eq!(l.pane_count(), PANE_COUNT as u32, "nothing lost or doubled");
        assert_eq!(l.edge_of(Pane::Buddies), Some(Edge::Left), "still in that dock");
        assert!(l.in_front(Pane::Buddies) && l.in_front(Pane::Servers), "each half has a front pane");
        assert_eq!(l.split[Edge::Left as usize], 50);
        // more tabs into the second half
        l.place_in(Pane::Urls, Edge::Left, 1, None);
        assert_eq!(l.ordered_in(Edge::Left, 1), [Pane::Buddies, Pane::Urls]);
        // and a tab back with the other half's button
        l.other_half(Pane::Buddies);
        assert_eq!(l.half_of(Pane::Buddies), Some((Edge::Left, 0)));
        assert!(l.is_split(Edge::Left), "Urls is still alone in the second half");
        // joining
        l.merge(Edge::Left);
        assert!(!l.is_split(Edge::Left));
        assert_eq!(l.ordered_in(Edge::Left, 0).len(), PANE_COUNT);
        assert_eq!(*l.ordered_in(Edge::Left, 0).last().unwrap(), Pane::Urls, "the second half's tabs come last");
        l.merge(Edge::Top); // nothing to join
    }

    #[test]
    fn a_half_that_empties_goes_away_and_a_lone_first_half_is_never_left_empty() {
        let mut l = Layout::default();
        l.other_half(Pane::Notices);
        // taking the only tab out of the second half unsplits
        l.move_pane(Pane::Notices, Edge::Right);
        assert!(!l.is_split(Edge::Left));
        assert_eq!(l.split[Edge::Left as usize], 50);
        // emptying the first half promotes the second
        let mut l = Layout::single(Edge::Bottom, true, 200, Pane::Servers.bit() | Pane::Events.bit(), 0);
        l.other_half(Pane::Events);
        assert!(l.is_split(Edge::Bottom));
        l.move_pane(Pane::Servers, Edge::Top);
        assert!(!l.is_split(Edge::Bottom));
        assert_eq!(l.dock(Edge::Bottom).panes, Pane::Events.bit(), "the second half became the dock");
        assert!(l.dock(Edge::Bottom).shown());
        // a dock with a single tab cannot be split
        let mut one = Layout::single(Edge::Left, true, 200, Pane::Urls.bit(), 4);
        one.other_half(Pane::Urls);
        assert!(!one.is_split(Edge::Left));
        // a tab dropped on the second half of a dock that has none just goes in the dock
        let mut l = Layout::default();
        l.place_in(Pane::Buddies, Edge::Top, 1, None);
        assert_eq!(l.half_of(Pane::Buddies), Some((Edge::Top, 0)));
    }

    #[test]
    fn halves_are_saved_with_their_order_and_split_and_old_lines_still_load() {
        let mut l = Layout::default();
        l.other_half(Pane::Buddies);
        l.place_in(Pane::Events, Edge::Left, 1, Some(0));
        l.split[Edge::Left as usize] = 35;
        let fields = l.dock_fields();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].split(',').count(), 10, "{fields:?}");
        assert_eq!(Layout::from_dock_fields(fields.iter().map(|s| s.as_str())), Some(l));
        assert_eq!(Layout::from_setting(&l.to_setting(3)), Some((3, l)));
        // an unsplit dock keeps the shorter form
        assert_eq!(Layout::default().dock_fields()[0].split(',').count(), 6);
        // damaged halves: a second half with no panes is dropped, the split is clamped, overlaps lose
        let a = Layout::from_dock_fields(["0,1,220,15,0,0123,0,0,,50"]).unwrap();
        assert!(!a.is_split(Edge::Left));
        let b = Layout::from_dock_fields(["0,1,220,15,0,0123,240,4,4567,3"]).unwrap();
        assert_eq!(b.split[Edge::Left as usize], SPLIT_MIN);
        let c = Layout::from_dock_fields(["0,1,220,15,0,0123,15,0,0123,50"]).unwrap();
        assert!(!c.is_split(Edge::Left), "panes already in the first half cannot be in the second");
    }

    #[test]
    fn toggling_and_gathering_know_about_halves() {
        let mut l = Layout::default();
        l.other_half(Pane::Buddies);
        // F9/F8 on a pane of the second half bring it to the front, and hide the whole dock when it is already
        l.toggle(Pane::Urls);
        assert!(l.in_front(Pane::Urls));
        l.place_in(Pane::Channels, Edge::Left, 1, None);
        l.toggle(Pane::Buddies);
        assert!(l.in_front(Pane::Buddies) && !l.in_front(Pane::Channels));
        l.toggle(Pane::Buddies);
        assert!(!l.dock(Edge::Left).visible, "the whole dock is hidden");
        // gathering puts every tab, from both halves, into one plain dock
        l.gather(Edge::Right);
        assert_eq!(l.pane_count(), PANE_COUNT as u32);
        assert!(!l.is_split(Edge::Right));
        assert_eq!(l.single_edge(), Some(Edge::Right));
    }

    #[test]
    fn a_pane_can_float_and_come_back() {
        let mut l = Layout::default();
        l.float_pane(Pane::Buddies, 300, 200, 360, 300);
        assert!(l.is_floating(Pane::Buddies) && l.shows(Pane::Buddies));
        assert_eq!(l.edge_of(Pane::Buddies), None, "it left its dock");
        assert_eq!(l.pane_count(), PANE_COUNT as u32, "still counted once");
        assert!(l.in_front(Pane::Buddies));
        assert_eq!(l.dock(Edge::Left).panes & Pane::Buddies.bit(), 0);
        // F9/F8 show and hide the window
        l.toggle(Pane::Buddies);
        assert!(l.is_floating(Pane::Buddies) && !l.in_front(Pane::Buddies));
        l.toggle(Pane::Buddies);
        assert!(l.in_front(Pane::Buddies));
        // moving to a dock ends the floating
        l.move_pane(Pane::Buddies, Edge::Right);
        assert!(!l.is_floating(Pane::Buddies));
        assert_eq!(l.edge_of(Pane::Buddies), Some(Edge::Right));
        // floating again, then switching the tab off and on
        l.float_pane(Pane::Buddies, 0, 0, 200, 200);
        assert!(l.remove_pane(Pane::Buddies));
        assert!(!l.shows(Pane::Buddies));
        l.add_pane(Pane::Buddies, None);
        assert!(!l.is_floating(Pane::Buddies), "it comes back into a dock");
        // gathering takes floating panes in too
        l.float_pane(Pane::Urls, 5, 5, 200, 200);
        l.gather(Edge::Top);
        assert_eq!(l.pane_count(), PANE_COUNT as u32);
        assert!(l.floats.iter().all(Option::is_none));
        assert_eq!(l.single_edge(), Some(Edge::Top));
    }

    #[test]
    fn a_layout_may_consist_of_floating_panes_only() {
        let mut l = Layout::single(Edge::Left, true, 220, Pane::Servers.bit(), 0);
        l.float_pane(Pane::Servers, 10, 10, 300, 300);
        assert_eq!(l.pane_count(), 1);
        let n = l.normalized();
        assert!(n.is_floating(Pane::Servers), "not replaced by the fallback");
        assert!(!n.dock(Edge::Left).shown());
        assert!(!n.clone().remove_pane(Pane::Servers), "the last pane cannot be switched off");
    }

    #[test]
    fn floating_panes_are_saved_and_repaired() {
        let mut l = Layout::default();
        l.float_pane(Pane::Buddies, -1200, 80, 360, 300);
        l.float_pane(Pane::Urls, 40, 50, 500, 200);
        l.toggle(Pane::Urls);
        let fields = l.dock_fields();
        assert!(fields.iter().any(|f| f.starts_with("f,7,1,-1200,80,360,300")), "{fields:?}");
        assert_eq!(Layout::from_dock_fields(fields.iter().map(|s| s.as_str())), Some(l));
        assert_eq!(Layout::from_setting(&l.to_setting(2)), Some((2, l)));
        // a float for a pane that is also in a dock is dropped, tiny or huge windows are repaired
        let both = Layout::from_dock_fields(["0,1,220,1023,0", "f,3,1,0,0,300,300"]).unwrap();
        assert!(!both.is_floating(Pane::Notices));
        let odd = Layout::from_dock_fields(["0,1,220,127,0", "f,7,1,0,0,5,5"]).unwrap();
        let f = odd.floats[7].unwrap();
        assert_eq!((f.width, f.height), (FLOAT_MIN_W, FLOAT_MIN_H));
        for junk in ["f", "f,10,1,0,0,300,300", "f,3,2,0,0,300,300", "f,3,1,x,0,300,300", "f,3,1,999999,0,300,300", "f,3,1,0,0,300"] {
            assert_eq!(parse_float_field(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn dropping_a_tab_picks_a_dock_an_edge_or_a_floating_window() {
        let frame = (100, 100, 1000, 700);
        let left = Some((100, 100, 200, 700));
        let docks = [left, None, None, None];
        // over a dock: that dock
        assert_eq!(drop_target((150, 400), frame, docks, 40), DropAt::Dock(Edge::Left));
        // near an edge of the window: a dock there
        assert_eq!(drop_target((1090, 400), frame, docks, 40), DropAt::Dock(Edge::Right));
        assert_eq!(drop_target((600, 110), frame, docks, 40), DropAt::Dock(Edge::Top));
        assert_eq!(drop_target((600, 790), frame, docks, 40), DropAt::Dock(Edge::Bottom));
        // the nearer edge wins in a corner
        assert_eq!(drop_target((1095, 120), frame, docks, 40), DropAt::Dock(Edge::Right));
        // the middle of the window and anywhere outside it: a floating window
        assert_eq!(drop_target((600, 400), frame, docks, 40), DropAt::Float(600, 400));
        assert_eq!(drop_target((50, 50), frame, docks, 40), DropAt::Float(50, 50));
        assert_eq!(drop_target((1500, 400), frame, docks, 40), DropAt::Float(1500, 400));
        // a dock wins over the edge zone it overlaps
        assert_eq!(drop_target((105, 400), frame, [None, Some((900, 100, 200, 700)), None, None], 40), DropAt::Dock(Edge::Left));
        assert_eq!(drop_target((1050, 400), frame, [None, Some((900, 100, 200, 700)), None, None], 40), DropAt::Dock(Edge::Right));
    }

    #[test]
    fn the_edge_helpers_are_consistent() {
        for (i, e) in EDGES.iter().enumerate() {
            assert_eq!(Edge::from_index(i as u8), Some(*e));
            assert_eq!(e.opposite().opposite(), *e);
            assert_ne!(e.opposite(), *e);
            assert_eq!(e.opposite().horizontal(), e.horizontal());
        }
        assert_eq!(Edge::from_index(4), None);
        assert!(Edge::Top.default_size() < Edge::Left.default_size());
    }

    #[test]
    fn splitting_the_window_puts_the_strip_on_the_chosen_edge() {
        let area = (0, 30, 1000, 600);
        assert_eq!(split(area, Edge::Left, 200, 100), ((0, 30, 200, 600), (200, 30, 800, 600)));
        assert_eq!(split(area, Edge::Right, 200, 100), ((800, 30, 200, 600), (0, 30, 800, 600)));
        assert_eq!(split(area, Edge::Top, 150, 100), ((0, 30, 1000, 150), (0, 180, 1000, 450)));
        assert_eq!(split(area, Edge::Bottom, 150, 100), ((0, 480, 1000, 150), (0, 30, 1000, 450)));
        assert_eq!(split(area, Edge::Left, 5000, 100).1, (900, 30, 100, 600));
        assert_eq!(split(area, Edge::Bottom, 5000, 120).1, (0, 30, 1000, 120));
        assert_eq!(split(area, Edge::Left, -7, 100).0.2, 0);
        assert_eq!(split((0, 0, 50, 50), Edge::Top, 40, 100).0.3, 0, "a window smaller than `keep` gets no strip");
        for e in EDGES {
            let (d, r) = split(area, e, 123, 100);
            assert_eq!(d.2 * d.3 + r.2 * r.3, 1000 * 600, "{e:?}");
        }
    }
}

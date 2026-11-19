//! A random play order that spreads each artist's songs through the whole order (upstream `utils/artistSpread.ts`,
//! `spreadByArtist` and `randomFromSeed`), ported step for step: with the same `random` stream it gives the same
//! order. See upstream's comment for the method (balanced spread, repair without repeats, runs of a majority artist,
//! keeping recently heard songs late).

use sha2::{Digest as _, Sha256};
use std::collections::HashMap;

/// What came before an order (upstream `Follows`): the artist played just before it, and the songs heard just
/// before it.
pub struct Follows<'a, T> {
    pub previous_artist: Option<&'a str>,
    pub recent: Option<Recent<'a, T>>,
}

/// The songs heard just before an order (`is_recent`), kept out of its first `window` places.
pub struct Recent<'a, T> {
    pub is_recent: &'a dyn Fn(&T) -> bool,
    pub window: usize,
}

impl<T> Default for Follows<'_, T> {
    fn default() -> Self {
        Self {
            previous_artist: None,
            recent: None,
        }
    }
}

/// Upstream `randomFromSeed`: numbers in [0, 1) from xoshiro128** started from the SHA-256 of `seed` (first four
/// little-endian words; all zero becomes a = 1).
pub fn random_from_seed(seed: &str) -> impl FnMut() -> f64 {
    let digest = Sha256::digest(seed.as_bytes());
    let word =
        |i: usize| u32::from_le_bytes([digest[i], digest[i + 1], digest[i + 2], digest[i + 3]]);
    let (mut a, mut b, mut c, mut d) = (word(0), word(4), word(8), word(12));
    if (a | b | c | d) == 0 {
        a = 1;
    }
    move || {
        let result = b.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = b << 9;
        c ^= a;
        d ^= b;
        b ^= c;
        a ^= d;
        c ^= t;
        d = d.rotate_left(11);
        f64::from(result) / 4_294_967_296.0
    }
}

/// Upstream `spreadByArtist`: every item kept, in a spread order; `artist_of` names an item's artist.
pub fn spread_by_artist<T: Clone>(
    items: &[T],
    artist_of: impl Fn(&T) -> String,
    random: &mut impl FnMut() -> f64,
    follows: &Follows<'_, T>,
) -> Vec<T> {
    if items.len() < 2 {
        return items.to_vec();
    }
    let order = spread_once(items, &artist_of, random, follows.previous_artist);
    match &follows.recent {
        Some(recent) => keep_recent_late(
            order,
            &artist_of,
            recent.is_recent,
            recent.window,
            follows.previous_artist,
            random,
        ),
        None => order,
    }
}

fn shuffle_in_place<T>(array: &mut [T], random: &mut impl FnMut() -> f64) {
    for i in (1..array.len()).rev() {
        let j = (random() * (i + 1) as f64).floor() as usize;
        array.swap(i, j);
    }
}

struct Placed<T> {
    artist: usize,
    item: T,
}

fn spread_once<T: Clone>(
    items: &[T],
    artist_of: &impl Fn(&T) -> String,
    random: &mut impl FnMut() -> f64,
    previous_artist: Option<&str>,
) -> Vec<T> {
    let n = items.len();
    // Artists in order of first appearance (a JavaScript Map's key order).
    let mut names: Vec<String> = Vec::new();
    let mut by_artist: HashMap<String, Vec<T>> = HashMap::new();
    for item in items {
        let artist = artist_of(item);
        match by_artist.get_mut(&artist) {
            Some(songs) => songs.push(item.clone()),
            None => {
                names.push(artist.clone());
                by_artist.insert(artist, vec![item.clone()]);
            }
        }
    }

    // Step 1. Largest artist first (ties in random order), each artist's songs shuffled and spread over the free places.
    shuffle_in_place(&mut names, random);
    names.sort_by(|a, b| by_artist[b].len().cmp(&by_artist[a].len()));
    let mut artists: Vec<Vec<T>> = names
        .iter()
        .map(|name| by_artist.remove(name).unwrap())
        .collect();
    let previous = previous_artist
        .and_then(|p| names.iter().position(|name| name == p))
        .map_or(-1, |i| i as i64);
    let mut placed: Vec<Option<Placed<T>>> = (0..n).map(|_| None).collect();
    let mut free = FreePlaces::new(n);
    for (artist, songs) in artists.iter_mut().enumerate() {
        shuffle_in_place(songs, random);
        let c = songs.len();
        let gap = free.count as f64 / c as f64;
        let mut at = vec![0i64; c];
        let offset = random() * gap;
        for j in 0..c {
            let target = (offset + j as f64 * gap + (random() - 0.5) * 0.2 * gap).floor() as i64;
            let low = if j == 0 { 0 } else { at[j - 1] + 1 };
            at[j] = target.max(low).min(free.count as i64 - (c - j) as i64);
        }
        // Last first, so taking a place does not move the free places before it.
        for j in (0..c).rev() {
            let place = free.take(at[j] as usize);
            placed[place] = Some(Placed {
                artist,
                item: songs[j].clone(),
            });
        }
    }
    let placed: Vec<Placed<T>> = placed
        .into_iter()
        .map(|p| p.expect("every place taken"))
        .collect();

    // Each artist's songs in spread order, as indexes into `placed`.
    let mut queues: Vec<Vec<usize>> = vec![Vec::new(); artists.len()];
    for (rank, p) in placed.iter().enumerate() {
        queues[p.artist].push(rank);
    }

    let mut largest = 0;
    for a in 1..queues.len() {
        if queues[a].len() > queues[largest].len() {
            largest = a;
        }
    }
    let big = queues[largest].len();
    if 2 * big > n + 1 {
        return runs_of_majority(
            &placed,
            largest,
            big,
            n - big,
            random,
            previous == largest as i64,
        );
    }
    without_repeats(&placed, &queues, previous)
}

/// Step 2: the spread order with no artist twice in a row.
fn without_repeats<T: Clone>(
    placed: &[Placed<T>],
    queues: &[Vec<usize>],
    mut previous: i64,
) -> Vec<T> {
    let mut heads = vec![0usize; queues.len()];
    let mut remaining: Vec<usize> = queues.iter().map(Vec::len).collect();
    let mut most = remaining.iter().copied().max().unwrap_or(0);
    let mut with_count = vec![0usize; most + 1];
    for &count in &remaining {
        with_count[count] += 1;
    }
    let mut leader = remaining.iter().position(|&c| c == most).unwrap();
    let mut heap = MinHeap::default();
    for (artist, q) in queues.iter().enumerate() {
        heap.push(q[0], artist);
    }

    let mut order = Vec::with_capacity(placed.len());
    for left in (1..=placed.len()).rev() {
        let artist = if 2 * most == left + 1 {
            // This artist has one more song than all the others left: it plays now, or two of its songs would meet.
            if remaining[leader] != most {
                leader = remaining.iter().position(|&c| c == most).unwrap();
            }
            leader
        } else {
            let mut top = pop_current(&mut heap, queues, &heads);
            if top.1 as i64 == previous {
                let next = pop_current(&mut heap, queues, &heads);
                heap.push(top.0, top.1);
                top = next;
            }
            top.1
        };
        order.push(placed[queues[artist][heads[artist]]].item.clone());
        heads[artist] += 1;
        let count = remaining[artist];
        remaining[artist] -= 1;
        with_count[count] -= 1;
        with_count[count - 1] += 1;
        while most > 0 && with_count[most] == 0 {
            most -= 1;
        }
        if heads[artist] < queues[artist].len() {
            heap.push(queues[artist][heads[artist]], artist);
        }
        previous = artist as i64;
    }
    order
}

fn pop_current(heap: &mut MinHeap, queues: &[Vec<usize>], heads: &[usize]) -> (usize, usize) {
    loop {
        let (rank, artist) = heap.pop().expect("spreadByArtist: no song left to play");
        if heads[artist] < queues[artist].len() && queues[artist][heads[artist]] == rank {
            return (rank, artist);
        }
    }
}

/// Step 3: the big artist's `big` songs in `others + 1` runs (`others` when it was just played and another song can
/// open), each other song alone between two runs; the longer runs evenly spaced from a random phase.
fn runs_of_majority<T: Clone>(
    placed: &[Placed<T>],
    largest: usize,
    big: usize,
    others: usize,
    random: &mut impl FnMut() -> f64,
    open_with_other: bool,
) -> Vec<T> {
    let mine: Vec<&T> = placed
        .iter()
        .filter(|p| p.artist == largest)
        .map(|p| &p.item)
        .collect();
    let theirs: Vec<&T> = placed
        .iter()
        .filter(|p| p.artist != largest)
        .map(|p| &p.item)
        .collect();
    let lead = open_with_other && others > 0;
    let runs = if lead { others } else { others + 1 };
    let short = big / runs;
    let longer = big % runs;
    let phase = random();
    let mut order = Vec::with_capacity(placed.len());
    let (mut next, mut other) = (0, 0);
    for run in 0..runs {
        if lead {
            order.push(theirs[other].clone());
            other += 1;
        }
        let long = (((run + 1) * longer) as f64 / runs as f64 + phase).floor()
            - (((run * longer) as f64) / runs as f64 + phase).floor();
        let length = short + long as usize;
        for _ in 0..length {
            order.push(mine[next].clone());
            next += 1;
        }
        if !lead && run < others {
            order.push(theirs[other].clone());
            other += 1;
        }
    }
    order
}

/// Step 4: recently heard songs out of the first `window` places (at most half the order), changing which song takes
/// which place but never which artist plays where.
fn keep_recent_late<T: Clone>(
    order: Vec<T>,
    artist_of: &impl Fn(&T) -> String,
    is_recent: &dyn Fn(&T) -> bool,
    window: usize,
    previous_artist: Option<&str>,
    random: &mut impl FnMut() -> f64,
) -> Vec<T> {
    let window = window.min(order.len() / 2);
    // Artists in order of first appearance, with their places and songs.
    let mut artists: Vec<String> = Vec::new();
    let mut places: HashMap<String, Vec<usize>> = HashMap::new();
    let mut songs: HashMap<String, Vec<T>> = HashMap::new();
    for (i, item) in order.iter().enumerate() {
        let artist = artist_of(item);
        if !places.contains_key(&artist) {
            artists.push(artist.clone());
            places.insert(artist.clone(), Vec::new());
            songs.insert(artist.clone(), Vec::new());
        }
        places.get_mut(&artist).unwrap().push(i);
        songs.get_mut(&artist).unwrap().push(item.clone());
    }
    let mut recent_of: Vec<(String, usize)> = Vec::new();
    for artist in &artists {
        let list = songs.get_mut(artist).unwrap();
        let recent = list.iter().filter(|s| is_recent(s)).count();
        if recent == 0 {
            continue;
        }
        recent_of.push((artist.clone(), recent));
        let (old, new): (Vec<T>, Vec<T>) = list.drain(..).partition(|s| !is_recent(s));
        list.extend(old);
        list.extend(new);
    }

    // Artists with no recent song, by their number of places (keys in first-seen order), each list shuffled.
    let mut spare: Vec<(usize, Vec<String>)> = Vec::new();
    for artist in &artists {
        if recent_of.iter().any(|(a, _)| a == artist) {
            continue;
        }
        let len = places[artist].len();
        match spare.iter_mut().find(|(l, _)| *l == len) {
            Some((_, list)) => list.push(artist.clone()),
            None => spare.push((len, vec![artist.clone()])),
        }
    }
    for (_, list) in &mut spare {
        shuffle_in_place(list, random);
    }
    let mut traded: Vec<String> = Vec::new();
    for (artist, recent) in &recent_of {
        let mine = places[artist].clone();
        if mine[mine.len() - recent] >= window {
            continue;
        }
        let empty = Vec::new();
        let candidates = spare
            .iter()
            .find(|(l, _)| *l == mine.len())
            .map_or(&empty, |(_, list)| list);
        let found = candidates.iter().find(|other| {
            if traded.contains(other) {
                return false;
            }
            let theirs = &places[*other];
            if theirs[theirs.len() - recent] < window {
                return false;
            }
            if theirs[0] == 0 && Some(artist.as_str()) == previous_artist {
                return false;
            }
            if mine[0] == 0 && Some(other.as_str()) == previous_artist {
                return false;
            }
            true
        });
        let Some(other) = found.cloned() else {
            continue;
        };
        traded.push(other.clone());
        let theirs = places[&other].clone();
        places.insert(artist.clone(), theirs);
        places.insert(other, mine);
    }

    let mut out: Vec<Option<T>> = (0..order.len()).map(|_| None).collect();
    for artist in &artists {
        for (k, song) in songs[artist].iter().enumerate() {
            out[places[artist][k]] = Some(song.clone());
        }
    }
    out.into_iter()
        .map(|s| s.expect("every place filled"))
        .collect()
}

/// Places 0..n-1, each free or taken; finds the k-th free place in O(log n) (a Fenwick tree of free counts).
struct FreePlaces {
    tree: Vec<i64>,
    top: usize,
    n: usize,
    count: usize,
}

impl FreePlaces {
    fn new(n: usize) -> Self {
        let mut tree = vec![0i64; n + 1];
        for i in 1..=n {
            tree[i] += 1;
            let parent = i + (i & i.wrapping_neg());
            if parent <= n {
                tree[parent] += tree[i];
            }
        }
        let mut top = 1;
        while top * 2 <= n {
            top *= 2;
        }
        Self {
            tree,
            top,
            n,
            count: n,
        }
    }

    /// Takes the k-th free place (from 0) and returns it.
    fn take(&mut self, k: usize) -> usize {
        let mut at = 0;
        let mut left = k as i64;
        let mut step = self.top;
        while step > 0 {
            let next = at + step;
            if next <= self.n && self.tree[next] <= left {
                at = next;
                left -= self.tree[next];
            }
            step >>= 1;
        }
        let mut i = at + 1;
        while i <= self.n {
            self.tree[i] -= 1;
            i += i & i.wrapping_neg();
        }
        self.count -= 1;
        at
    }
}

/// A binary min-heap of (rank, artist) by rank, as upstream's (equal ranks never occur: ranks are distinct places).
#[derive(Default)]
struct MinHeap {
    entries: Vec<(usize, usize)>,
}

impl MinHeap {
    fn push(&mut self, rank: usize, artist: usize) {
        let mut i = self.entries.len();
        self.entries.push((rank, artist));
        while i > 0 {
            let parent = (i - 1) >> 1;
            if self.entries[parent].0 <= rank {
                break;
            }
            self.entries[i] = self.entries[parent];
            i = parent;
        }
        self.entries[i] = (rank, artist);
    }

    fn pop(&mut self) -> Option<(usize, usize)> {
        if self.entries.is_empty() {
            return None;
        }
        let top = self.entries[0];
        let last = self.entries.pop().unwrap();
        let size = self.entries.len();
        if size > 0 {
            let mut i = 0;
            loop {
                let left = 2 * i + 1;
                if left >= size {
                    break;
                }
                let child = if left + 1 < size && self.entries[left + 1].0 < self.entries[left].0 {
                    left + 1
                } else {
                    left
                };
                if self.entries[child].0 >= last.0 {
                    break;
                }
                self.entries[i] = self.entries[child];
                i = child;
            }
            self.entries[i] = last;
        }
        Some(top)
    }
}

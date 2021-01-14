/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

mod sparse_array_usize;

use super::entry::Entry;
use crate::list;
use crate::utils::DefaultBuildHasher;
use crate::List;
use crate::ListSync;
use alloc::vec::Vec;
use archery::{ArcK, RcK, SharedPointer, SharedPointerKind};
use core::borrow::Borrow;
use core::fmt::Display;
use core::hash::BuildHasher;
use core::hash::Hash;
use core::iter::FromIterator;
use core::iter::Peekable;
use core::mem::size_of;
use core::ops::Index;
use core::slice;
use sparse_array_usize::SparseArrayUsize;

type HashValue = u64;

// TODO Use impl trait instead of this when available.
pub type Iter<'a, K, V, P> =
    core::iter::Map<IterPtr<'a, K, V, P>, fn(&'a SharedPointer<Entry<K, V>, P>) -> (&'a K, &'a V)>;
pub type IterKeys<'a, K, V, P> = core::iter::Map<Iter<'a, K, V, P>, fn((&'a K, &V)) -> &'a K>;
pub type IterValues<'a, K, V, P> = core::iter::Map<Iter<'a, K, V, P>, fn((&K, &'a V)) -> &'a V>;

const DEFAULT_DEGREE: u8 = 8 * size_of::<usize>() as u8;

/// Creates a [`HashTrieMap`](crate::HashTrieMap) containing the given arguments:
///
/// ```
/// # use rpds::*;
/// #
/// let m = HashTrieMap::new()
///     .insert(1, "one")
///     .insert(2, "two")
///     .insert(3, "three");
///
/// assert_eq!(ht_map![1 => "one", 2 => "two", 3 => "three"], m);
/// ```
#[macro_export]
macro_rules! ht_map {
    ($($k:expr => $v:expr),*) => {
        {
            #[allow(unused_mut)]
            let mut m = $crate::HashTrieMap::new();
            $(
                m.insert_mut($k, $v);
            )*
            m
        }
    };
}

/// Creates a [`HashTrieMap`](crate::HashTrieMap) that implements `Sync`, containing the given
/// arguments:
///
/// ```
/// # use rpds::*;
/// #
/// let m = HashTrieMap::new_sync()
///     .insert(1, "one")
///     .insert(2, "two")
///     .insert(3, "three");
///
/// assert_eq!(ht_map_sync![1 => "one", 2 => "two", 3 => "three"], m);
/// ```
#[macro_export]
macro_rules! ht_map_sync {
    ($($k:expr => $v:expr),*) => {
        {
            #[allow(unused_mut)]
            let mut m = $crate::HashTrieMap::new_sync();
            $(
                m.insert_mut($k, $v);
            )*
            m
        }
    };
}

/// A persistent map with structural sharing.  This implementation uses a
/// [hash array mapped trie](https://en.wikipedia.org/wiki/Hash_array_mapped_trie).
///
/// # Complexity
///
/// Let *n* be the number of elements in the map.
///
/// ## Temporal complexity
///
/// | Operation                  | Average   | Worst case  |
/// |:-------------------------- | ---------:| -----------:|
/// | `new()`                    |      Θ(1) |        Θ(1) |
/// | `insert()`                 |      Θ(1) |        Θ(n) |
/// | `remove()`                 |      Θ(1) |        Θ(n) |
/// | `get()`                    |      Θ(1) |        Θ(n) |
/// | `contains_key()`           |      Θ(1) |        Θ(n) |
/// | `size()`                   |      Θ(1) |        Θ(1) |
/// | `clone()`                  |      Θ(1) |        Θ(1) |
/// | iterator creation          |      Θ(1) |        Θ(1) |
/// | iterator step              |      Θ(1) |        Θ(1) |
/// | iterator full              |      Θ(n) |        Θ(n) |
///
/// # Implementation details
///
/// This implementation uses a
/// [hash array mapped trie](https://en.wikipedia.org/wiki/Hash_array_mapped_trie).
/// Details can be found in
/// [Ideal Hash Trees](https://infoscience.epfl.ch/record/64398/files/idealhashtrees.pdf).
///
/// See the `Node` documentation for details.
#[derive(Debug)]
pub struct HashTrieMap<K, V, P = RcK, H: BuildHasher = DefaultBuildHasher>
where
    P: SharedPointerKind,
{
    root: SharedPointer<Node<K, V, P>, P>,
    size: usize,
    degree: u8,
    hasher_builder: H,
}

pub type HashTrieMapSync<K, V, H = DefaultBuildHasher> = HashTrieMap<K, V, ArcK, H>;

/// This map works like a trie that breaks the hash of the key in segments, and the segments are
/// used as the index in the trie branches.
///
/// Consider the following example, where we have a tree with degree 16 (e.g. each level uses 4
/// bits of the hash) and the following mapping between keys and their hashes:
///
/// | *key*   | *hash(key)*                       |
/// | ------- | ---------------------------------:|
/// |   *A*   | `0b_0000_0000_···_0000_0010_0110` |
/// |   *B*   | `0b_0000_0000_···_0000_0001_0110` |
/// |   *C*   | `0b_0000_0000_···_0000_0100_0010` |
/// |   *D*   | `0b_0111_0000_···_0000_0000_1000` |
/// |   *E*   | `0b_0111_0000_···_0000_0000_1000` |
///
/// Then the tree will look like this:
///
/// ```text
///        0  ···  2  ···  6  ···  8  ···
///      ├───┼───┼───┼───┼───┼───┼───┼───┤
///      │ ∅ │ ∅ │ C │ ∅ │ • │ ∅ │ • │ ∅ │                depth 0
///      └───┴───┴───┴───┴─│─┴───┴─│─┴───┘
///                       ╱         ╲
///                      ╱           ╲
///                     ╱             ╲
///         0   1   2  ···            0   1   2  ···
///       ├───┼───┼───┼───┤         ├───┼───┼───┼───┤
///       │ ∅ │ B │ A │ ∅ │         │ • │ ∅ │ ∅ │ ∅ │     depth 1
///       └───┴───┴───┴───┘         └─│─┴───┴───┴───┘
///                                   │
///                                   ·
///                                   ·
///                                   ·
///                                   │
///                            0  ···   7   ···
///                          ├───┼───┼─────┼───┤
///                          │ ∅ │ ∅ │ D E │ ∅ │          depth 16 (maximum depth)
///                          └───┴───┴─────┴───┘
/// ```
///
/// Note that we stop the insertion process early when possible.  In the example above we did not
/// had to expand the tree any further to accommodate *C*, since there is no other entry with a
/// hash that starts with `0b0010`.  The entries *A* and *B* exemplifies the case where a single
/// level is not enough because their hash both start with `0b0110`.  In case of a full hash
/// collision we dig through all the levels of the tree so we get to the final leaf where a
/// collision exists, like we can see in the case of *D* and *E*.
///
/// # Invariants
///
/// The tree has the following invariants (among others):
///
///   1. The root is the only node that can have zero children.
///   2. A node with a collision can only exist at the maximum depth of the tree.
///   3. A non-root branch always have two or more entries under it (because it could be
///      compressed).
#[derive(Debug)]
enum Node<K, V, P = RcK>
where
    P: SharedPointerKind,
{
    Branch(SparseArrayUsize<SharedPointer<Node<K, V, P>, P>>),
    Leaf(Bucket<K, V, P>),
}

#[derive(Debug)]
enum Bucket<K, V, P = RcK>
where
    P: SharedPointerKind,
{
    Single(EntryWithHash<K, V, P>),
    Collision(ListSync<EntryWithHash<K, V, P>>),
}

#[derive(Debug)]
struct EntryWithHash<K, V, P = RcK>
where
    P: SharedPointerKind,
{
    entry: SharedPointer<Entry<K, V>, P>,
    key_hash: HashValue,
}

mod node_utils {
    use super::HashValue;
    use core::hash::BuildHasher;
    use core::hash::Hash;
    use core::hash::Hasher;
    use core::mem::size_of_val;

    // Returns the index of the array for the given hash on depth `depth`.
    //
    // When the hash is exhausted, meaning that we are at the maximum depth, this returns `None`.
    #[inline]
    pub fn index_from_hash(hash: HashValue, depth: usize, degree: u8) -> Option<usize> {
        debug_assert!(degree.is_power_of_two());

        let shift = depth as u32 * degree.trailing_zeros();

        if (shift as usize) < 8 * size_of_val(&hash) {
            let mask = degree as HashValue - 1;
            Some(((hash >> shift) & mask) as usize)
        } else {
            None
        }
    }

    pub fn hash<T: ?Sized + Hash, H: BuildHasher>(v: &T, hasher_builder: &H) -> HashValue {
        let mut hasher = hasher_builder.build_hasher();

        v.hash(&mut hasher);

        hasher.finish()
    }
}

impl<K, V, P> Node<K, V, P>
where
    K: Eq + Hash,
    P: SharedPointerKind,
{
    fn new_empty_branch() -> Node<K, V, P> {
        Node::Branch(SparseArrayUsize::new())
    }

    fn get<Q: ?Sized>(
        &self,
        key: &Q,
        key_hash: HashValue,
        depth: usize,
        degree: u8,
    ) -> Option<&EntryWithHash<K, V, P>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        match self {
            Node::Branch(subtrees) => {
                let index: usize = node_utils::index_from_hash(key_hash, depth, degree)
                    .expect("hash cannot be exhausted if we are on a branch");

                subtrees
                    .get(index)
                    .and_then(|subtree| subtree.get(key, key_hash, depth + 1, degree))
            }
            Node::Leaf(bucket) => bucket.get(key, key_hash),
        }
    }

    /// Returns a pair with the node with the new entry and whether the key is new.
    fn insert(&mut self, entry: EntryWithHash<K, V, P>, depth: usize, degree: u8) -> bool {
        match self {
            Node::Branch(subtrees) => {
                let index: usize = node_utils::index_from_hash(entry.key_hash, depth, degree)
                    .expect("hash cannot be exhausted if we are on a branch");

                match subtrees.get_mut(index) {
                    Some(subtree) => {
                        SharedPointer::make_mut(subtree).insert(entry, depth + 1, degree)
                    }

                    None => {
                        let new_subtree = Node::Leaf(Bucket::Single(entry));
                        subtrees.set(index, SharedPointer::new(new_subtree));
                        true
                    }
                }
            }
            Node::Leaf(bucket) => {
                // If we are at maximum depth then the hash was totally consumed and we have a
                // collision.
                let maximum_depth =
                    node_utils::index_from_hash(entry.key_hash, depth, degree).is_none();

                let bucket_contains_key: bool = bucket.contains_key(entry.key(), entry.key_hash);

                match maximum_depth {
                    // We reached a bucket.  If the bucket contains the key we are inserting then
                    // we just need to replace it.
                    false if bucket_contains_key => bucket.insert(entry),

                    // We reached a bucket and the key we will insert is not there.  We need to
                    // create a `Node::Branch` and insert the elements of the bucket there, as well
                    // as the new element.
                    false => {
                        // TODO This clone should not be needed.
                        let old_entry: EntryWithHash<K, V, P> = match bucket {
                            Bucket::Single(e) => e.clone(),
                            Bucket::Collision(_) => unreachable!(
                                "hash is not exhausted, so there cannot be a collision here"
                            ),
                        };

                        *self = Node::new_empty_branch();

                        self.insert(old_entry, depth, degree);
                        self.insert(entry, depth, degree);

                        true
                    }

                    // Hash was already totally consumed.  This is a collision.
                    true => bucket.insert(entry),
                }
            }
        }
    }

    /// Compresses a node.  This makes the shallowest tree that is well-formed, i.e. branches with
    /// a single entry become a leaf with it.
    fn compress(&mut self) {
        let new_node = match self {
            Node::Branch(subtrees) => {
                match subtrees.size() {
                    1 => {
                        let compress: bool = {
                            let subtree = subtrees.first().unwrap();

                            // Keep collision at the bottom of the tree.
                            match subtree.borrow() {
                                Node::Leaf(Bucket::Single(_)) => true,
                                _ => false,
                            }
                        };

                        match compress {
                            true => subtrees.pop(),
                            false => None,
                        }
                    }
                    _ => None,
                }
            }
            Node::Leaf(_) => None,
        };

        if let Some(node) = new_node {
            crate::utils::replace(self, node);
        }
    }

    /// Returns `true` if the key was present.
    fn remove<Q: ?Sized>(&mut self, key: &Q, key_hash: HashValue, depth: usize, degree: u8) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        match self {
            Node::Branch(subtrees) => {
                let index: usize = node_utils::index_from_hash(key_hash, depth, degree)
                    .expect("hash cannot be exhausted if we are on a branch");

                match subtrees.get_mut(index) {
                    Some(subtree) => {
                        let subtree = SharedPointer::make_mut(subtree);
                        let removed = subtree.remove(key, key_hash, depth + 1, degree);

                        match (subtree.is_empty(), removed) {
                            (_, false) => (),
                            (false, true) => {
                                // Note that we still must call compress because it is possible that
                                // we had a node with just one entry, which was not compressed
                                // because it had a collision.  Maybe now we do not have a collision
                                // and we can compress it.
                                self.compress();
                            }
                            (true, true) => {
                                subtrees.remove(index);

                                self.compress();
                            }
                        };

                        removed
                    }

                    None => false,
                }
            }

            Node::Leaf(bucket) => {
                let mut bucket_ref = Some(bucket);
                let removed = Bucket::remove(&mut bucket_ref, key, key_hash);

                if bucket_ref.is_none() {
                    // TODO Most of these empty branches will be dropped very soon.  We might
                    //      gain some speed if we avoid this.  (However, currently no heap
                    //      allocation happens anyway.)
                    //      We can do something similar to Bucket::remove() where we receive
                    //      a `&mut Option<&mut Bucket<_, _>>`.
                    *self = Node::new_empty_branch();
                }

                removed
            }
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Node::Branch(subtrees) => subtrees.size() == 0,
            Node::Leaf(Bucket::Single(_)) => false,
            Node::Leaf(Bucket::Collision(entries)) => {
                debug_assert!(entries.len() >= 2, "collisions must have at least two entries");
                false
            }
        }
    }
}

impl<K, V, P> Clone for Node<K, V, P>
where
    K: Eq + Hash,
    P: SharedPointerKind,
{
    fn clone(&self) -> Node<K, V, P> {
        match self {
            Node::Branch(subtrees) => Node::Branch(subtrees.clone()),
            Node::Leaf(bucket) => Node::Leaf(bucket.clone()),
        }
    }
}

mod bucket_utils {
    use super::*;

    /// Returns `true` if an element was removed.
    pub fn list_remove_first<T: Clone, F: Fn(&T) -> bool>(
        list: &mut ListSync<T>,
        predicate: F,
    ) -> bool {
        let mut before_needle: Vec<T> = Vec::with_capacity(list.len());
        let remaining: &mut ListSync<T> = list;
        let mut removed = false;

        while !remaining.is_empty() {
            let e: T = remaining.first().unwrap().clone();

            remaining.drop_first_mut();

            if predicate(&e) {
                removed = true;
                break;
            }

            before_needle.push(e);
        }

        let new_entries = remaining;

        while let Some(e) = before_needle.pop() {
            new_entries.push_front_mut(e);
        }

        removed
    }
}

impl<K, V, P> Bucket<K, V, P>
where
    K: Eq + Hash,
    P: SharedPointerKind,
{
    fn get<Q: ?Sized>(&self, key: &Q, key_hash: HashValue) -> Option<&EntryWithHash<K, V, P>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        match self {
            Bucket::Single(entry) if entry.matches(key, key_hash) => Some(entry.borrow()),
            Bucket::Single(_) => None,
            Bucket::Collision(entries) => {
                entries.iter().find(|e| e.matches(key, key_hash)).map(|e| e.borrow())
            }
        }
    }

    #[inline]
    fn contains_key<Q: ?Sized>(&self, key: &Q, key_hash: HashValue) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        self.get(key, key_hash).is_some()
    }

    /// Returns `true` if the key is new.
    ///
    /// If there is a collision then `entry` will be put on the front of the entries list to
    /// improve performance with high temporal locality (since `get()` will try to match according
    /// to the list order).  The order of the rest of the list must be preserved for the same
    /// reason.
    fn insert(&mut self, entry: EntryWithHash<K, V, P>) -> bool {
        match self {
            Bucket::Single(existing_entry)
                if existing_entry.matches(entry.key(), entry.key_hash) =>
            {
                *existing_entry = entry;
                false
            }
            Bucket::Single(existing_entry) => {
                // TODO In theory we should not need to clone `existing_entry`.
                let entries = list_sync!(entry, existing_entry.clone());

                *self = Bucket::Collision(entries);

                true
            }
            Bucket::Collision(entries) => {
                let key_existed = bucket_utils::list_remove_first(entries, |e| {
                    e.matches(entry.key(), entry.key_hash)
                });

                entries.push_front_mut(entry);

                !key_existed
            }
        }
    }

    /// Returns `true` if the key was present.
    ///
    /// If the bucket becomes empty `bucket` it be set to `None`.
    fn remove<Q: ?Sized>(
        bucket: &mut Option<&mut Bucket<K, V, P>>,
        key: &Q,
        key_hash: HashValue,
    ) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        match bucket.take() {
            Some(b) => {
                match b {
                    Bucket::Single(existing_entry) if existing_entry.matches(key, key_hash) => {
                        // bucket is already `None`.
                        true
                    }
                    Bucket::Single(_) => {
                        // Nothing to change.
                        *bucket = Some(b);
                        false
                    }

                    Bucket::Collision(entries) => {
                        let removed =
                            bucket_utils::list_remove_first(entries, |e| e.matches(key, key_hash));

                        match entries.len() {
                            0 => unreachable!(
                                "impossible to have collision with a single or no entry"
                            ),
                            1 => {
                                let entry = entries.first().unwrap().clone();

                                *b = Bucket::Single(entry);
                            }
                            _ => (),
                        };

                        *bucket = Some(b);

                        removed
                    }
                }
            }
            None => false,
        }
    }
}

impl<K, V, P> Clone for Bucket<K, V, P>
where
    K: Eq + Hash,
    P: SharedPointerKind,
{
    fn clone(&self) -> Bucket<K, V, P> {
        match self {
            Bucket::Single(entry) => Bucket::Single(EntryWithHash::clone(entry)),
            Bucket::Collision(entries) => Bucket::Collision(List::clone(entries)),
        }
    }
}

impl<K, V, P> EntryWithHash<K, V, P>
where
    K: Eq + Hash,
    P: SharedPointerKind,
{
    fn new<H: BuildHasher>(key: K, value: V, hash_builder: &H) -> EntryWithHash<K, V, P> {
        let key_hash = node_utils::hash(&key, hash_builder);

        EntryWithHash { entry: SharedPointer::new(Entry::new(key, value)), key_hash }
    }

    fn key(&self) -> &K {
        &self.entry.key
    }

    fn value(&self) -> &V {
        &self.entry.value
    }

    #[inline]
    fn matches<Q: ?Sized>(&self, key: &Q, key_hash: HashValue) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        self.key_hash == key_hash && self.key().borrow() == key
    }
}

impl<K, V, P> Clone for EntryWithHash<K, V, P>
where
    K: Eq + Hash,
    P: SharedPointerKind,
{
    fn clone(&self) -> EntryWithHash<K, V, P> {
        EntryWithHash { entry: SharedPointer::clone(&self.entry), key_hash: self.key_hash }
    }
}

impl<K, V> HashTrieMap<K, V>
where
    K: Eq + Hash,
{
    #[must_use]
    pub fn new() -> HashTrieMap<K, V> {
        HashTrieMap::new_with_degree(DEFAULT_DEGREE)
    }

    #[must_use]
    pub fn new_with_degree(degree: u8) -> HashTrieMap<K, V> {
        HashTrieMap::new_with_hasher_and_degree_and_ptr_kind(DefaultBuildHasher::default(), degree)
    }
}

impl<K, V> HashTrieMapSync<K, V>
where
    K: Eq + Hash,
{
    #[must_use]
    pub fn new_sync() -> HashTrieMapSync<K, V> {
        HashTrieMap::new_sync_with_degree(DEFAULT_DEGREE)
    }

    #[must_use]
    pub fn new_sync_with_degree(degree: u8) -> HashTrieMapSync<K, V> {
        HashTrieMap::new_with_hasher_and_degree_and_ptr_kind(DefaultBuildHasher::default(), degree)
    }
}

impl<K, V, P, H: BuildHasher> HashTrieMap<K, V, P, H>
where
    K: Eq + Hash,
    H: Clone,
    P: SharedPointerKind,
{
    #[must_use]
    pub fn new_with_hasher_and_ptr_kind(hasher_builder: H) -> HashTrieMap<K, V, P, H> {
        HashTrieMap::new_with_hasher_and_degree_and_ptr_kind(hasher_builder, DEFAULT_DEGREE)
    }

    #[must_use]
    pub fn new_with_hasher_and_degree_and_ptr_kind(
        hasher_builder: H,
        degree: u8,
    ) -> HashTrieMap<K, V, P, H> {
        assert!(degree.is_power_of_two(), "degree must be a power of two");
        assert!(degree <= DEFAULT_DEGREE, "degree is too big");

        HashTrieMap {
            root: SharedPointer::new(Node::new_empty_branch()),
            size: 0,
            degree,
            hasher_builder,
        }
    }

    #[must_use]
    pub fn get<Q: ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        let key_hash = node_utils::hash(key, &self.hasher_builder);

        self.root.get(key, key_hash, 0, self.degree).map(|e| e.value())
    }

    #[must_use]
    pub fn insert(&self, key: K, value: V) -> HashTrieMap<K, V, P, H> {
        let mut new_map = self.clone();

        new_map.insert_mut(key, value);

        new_map
    }

    pub fn insert_mut(&mut self, key: K, value: V) {
        let entry = EntryWithHash::new(key, value, &self.hasher_builder);
        let is_new_key = SharedPointer::make_mut(&mut self.root).insert(entry, 0, self.degree);

        if is_new_key {
            self.size += 1;
        }
    }

    #[must_use]
    pub fn remove<Q: ?Sized>(&self, key: &Q) -> HashTrieMap<K, V, P, H>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        let mut new_map = self.clone();

        if new_map.remove_mut(key) {
            new_map
        } else {
            // We want to keep maximum sharing so in case of no change we just `clone()` ourselves.
            self.clone()
        }
    }

    pub fn remove_mut<Q: ?Sized>(&mut self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        let key_hash = node_utils::hash(key, &self.hasher_builder);
        let removed = SharedPointer::make_mut(&mut self.root).remove(key, key_hash, 0, self.degree);

        // Note that unfortunately, even if nothing was removed, we still might have cloned some
        // part of the tree unnecessarily.

        if removed {
            self.size -= 1;
        }

        removed
    }

    #[must_use]
    pub fn contains_key<Q: ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        self.get(key).is_some()
    }

    #[must_use]
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.size() == 0
    }

    #[must_use]
    pub fn iter(&self) -> Iter<'_, K, V, P> {
        self.iter_ptr().map(|e| (&e.key, &e.value))
    }

    #[must_use]
    fn iter_ptr(&self) -> IterPtr<'_, K, V, P> {
        IterPtr::new(self)
    }

    #[must_use]
    pub fn keys(&self) -> IterKeys<'_, K, V, P> {
        self.iter().map(|(k, _)| k)
    }

    #[must_use]
    pub fn values(&self) -> IterValues<'_, K, V, P> {
        self.iter().map(|(_, v)| v)
    }
}

impl<'a, K, Q: ?Sized, V, P, H: BuildHasher> Index<&'a Q> for HashTrieMap<K, V, P, H>
where
    K: Eq + Hash + Borrow<Q>,
    Q: Hash + Eq,
    H: Clone,
    P: SharedPointerKind,
{
    type Output = V;

    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("no entry found for key")
    }
}

impl<K, V, P, H: BuildHasher> Clone for HashTrieMap<K, V, P, H>
where
    K: Eq + Hash,
    H: Clone,
    P: SharedPointerKind,
{
    fn clone(&self) -> HashTrieMap<K, V, P, H> {
        HashTrieMap {
            root: SharedPointer::clone(&self.root),
            size: self.size,
            degree: self.degree,
            hasher_builder: self.hasher_builder.clone(),
        }
    }
}

impl<K, V, P, H: BuildHasher> Default for HashTrieMap<K, V, P, H>
where
    K: Eq + Hash,
    H: Default + Clone,
    P: SharedPointerKind,
{
    fn default() -> HashTrieMap<K, V, P, H> {
        HashTrieMap::new_with_hasher_and_ptr_kind(H::default())
    }
}

impl<K: Eq, V: PartialEq, P, PO, H: BuildHasher> PartialEq<HashTrieMap<K, V, PO, H>>
    for HashTrieMap<K, V, P, H>
where
    K: Hash,
    H: Clone,
    P: SharedPointerKind,
    PO: SharedPointerKind,
{
    fn eq(&self, other: &HashTrieMap<K, V, PO, H>) -> bool {
        self.size() == other.size()
            && self.iter().all(|(key, value)| other.get(key).map_or(false, |v| *value == *v))
    }
}

impl<K: Eq, V: Eq, P, H: BuildHasher> Eq for HashTrieMap<K, V, P, H>
where
    K: Hash,
    H: Clone,
    P: SharedPointerKind,
{
}

impl<K, V, P, H: BuildHasher> Display for HashTrieMap<K, V, P, H>
where
    K: Eq + Hash + Display,
    V: Display,
    H: Clone,
    P: SharedPointerKind,
{
    fn fmt(&self, fmt: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;

        fmt.write_str("{")?;

        for (k, v) in self.iter() {
            if !first {
                fmt.write_str(", ")?;
            }
            k.fmt(fmt)?;
            fmt.write_str(": ")?;
            v.fmt(fmt)?;
            first = false;
        }

        fmt.write_str("}")
    }
}

impl<'a, K, V, P, H: BuildHasher> IntoIterator for &'a HashTrieMap<K, V, P, H>
where
    K: Eq + Hash,
    H: Default + Clone,
    P: SharedPointerKind,
{
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V, P>;

    fn into_iter(self) -> Iter<'a, K, V, P> {
        self.iter()
    }
}

impl<K, V, P, H> FromIterator<(K, V)> for HashTrieMap<K, V, P, H>
where
    K: Eq + Hash,
    H: BuildHasher + Clone + Default,
    P: SharedPointerKind,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(into_iter: I) -> HashTrieMap<K, V, P, H> {
        let mut map = HashTrieMap::new_with_hasher_and_ptr_kind(Default::default());

        for (k, v) in into_iter {
            map.insert_mut(k, v);
        }

        map
    }
}

#[derive(Debug)]
pub struct IterPtr<'a, K, V, P>
where
    P: SharedPointerKind,
{
    stack: Vec<IterStackElement<'a, K, V, P>>,
    size: usize,
}

#[derive(Debug)]
enum IterStackElement<'a, K, V, P>
where
    P: SharedPointerKind,
{
    Branch(Peekable<slice::Iter<'a, SharedPointer<Node<K, V, P>, P>>>),
    LeafSingle(&'a EntryWithHash<K, V, P>),
    LeafCollision(Peekable<list::Iter<'a, EntryWithHash<K, V, P>, ArcK>>),
}

impl<'a, K, V, P> IterStackElement<'a, K, V, P>
where
    K: Eq + Hash,
    P: SharedPointerKind,
{
    fn new(node: &Node<K, V, P>) -> IterStackElement<'_, K, V, P> {
        match node {
            Node::Branch(children) => IterStackElement::Branch(children.iter().peekable()),
            Node::Leaf(Bucket::Single(entry)) => IterStackElement::LeafSingle(entry),
            Node::Leaf(Bucket::Collision(entries)) => {
                IterStackElement::LeafCollision(entries.iter().peekable())
            }
        }
    }

    fn current_elem(&mut self) -> &'a SharedPointer<Entry<K, V>, P> {
        match self {
            IterStackElement::Branch(_) => panic!("called current element of a branch"),
            IterStackElement::LeafSingle(entry) => &entry.entry,
            IterStackElement::LeafCollision(iter) => &iter.peek().unwrap().entry,
        }
    }

    /// Advance and returns `true` if finished.
    #[inline]
    fn advance(&mut self) -> bool {
        match self {
            IterStackElement::Branch(iter) => {
                iter.next();
                iter.peek().is_none()
            }
            IterStackElement::LeafSingle(_) => true,
            IterStackElement::LeafCollision(iter) => {
                iter.next();
                iter.peek().is_none()
            }
        }
    }
}

mod iter_utils {
    use super::HashValue;
    use core::mem::size_of;

    pub fn trie_max_height(degree: u8) -> usize {
        let bits_per_level = (degree - 1).count_ones() as usize;
        let hash_bits = 8 * size_of::<HashValue>();

        (hash_bits / bits_per_level) + if hash_bits % bits_per_level > 0 { 1 } else { 0 }
    }
}

impl<'a, K, V, P> IterPtr<'a, K, V, P>
where
    K: Eq + Hash,
    P: SharedPointerKind,
{
    fn new<H: BuildHasher + Clone>(map: &HashTrieMap<K, V, P, H>) -> IterPtr<'_, K, V, P> {
        let mut stack: Vec<IterStackElement<'_, K, V, P>> =
            Vec::with_capacity(iter_utils::trie_max_height(map.degree) + 1);

        if map.size() > 0 {
            stack.push(IterStackElement::new(map.root.borrow()));
        }

        let mut iter = IterPtr { stack, size: map.size() };

        iter.dig();

        iter
    }

    fn dig(&mut self) {
        let next_stack_elem: Option<IterStackElement<'_, K, V, P>> =
            self.stack.last_mut().and_then(|stack_top| match stack_top {
                IterStackElement::Branch(iter) => {
                    iter.peek().map(|node| IterStackElement::new(node))
                }
                _ => None,
            });

        if let Some(e) = next_stack_elem {
            self.stack.push(e);
            self.dig();
        }
    }

    fn advance(&mut self) {
        if let Some(mut stack_element) = self.stack.pop() {
            let finished = stack_element.advance();

            if finished {
                self.advance();
            } else {
                self.stack.push(stack_element);

                self.dig();
            }
        }
    }

    fn current(&mut self) -> Option<&'a SharedPointer<Entry<K, V>, P>> {
        self.stack.last_mut().map(|e| e.current_elem())
    }
}

impl<'a, K, V, P> Iterator for IterPtr<'a, K, V, P>
where
    K: Eq + Hash,
    P: SharedPointerKind,
{
    type Item = &'a SharedPointer<Entry<K, V>, P>;

    fn next(&mut self) -> Option<&'a SharedPointer<Entry<K, V>, P>> {
        let current = self.current();

        self.advance();

        if current.is_some() {
            self.size -= 1;
        }

        current
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.size, Some(self.size))
    }
}

impl<'a, K: Eq + Hash, V, P> ExactSizeIterator for IterPtr<'a, K, V, P> where P: SharedPointerKind {}

#[cfg(feature = "serde")]
pub mod serde {
    use super::*;
    use ::serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
    use ::serde::ser::{Serialize, Serializer};
    use core::fmt;
    use core::marker::PhantomData;

    impl<K, V, P, H> Serialize for HashTrieMap<K, V, P, H>
    where
        K: Eq + Hash + Serialize,
        V: Serialize,
        H: BuildHasher + Clone + Default,
        P: SharedPointerKind,
    {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_map(self)
        }
    }

    impl<'de, K, V, P, H> Deserialize<'de> for HashTrieMap<K, V, P, H>
    where
        K: Eq + Hash + Deserialize<'de>,
        V: Deserialize<'de>,
        H: BuildHasher + Clone + Default,
        P: SharedPointerKind,
    {
        fn deserialize<D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<HashTrieMap<K, V, P, H>, D::Error> {
            deserializer.deserialize_map(HashTrieMapVisitor {
                _phantom_entry: PhantomData,
                _phantom_h: PhantomData,
                _phantom_p: PhantomData,
            })
        }
    }

    struct HashTrieMapVisitor<K, V, P, H>
    where
        P: SharedPointerKind,
    {
        _phantom_entry: PhantomData<(K, V)>,
        _phantom_h: PhantomData<H>,
        _phantom_p: PhantomData<P>,
    }

    impl<'de, K, V, P, H> Visitor<'de> for HashTrieMapVisitor<K, V, P, H>
    where
        K: Eq + Hash + Deserialize<'de>,
        V: Deserialize<'de>,
        H: BuildHasher + Clone + Default,
        P: SharedPointerKind,
    {
        type Value = HashTrieMap<K, V, P, H>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map")
        }

        fn visit_map<A>(self, mut map: A) -> Result<HashTrieMap<K, V, P, H>, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut hash_trie_map = HashTrieMap::new_with_hasher_and_ptr_kind(Default::default());

            while let Some((k, v)) = map.next_entry()? {
                hash_trie_map.insert_mut(k, v);
            }

            Ok(hash_trie_map)
        }
    }
}

#[cfg(test)]
mod test;

// This file is part of Substrate.

// Copyright (C) 2015-2021 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Utility functions to interact with Substrate's Base-16 Modified Merkle Patricia tree ("trie").

#![cfg_attr(not(feature = "std"), no_std)]

mod error;
mod node_header;
mod node_codec;
mod storage_proof;
mod trie_stream;

use sp_std::{boxed::Box, marker::PhantomData, vec, vec::Vec, borrow::Borrow, fmt};
use hash_db::{Hasher, Prefix};
//use trie_db::proof::{generate_proof, verify_proof};
pub use trie_db::proof::VerifyError;
/// Our `NodeCodec`-specific error.
pub use error::Error;
/// The Substrate format implementation of `TrieStream`.
pub use trie_stream::TrieStream;
/// The Substrate format implementation of `NodeCodec`.
pub use node_codec::NodeCodec;
pub use storage_proof::StorageProof;
/// Various re-exports from the `trie-db` crate.
pub use trie_db::{
	Trie, TrieMut, DBValue, Recorder, CError, Query, TrieLayout, TrieConfiguration,
	nibble_ops, TrieDBIterator, Meta, NodeChange, node::{NodePlan, ValuePlan},
};
/// Various re-exports from the `memory-db` crate.
pub use memory_db::KeyFunction;
pub use memory_db::prefixed_key;
/// Various re-exports from the `hash-db` crate.
pub use hash_db::{HashDB as HashDBT, EMPTY_PREFIX, MetaHasher};
pub use hash_db::NoMeta;

/// Meta use by trie state.
#[derive(Default, Clone)]
pub struct TrieMeta {
	// range of encoded value or hashed value.
	pub range: Option<core::ops::Range<usize>>,
	// When `do_value_hash` is true, try to
	// store this behavior in top node
	// encoded (need to be part of state).
	pub recorded_do_value_hash: bool,
	// Does current encoded contains a hash instead of
	// a value (information stored in meta for proofs).
	pub contain_hash: bool,
	// Flag indicating if value hash can run.
	// When defined for a node it gets active
	// for all children node
	pub do_value_hash: bool,
	// Record if a value was accessed, this is
	// set as accessed by defalult, but can be
	// change on access explicitely: `HashDB::get_with_meta`.
	// and reset on access explicitely: `HashDB::access_from`.
	pub unused_value: bool,
	// Indicate that a node is using old hash scheme.
	// Write with `do_value_hash` inactive will set this to
	// true.
	// In this case hash is not doing internal hashing,
	// but next write with `do_value_hash` will remove switch scheme.
	pub old_hash: bool,
}

impl Meta for TrieMeta {
	/// Layout do not have content.
	type MetaInput = ();

	/// When true apply inner hashing of value.
	type StateMeta = bool;

	fn set_state_meta(&mut self, state_meta: Self::StateMeta) {
		self.recorded_do_value_hash = state_meta;
		self.do_value_hash = state_meta;
	}

	fn has_state_meta(&self) -> bool {
		self.recorded_do_value_hash
	}

	fn read_state_meta(&mut self, data: &[u8]) -> Result<usize, &'static str> {
		let offset = if data[0] == trie_constants::ENCODED_META_ALLOW_HASH {
			self.recorded_do_value_hash = true;
			self.do_value_hash = true;
			1
		} else {
			0
		};
		Ok(offset)
	}

	fn write_state_meta(&self) -> Vec<u8> {
		if self.recorded_do_value_hash {
			// Note that this only works with sp_trie codec that
			// cannot encode node starting by this byte.
			[trie_constants::ENCODED_META_ALLOW_HASH].to_vec()
		} else {
			Vec::new()
		}
	}

	fn meta_for_new(
		_input: Self::MetaInput,
		parent: Option<&Self>,
	) -> Self {
		let mut result = Self::default();
		result.do_value_hash = parent.map(|p| p.do_value_hash).unwrap_or_default();
		result
	}

	fn meta_for_existing_inline_node(
		input: Self::MetaInput,
		parent: Option<&Self>,
	) -> Self {
		Self::meta_for_new(input, parent)
	}

	fn meta_for_empty(
	) -> Self {
		Default::default()
	}

	fn set_value_callback(
		&mut self,
		_new_value: Option<&[u8]>,
		_is_branch: bool,
		changed: NodeChange,
	) -> NodeChange {
		changed
	}

	fn encoded_value_callback(
		&mut self,
		value_plan: ValuePlan,
	) {
		let (contain_hash, range) = match value_plan {
			ValuePlan::Value(range) => (false, range),
			ValuePlan::HashedValue(range, _size) => (true, range),
			ValuePlan::NoValue => return,
		};

		self.range = Some(range);
		self.contain_hash = contain_hash;
		if self.do_value_hash {
			// Switch value hashing.
			self.old_hash = false;
		}
	}

	fn set_child_callback(
		&mut self,
		_child: Option<&Self>,
		changed: NodeChange,
		_at: usize,
	) -> NodeChange {
		changed
	}

	fn decoded_callback(
		&mut self,
		node_plan: &NodePlan,
	) {
		let (contain_hash, range) = match node_plan.value_plan() {
			Some(ValuePlan::Value(range)) => (false, range.clone()),
			Some(ValuePlan::HashedValue(range, _size)) => (true, range.clone()),
			Some(ValuePlan::NoValue) => return,
			None => return,
		};

		self.range = Some(range);
		self.contain_hash = contain_hash;
	}

	fn contains_hash_of_value(&self) -> bool {
		self.contain_hash
	}

	fn do_value_hash(&self) -> bool {
		self.unused_value
	}
}

impl TrieMeta {
	/// Was value accessed.
	pub fn accessed_value(&mut self) -> bool {
		!self.unused_value
	}

	/// For proof, this allow setting node as unaccessed until
	/// a call to `access_from`.
	pub fn set_accessed_value(&mut self, accessed: bool) {
		self.unused_value = !accessed;
	}
}

/// substrate trie layout
pub struct Layout<H, M>(sp_std::marker::PhantomData<(H, M)>);

impl<H, M> fmt::Debug for Layout<H, M> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.debug_struct("Layout").finish()
	}
}

impl<H, M> Default for Layout<H, M> {
	fn default() -> Self {
		Layout(sp_std::marker::PhantomData)
	}
}

impl<H, M> Clone for Layout<H, M> {
	fn clone(&self) -> Self {
		Layout(sp_std::marker::PhantomData)
	}
}

impl<H, M> TrieLayout for Layout<H, M>
	where
		H: Hasher,
		M: MetaHasher<H, DBValue>,
		M::Meta: Meta<MetaInput = ()>,
{
	const USE_EXTENSION: bool = false;
	const ALLOW_EMPTY: bool = true;
	const USE_META: bool = true;
	type Hash = H;
	type Codec = NodeCodec<Self::Hash>;
	type MetaHasher = M;
	type Meta = M::Meta;

	fn metainput_for_new_node(&self) -> <Self::Meta as Meta>::MetaInput {
		()
	}
	fn metainput_for_stored_inline_node(&self) -> <Self::Meta as Meta>::MetaInput {
		()
	}
}

/// Hasher with support to meta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateHasher;

impl<H> MetaHasher<H, DBValue> for StateHasher
	where
		H: Hasher,
{
	type Meta = TrieMeta;

	fn hash(value: &[u8], meta: &Self::Meta) -> H::Out {
		match &meta {
			TrieMeta { range: Some(range), contain_hash: false, do_value_hash, old_hash: false, .. } => {
				if *do_value_hash && range.end - range.start >= trie_constants::INNER_HASH_TRESHOLD {
					let value = inner_hashed_value::<H>(value, Some((range.start, range.end)));
					H::hash(value.as_slice())
				} else {
					H::hash(value)
				}
			},
			TrieMeta { range: Some(_range), contain_hash: true, .. } => {
				// value contains a hash of data (already inner_hashed_value).
				H::hash(value)
			},
			_ => {
				H::hash(value)
			},
		}
	}

	fn stored_value(value: &[u8], mut meta: Self::Meta) -> DBValue {
		let mut stored = Vec::with_capacity(value.len() + 1);
		if meta.old_hash {
			// write as old hash.
			stored.push(trie_constants::OLD_HASHING);
			stored.extend_from_slice(value);
			return stored;
		}
		if !meta.do_value_hash {
			if let Some(range) = meta.range.as_ref() {
				if range.end - range.start >= trie_constants::INNER_HASH_TRESHOLD {
					// write as old hash.
					stored.push(trie_constants::OLD_HASHING);
					stored.extend_from_slice(value);
					return stored;
				}
			}
		}
		if meta.contain_hash {
			// already contain hash, just flag it.
			stored.push(trie_constants::DEAD_HEADER_META_HASHED_VALUE);
			stored.extend_from_slice(value);
			return stored;
		}
		if meta.unused_value {
			if let Some(range) = meta.range.as_ref() {
				if range.end - range.start >= trie_constants::INNER_HASH_TRESHOLD {
					// Waring this assume that encoded value does not start by this, so it is tightly coupled
					// with the header type of the codec: only for optimization.
					stored.push(trie_constants::DEAD_HEADER_META_HASHED_VALUE);
					let range = meta.range.as_ref().expect("Tested in condition");
					meta.contain_hash = true; // useless but could be with meta as &mut
					// store hash instead of value.
					let value = inner_hashed_value::<H>(value, Some((range.start, range.end)));
					stored.extend_from_slice(value.as_slice());
					return stored;
				}
			}
		}
		stored.extend_from_slice(value);
		stored
	}

	fn stored_value_owned(value: DBValue, meta: Self::Meta) -> DBValue {
		<Self as MetaHasher<H, DBValue>>::stored_value(value.as_slice(), meta)
	}

	fn extract_value<'a>(mut stored: &'a [u8], parent_meta: Option<&Self::Meta>) -> (&'a [u8], Self::Meta) {
		let input = &mut stored;
		let mut contain_hash = false;
		let mut old_hash = false;
		if input.get(0) == Some(&trie_constants::DEAD_HEADER_META_HASHED_VALUE) {
			contain_hash = true;
			*input = &input[1..];
		}
		if input.get(0) == Some(&trie_constants::OLD_HASHING) {
			old_hash = true;
			*input = &input[1..];
		}
		let mut meta = TrieMeta {
			range: None,
			unused_value: contain_hash,
			contain_hash,
			do_value_hash: false,
			recorded_do_value_hash: false,
			old_hash,
		};
		// get recorded_do_value_hash
		let _offset = meta.read_state_meta(stored)
			.expect("State meta reading failure.");
		//let stored = &stored[offset..];
		meta.do_value_hash = meta.recorded_do_value_hash || parent_meta.map(|m| m.do_value_hash).unwrap_or(false);
		(stored, meta)
	}

	fn extract_value_owned(mut stored: DBValue, parent_meta: Option<&Self::Meta>) -> (DBValue, Self::Meta) {
		let len = stored.len();
		let (v, meta) = <Self as MetaHasher<H, DBValue>>::extract_value(stored.as_slice(), parent_meta);
		let removed = len - v.len();
		(stored.split_off(removed), meta)
	}
}

/// Reimplement `NoMeta` `MetaHasher` with
/// additional constraint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoMetaHasher;

impl<H> MetaHasher<H, DBValue> for NoMetaHasher 
	where
		H: Hasher,
{
	type Meta = TrieMeta;

	fn hash(value: &[u8], _meta: &Self::Meta) -> H::Out {
		H::hash(value)
	}

	fn stored_value(value: &[u8], _meta: Self::Meta) -> DBValue {
		value.to_vec()
	}

	fn stored_value_owned(value: DBValue, _meta: Self::Meta) -> DBValue {
		value
	}

	fn extract_value<'a>(stored: &'a [u8], _parent_meta: Option<&Self::Meta>) -> (&'a [u8], Self::Meta) {
		(stored, Default::default())
	}

	fn extract_value_owned(stored: DBValue, _parent_meta: Option<&Self::Meta>) -> (DBValue, Self::Meta) {
		(stored, Default::default())
	}
}

impl<H, M> TrieConfiguration for Layout<H, M>
	where
		H: Hasher,
		M: MetaHasher<H, DBValue>,
		M::Meta: Meta<MetaInput = ()>,
{
	fn trie_root<I, A, B>(&self, input: I) -> <Self::Hash as Hasher>::Out where
		I: IntoIterator<Item = (A, B)>,
		A: AsRef<[u8]> + Ord,
		B: AsRef<[u8]>,
	{
		trie_root::trie_root_no_extension::<H, TrieStream, _, _, _>(input)
	}

	fn trie_root_unhashed<I, A, B>(&self, input: I) -> Vec<u8> where
		I: IntoIterator<Item = (A, B)>,
		A: AsRef<[u8]> + Ord,
		B: AsRef<[u8]>,
	{
		trie_root::unhashed_trie_no_extension::<H, TrieStream, _, _, _>(input)
	}

	fn encode_index(input: u32) -> Vec<u8> {
		codec::Encode::encode(&codec::Compact(input))
	}
}

#[cfg(not(feature = "memory-tracker"))]
type MemTracker = memory_db::NoopTracker<trie_db::DBValue>;
#[cfg(feature = "memory-tracker")]
type MemTracker = memory_db::MemCounter<trie_db::DBValue>;

/// TrieDB error over `TrieConfiguration` trait.
pub type TrieError<L> = trie_db::TrieError<TrieHash<L>, CError<L>>;
/// Reexport from `hash_db`, with genericity set for `Hasher` trait.
pub trait AsHashDB<H: Hasher, M>: hash_db::AsHashDB<H, trie_db::DBValue, M> {}
impl<H: Hasher, M, T: hash_db::AsHashDB<H, trie_db::DBValue, M>> AsHashDB<H, M> for T {}
/// Reexport from `hash_db`, with genericity set for `Hasher` trait.
pub type HashDB<'a, H, M> = dyn hash_db::HashDB<H, trie_db::DBValue, M> + 'a;
/// Reexport from `hash_db`, with genericity set for `Hasher` trait.
/// This uses a `KeyFunction` for prefixing keys internally (avoiding
/// key conflict for non random keys).
pub type PrefixedMemoryDB<H> = memory_db::MemoryDB<
	H, memory_db::PrefixedKey<H>, trie_db::DBValue, StateHasher, MemTracker
>;
/// Reexport from `hash_db`, with genericity set for `Hasher` trait.
/// This uses a noops `KeyFunction` (key addressing must be hashed or using
/// an encoding scheme that avoid key conflict).
pub type MemoryDB<H> = memory_db::MemoryDB<
	H, memory_db::HashKey<H>, trie_db::DBValue, StateHasher, MemTracker,
>;
/// Reexport from `hash_db`, with genericity set for `Hasher` trait.
/// This uses a noops `KeyFunction` (key addressing must be hashed or using
/// an encoding scheme that avoid key conflict).
pub type MemoryDBNoMeta<H> = memory_db::MemoryDB<
	H, memory_db::HashKey<H>, trie_db::DBValue, NoMetaHasher, MemTracker,
>;
/// MemoryDB with specific meta hasher.
pub type MemoryDBMeta<H, M> = memory_db::MemoryDB<
	H, memory_db::HashKey<H>, trie_db::DBValue, M, MemTracker,
>;

/// Reexport from `hash_db`, with genericity set for `Hasher` trait.
pub type GenericMemoryDB<H, KF, MH> = memory_db::MemoryDB<
	H, KF, trie_db::DBValue, MH, MemTracker
>;

/// Persistent trie database read-access interface for the a given hasher.
pub type TrieDB<'a, L> = trie_db::TrieDB<'a, L>;
/// Persistent trie database write-access interface for the a given hasher.
pub type TrieDBMut<'a, L> = trie_db::TrieDBMut<'a, L>;
/// Querying interface, as in `trie_db` but less generic.
pub type Lookup<'a, L, Q> = trie_db::Lookup<'a, L, Q>;
/// Hash type for a trie layout.
pub type TrieHash<L> = <<L as TrieLayout>::Hash as Hasher>::Out;
/// This module is for non generic definition of trie type.
/// Only the `Hasher` trait is generic in this case.
pub mod trie_types {
	/// State layout.
	pub type Layout<H> = super::Layout<H, super::StateHasher>;
	/// Old state layout definition, do not use meta, do not
	/// do internal value hashing.
	pub type LayoutNoMeta<H> = super::Layout<H, super::NoMetaHasher>;
	/// Persistent trie database read-access interface for the a given hasher.
	pub type TrieDB<'a, H> = super::TrieDB<'a, Layout<H>>;
	/// Persistent trie database write-access interface for the a given hasher.
	pub type TrieDBMut<'a, H> = super::TrieDBMut<'a, Layout<H>>;
	/// Persistent trie database write-access interface for the a given hasher,
	/// old layout.
	pub type TrieDBMutNoMeta<'a, H> = super::TrieDBMut<'a, LayoutNoMeta<H>>;
	/// Querying interface, as in `trie_db` but less generic.
	pub type Lookup<'a, H, Q> = trie_db::Lookup<'a, Layout<H>, Q>;
	/// As in `trie_db`, but less generic, error type for the crate.
	pub type TrieError<H> = trie_db::TrieError<H, super::Error>;
}

/*
/// Create a proof for a subset of keys in a trie.
///
/// The `keys` may contain any set of keys regardless of each one of them is included
/// in the `db`.
///
/// For a key `K` that is included in the `db` a proof of inclusion is generated.
/// For a key `K` that is not included in the `db` a proof of non-inclusion is generated.
/// These can be later checked in `verify_trie_proof`.
pub fn generate_trie_proof<'a, L: TrieConfiguration, I, K, DB>(
	db: &DB,
	root: TrieHash<L>,
	keys: I,
) -> Result<Vec<Vec<u8>>, Box<TrieError<L>>> where
	I: IntoIterator<Item=&'a K>,
	K: 'a + AsRef<[u8]>,
	DB: hash_db::HashDBRef<L::Hash, trie_db::DBValue, L::Meta>,
{
	let trie = TrieDB::<L>::new(db, &root)?;
	generate_proof(&trie, keys)
}

/// Verify a set of key-value pairs against a trie root and a proof.
///
/// Checks a set of keys with optional values for inclusion in the proof that was generated by
/// `generate_trie_proof`.
/// If the value in the pair is supplied (`(key, Some(value))`), this key-value pair will be
/// checked for inclusion in the proof.
/// If the value is omitted (`(key, None)`), this key will be checked for non-inclusion in the
/// proof.
pub fn verify_trie_proof<'a, L: TrieConfiguration, I, K, V>(
	root: &TrieHash<L>,
	proof: &[Vec<u8>],
	items: I,
) -> Result<(), VerifyError<TrieHash<L>, error::Error>> where
	I: IntoIterator<Item=&'a (K, Option<V>)>,
	K: 'a + AsRef<[u8]>,
	V: 'a + AsRef<[u8]>,
{
	verify_proof::<Layout<L::Hash>, _, _, _>(root, proof, items)
}
*/

/// Determine a trie root given a hash DB and delta values.
pub fn delta_trie_root<L: TrieConfiguration, I, A, B, DB, V>(
	db: &mut DB,
	mut root: TrieHash<L>,
	delta: I
) -> Result<TrieHash<L>, Box<TrieError<L>>> where
	I: IntoIterator<Item = (A, B)>,
	A: Borrow<[u8]>,
	B: Borrow<Option<V>>,
	V: Borrow<[u8]>,
	DB: hash_db::HashDB<L::Hash, trie_db::DBValue, L::Meta>,
{
	{
		let mut trie = TrieDBMut::<L>::from_existing(db, &mut root)?;

		let mut delta = delta.into_iter().collect::<Vec<_>>();
		delta.sort_by(|l, r| l.0.borrow().cmp(r.0.borrow()));

		for (key, change) in delta {
			match change.borrow() {
				Some(val) => trie.insert(key.borrow(), val.borrow())?,
				None => trie.remove(key.borrow())?,
			};
		}
	}

	Ok(root)
}

/// Flag inner trie with state metadata to enable hash of value internally.
pub fn flag_inner_meta_hasher<L, DB>(
	db: &mut DB,
	mut root: TrieHash<L>,
) -> Result<TrieHash<L>, Box<TrieError<L>>> where
	L: TrieConfiguration<Meta = TrieMeta>,
	DB: hash_db::HashDB<L::Hash, trie_db::DBValue, L::Meta>,
{
	{
		let mut t = TrieDBMut::<L>::from_existing(db, &mut root)?;
		flag_meta_hasher(&mut t)?;
	}
	Ok(root)
}

/// Flag inner trie with state metadata to enable hash of value internally.
pub fn flag_meta_hasher<L>(
	t: &mut TrieDBMut<L>
) -> Result<(), Box<TrieError<L>>> where
	L: TrieConfiguration<Meta = TrieMeta>,
{
	let flag = true;
	let key: &[u8]= &[];
	if !t.contains(key)? {
		t.insert(key, b"")?;
	}
	assert!(t.flag(key, flag)?);
	Ok(())
}

/// Read a value from the trie.
pub fn read_trie_value<L: TrieConfiguration, DB: hash_db::HashDBRef<L::Hash, trie_db::DBValue, L::Meta>>(
	db: &DB,
	root: &TrieHash<L>,
	key: &[u8]
) -> Result<Option<Vec<u8>>, Box<TrieError<L>>> {
	Ok(TrieDB::<L>::new(&*db, root)?.get(key).map(|x| x.map(|val| val.to_vec()))?)
}

/// Read a value from the trie with given Query.
pub fn read_trie_value_with<
	L: TrieConfiguration,
	Q: Query<L::Hash, Item=DBValue>,
	DB: hash_db::HashDBRef<L::Hash, trie_db::DBValue, L::Meta>
>(
	db: &DB,
	root: &TrieHash<L>,
	key: &[u8],
	query: Q
) -> Result<Option<Vec<u8>>, Box<TrieError<L>>> {
	Ok(TrieDB::<L>::new(&*db, root)?.get_with(key, query).map(|x| x.map(|val| val.to_vec()))?)
}

/// Determine the empty trie root.
pub fn empty_trie_root<L: TrieConfiguration>() -> <L::Hash as Hasher>::Out {
	L::default().trie_root::<_, Vec<u8>, Vec<u8>>(core::iter::empty())
}

/// Determine the empty child trie root.
pub fn empty_child_trie_root<L: TrieConfiguration>() -> <L::Hash as Hasher>::Out {
	L::default().trie_root::<_, Vec<u8>, Vec<u8>>(core::iter::empty())
}

/// Determine a child trie root given its ordered contents, closed form. H is the default hasher,
/// but a generic implementation may ignore this type parameter and use other hashers.
pub fn child_trie_root<L: TrieConfiguration, I, A, B>(
	layout: &L,
	input: I,
) -> <L::Hash as Hasher>::Out
	where
		I: IntoIterator<Item = (A, B)>,
		A: AsRef<[u8]> + Ord,
		B: AsRef<[u8]>,
{
	layout.trie_root(input)
}

/// Determine a child trie root given a hash DB and delta values. H is the default hasher,
/// but a generic implementation may ignore this type parameter and use other hashers.
pub fn child_delta_trie_root<L: TrieConfiguration, I, A, B, DB, RD, V>(
	keyspace: &[u8],
	db: &mut DB,
	root_data: RD,
	delta: I,
) -> Result<<L::Hash as Hasher>::Out, Box<TrieError<L>>>
	where
		I: IntoIterator<Item = (A, B)>,
		A: Borrow<[u8]>,
		B: Borrow<Option<V>>,
		V: Borrow<[u8]>,
		RD: AsRef<[u8]>,
		DB: hash_db::HashDB<L::Hash, trie_db::DBValue, L::Meta>
{
	let mut root = TrieHash::<L>::default();
	// root is fetched from DB, not writable by runtime, so it's always valid.
	root.as_mut().copy_from_slice(root_data.as_ref());

	let mut db = KeySpacedDBMut::new(&mut *db, keyspace);
	delta_trie_root::<L, _, _, _, _, _>(
		&mut db,
		root,
		delta,
	)
}

/// Call `f` for all keys in a child trie.
/// Aborts as soon as `f` returns false.
pub fn for_keys_in_child_trie<L: TrieConfiguration, F: FnMut(&[u8]) -> bool, DB>(
	keyspace: &[u8],
	db: &DB,
	root_slice: &[u8],
	mut f: F
) -> Result<(), Box<TrieError<L>>>
	where
		DB: hash_db::HashDBRef<L::Hash, trie_db::DBValue, L::Meta>
{
	let mut root = TrieHash::<L>::default();
	// root is fetched from DB, not writable by runtime, so it's always valid.
	root.as_mut().copy_from_slice(root_slice);

	let db = KeySpacedDB::new(&*db, keyspace);
	let trie = TrieDB::<L>::new(&db, &root)?;
	let iter = trie.iter()?;

	for x in iter {
		let (key, _) = x?;
		if !f(&key) {
			break;
		}
	}

	Ok(())
}

/// Record all keys for a given root.
pub fn record_all_keys<L: TrieConfiguration, DB>(
	db: &DB,
	root: &TrieHash<L>,
	recorder: &mut Recorder<TrieHash<L>>
) -> Result<(), Box<TrieError<L>>> where
	DB: hash_db::HashDBRef<L::Hash, trie_db::DBValue, L::Meta>
{
	let trie = TrieDB::<L>::new(&*db, root)?;
	let iter = trie.iter()?;

	for x in iter {
		let (key, _) = x?;

		// there's currently no API like iter_with()
		// => use iter to enumerate all keys AND lookup each
		// key using get_with
		trie.get_with(&key, &mut *recorder)?;
	}

	Ok(())
}

/// Read a value from the child trie.
pub fn read_child_trie_value<L: TrieConfiguration, DB>(
	keyspace: &[u8],
	db: &DB,
	root_slice: &[u8],
	key: &[u8]
) -> Result<Option<Vec<u8>>, Box<TrieError<L>>>
	where
		DB: hash_db::HashDBRef<L::Hash, trie_db::DBValue, L::Meta>
{
	let mut root = TrieHash::<L>::default();
	// root is fetched from DB, not writable by runtime, so it's always valid.
	root.as_mut().copy_from_slice(root_slice);

	let db = KeySpacedDB::new(&*db, keyspace);
	Ok(TrieDB::<L>::new(&db, &root)?.get(key).map(|x| x.map(|val| val.to_vec()))?)
}

/// Read a value from the child trie with given query.
pub fn read_child_trie_value_with<L: TrieConfiguration, Q: Query<L::Hash, Item=DBValue>, DB>(
	keyspace: &[u8],
	db: &DB,
	root_slice: &[u8],
	key: &[u8],
	query: Q
) -> Result<Option<Vec<u8>>, Box<TrieError<L>>>
	where
		DB: hash_db::HashDBRef<L::Hash, trie_db::DBValue, L::Meta>
{
	let mut root = TrieHash::<L>::default();
	// root is fetched from DB, not writable by runtime, so it's always valid.
	root.as_mut().copy_from_slice(root_slice);

	let db = KeySpacedDB::new(&*db, keyspace);
	Ok(TrieDB::<L>::new(&db, &root)?.get_with(key, query).map(|x| x.map(|val| val.to_vec()))?)
}

/// `HashDB` implementation that append a encoded prefix (unique id bytes) in addition to the
/// prefix of every key value.
pub struct KeySpacedDB<'a, DB, H>(&'a DB, &'a [u8], PhantomData<H>);

/// `HashDBMut` implementation that append a encoded prefix (unique id bytes) in addition to the
/// prefix of every key value.
///
/// Mutable variant of `KeySpacedDB`, see [`KeySpacedDB`].
pub struct KeySpacedDBMut<'a, DB, H>(&'a mut DB, &'a [u8], PhantomData<H>);

/// Utility function used to merge some byte data (keyspace) and `prefix` data
/// before calling key value database primitives.
fn keyspace_as_prefix_alloc(ks: &[u8], prefix: Prefix) -> (Vec<u8>, Option<u8>) {
	let mut result = sp_std::vec![0; ks.len() + prefix.0.len()];
	result[..ks.len()].copy_from_slice(ks);
	result[ks.len()..].copy_from_slice(prefix.0);
	(result, prefix.1)
}

impl<'a, DB, H> KeySpacedDB<'a, DB, H> where
	H: Hasher,
{
	/// instantiate new keyspaced db
	pub fn new(db: &'a DB, ks: &'a [u8]) -> Self {
		KeySpacedDB(db, ks, PhantomData)
	}
}

impl<'a, DB, H> KeySpacedDBMut<'a, DB, H> where
	H: Hasher,
{
	/// instantiate new keyspaced db
	pub fn new(db: &'a mut DB, ks: &'a [u8]) -> Self {
		KeySpacedDBMut(db, ks, PhantomData)
	}
}

impl<'a, DB, H, T, M> hash_db::HashDBRef<H, T, M> for KeySpacedDB<'a, DB, H> where
	DB: hash_db::HashDBRef<H, T, M>,
	H: Hasher,
	T: From<&'static [u8]>,
{
	fn get(&self, key: &H::Out, prefix: Prefix) -> Option<T> {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.get(key, (&derived_prefix.0, derived_prefix.1))
	}

	fn access_from(&self, key: &H::Out, at: Option<&H::Out>) -> Option<T> {
		self.0.access_from(key, at)
	}

	fn get_with_meta(&self, key: &H::Out, prefix: Prefix, parent: Option<&M>) -> Option<(T, M)> {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.get_with_meta(key, (&derived_prefix.0, derived_prefix.1), parent)
	}

	fn contains(&self, key: &H::Out, prefix: Prefix) -> bool {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.contains(key, (&derived_prefix.0, derived_prefix.1))
	}
}

impl<'a, DB, H, T, M> hash_db::HashDB<H, T, M> for KeySpacedDBMut<'a, DB, H> where
	DB: hash_db::HashDB<H, T, M>,
	H: Hasher,
	T: Default + PartialEq<T> + for<'b> From<&'b [u8]> + Clone + Send + Sync,
{
	fn get(&self, key: &H::Out, prefix: Prefix) -> Option<T> {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.get(key, (&derived_prefix.0, derived_prefix.1))
	}

	fn access_from(&self, key: &H::Out, at: Option<&H::Out>) -> Option<T> {
		self.0.access_from(key, at)
	}

	fn get_with_meta(&self, key: &H::Out, prefix: Prefix, parent: Option<&M>) -> Option<(T, M)> {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.get_with_meta(key, (&derived_prefix.0, derived_prefix.1), parent)
	}

	fn contains(&self, key: &H::Out, prefix: Prefix) -> bool {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.contains(key, (&derived_prefix.0, derived_prefix.1))
	}

	fn insert(&mut self, prefix: Prefix, value: &[u8]) -> H::Out {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.insert((&derived_prefix.0, derived_prefix.1), value)
	}

	fn insert_with_meta(
		&mut self,
		prefix: Prefix,
		value: &[u8],
		meta: M,
	) -> H::Out {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.insert_with_meta((&derived_prefix.0, derived_prefix.1), value, meta)
	}

	fn emplace(&mut self, key: H::Out, prefix: Prefix, value: T) {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.emplace(key, (&derived_prefix.0, derived_prefix.1), value)
	}

	fn remove(&mut self, key: &H::Out, prefix: Prefix) {
		let derived_prefix = keyspace_as_prefix_alloc(self.1, prefix);
		self.0.remove(key, (&derived_prefix.0, derived_prefix.1))
	}
}

impl<'a, DB, H, T, M> hash_db::AsHashDB<H, T, M> for KeySpacedDBMut<'a, DB, H> where
	DB: hash_db::HashDB<H, T, M>,
	H: Hasher,
	T: Default + PartialEq<T> + for<'b> From<&'b [u8]> + Clone + Send + Sync,
{
	fn as_hash_db(&self) -> &dyn hash_db::HashDB<H, T, M> { &*self }

	fn as_hash_db_mut<'b>(&'b mut self) -> &'b mut (dyn hash_db::HashDB<H, T, M> + 'b) {
		&mut *self
	}
}

/// Representation of node with with inner hash instead of value.
fn inner_hashed_value<H: Hasher>(x: &[u8], range: Option<(usize, usize)>) -> Vec<u8> {
	if let Some((start, end)) = range {
		let len = x.len();
		if start < len && end == len {
			// terminal inner hash
			let hash_end = H::hash(&x[start..]);
			let mut buff = vec![0; x.len() + hash_end.as_ref().len() - (end - start)];
			buff[..start].copy_from_slice(&x[..start]);
			buff[start..].copy_from_slice(hash_end.as_ref());
			return buff;
		}
		if start == 0 && end < len {
			// start inner hash
			let hash_start = H::hash(&x[..start]);
			let hash_len = hash_start.as_ref().len();
			let mut buff = vec![0; x.len() + hash_len - (end - start)];
			buff[..hash_len].copy_from_slice(hash_start.as_ref());
			buff[hash_len..].copy_from_slice(&x[end..]);
			return buff;
		}
		if start < len && end < len {
			// middle inner hash
			let hash_middle = H::hash(&x[start..end]);
			let hash_len = hash_middle.as_ref().len();
			let mut buff = vec![0; x.len() + hash_len - (end - start)];
			buff[..start].copy_from_slice(&x[..start]);
			buff[start..start + hash_len].copy_from_slice(hash_middle.as_ref());
			buff[start + hash_len..].copy_from_slice(&x[end..]);
			return buff;
		}
	}
	// if anything wrong default to hash
	x.to_vec()
}

/// Estimate encoded size of node.
pub fn estimate_entry_size(entry: &(DBValue, TrieMeta), hash_len: usize) -> usize {
	use codec::Encode;
	let mut full_encoded = entry.0.encoded_size();
	if entry.1.unused_value {
		if let Some(range) = entry.1.range.as_ref() {
			let value_size = range.end - range.start;
			if range.end - range.start >= trie_constants::INNER_HASH_TRESHOLD {
				full_encoded -= value_size;
				full_encoded += hash_len;
				full_encoded += 1;
			}
		}
	}

	full_encoded
}

/// If needed, call to decode plan in order to record meta.
pub fn resolve_encoded_meta<H: Hasher>(entry: &mut (DBValue, TrieMeta)) {
	use trie_db::NodeCodec;
	if entry.1.do_value_hash {
		let _ = <trie_types::Layout::<H> as TrieLayout>::Codec::decode_plan(entry.0.as_slice(), &mut entry.1);
	}
}

/// Constants used into trie simplification codec.
mod trie_constants {
	/// Treshold for using hash of value instead of value
	/// in encoded trie node when flagged.
	pub const INNER_HASH_TRESHOLD: usize = 33;
	const FIRST_PREFIX: u8 = 0b_00 << 6;
	pub const EMPTY_TRIE: u8 = FIRST_PREFIX | 0b_00;
	pub const ENCODED_META_ALLOW_HASH: u8 = FIRST_PREFIX | 0b_01;
	/// In proof this header is used when only hashed value is stored.
	pub const DEAD_HEADER_META_HASHED_VALUE: u8 = FIRST_PREFIX | 0b_00_10;
	/// If inner hashing should apply, but state is not flagged, then set
	/// this meta to avoid checking both variant of hashes.
	pub const OLD_HASHING: u8 = FIRST_PREFIX | 0b_00_11;
	pub const NIBBLE_SIZE_BOUND: usize = u16::max_value() as usize;
	pub const LEAF_PREFIX_MASK: u8 = 0b_01 << 6;
	pub const BRANCH_WITHOUT_MASK: u8 = 0b_10 << 6;
	pub const BRANCH_WITH_MASK: u8 = 0b_11 << 6;
}

#[cfg(test)]
mod tests {
	use super::*;
	use codec::{Encode, Decode, Compact};
	use sp_core::Blake2Hasher;
	use hash_db::{HashDB, Hasher};
	use trie_db::{DBValue, TrieMut, Trie, NodeCodec as NodeCodecT};
	use trie_standardmap::{Alphabet, ValueMode, StandardMap};
	use hex_literal::hex;

	type Layout = super::trie_types::Layout<Blake2Hasher>;

	fn hashed_null_node<T: TrieConfiguration>() -> TrieHash<T> {
		<T::Codec as NodeCodecT<T::Meta>>::hashed_null_node()
	}

	fn check_equivalent<T: TrieConfiguration>(input: &Vec<(&[u8], &[u8])>) {
		{
			// TODO test flagged
			let layout = T::default();
			let closed_form = layout.trie_root(input.clone());
			let d = layout.trie_root_unhashed(input.clone());
			println!("Data: {:#x?}, {:#x?}", d, Blake2Hasher::hash(&d[..]));
			let persistent = {
				let mut memdb = MemoryDBMeta::<_, T::MetaHasher>::default();
				let mut root = Default::default();
				// TODO test flagged
				let mut t = TrieDBMut::<T>::new(&mut memdb, &mut root);
				for (x, y) in input.iter().rev() {
					t.insert(x, y).unwrap();
				}
				t.root().clone()
			};
			assert_eq!(closed_form, persistent);
		}
	}

	fn check_iteration<T: TrieConfiguration>(input: &Vec<(&[u8], &[u8])>) {
		let mut memdb = MemoryDBMeta::<_, T::MetaHasher>::default();
		let mut root = Default::default();
		{
			let mut t = TrieDBMut::<T>::new(&mut memdb, &mut root);
			for (x, y) in input.clone() {
				t.insert(x, y).unwrap();
			}
		}
		{
			let t = TrieDB::<T>::new(&mut memdb, &root).unwrap();
			assert_eq!(
				input.iter().map(|(i, j)| (i.to_vec(), j.to_vec())).collect::<Vec<_>>(),
				t.iter().unwrap()
					.map(|x| x.map(|y| (y.0, y.1.to_vec())).unwrap())
					.collect::<Vec<_>>()
			);
		}
	}

	#[test]
	fn default_trie_root() {
		let mut db = MemoryDB::default();
		let mut root = TrieHash::<Layout>::default();
		let mut empty = TrieDBMut::<Layout>::new(&mut db, &mut root);
		empty.commit();
		let root1 = empty.root().as_ref().to_vec();
		let root2: Vec<u8> = Layout::default().trie_root::<_, Vec<u8>, Vec<u8>>(
			std::iter::empty(),
		).as_ref().iter().cloned().collect();

		assert_eq!(root1, root2);
	}

	#[test]
	fn empty_is_equivalent() {
		let input: Vec<(&[u8], &[u8])> = vec![];
		check_equivalent::<Layout>(&input);
		check_iteration::<Layout>(&input);
	}

	#[test]
	fn leaf_is_equivalent() {
		let input: Vec<(&[u8], &[u8])> = vec![(&[0xaa][..], &[0xbb][..])];
		check_equivalent::<Layout>(&input);
		check_iteration::<Layout>(&input);
	}

	#[test]
	fn branch_is_equivalent() {
		let input: Vec<(&[u8], &[u8])> = vec![
			(&[0xaa][..], &[0x10][..]),
			(&[0xba][..], &[0x11][..]),
		];
		check_equivalent::<Layout>(&input);
		check_iteration::<Layout>(&input);
	}

	#[test]
	fn extension_and_branch_is_equivalent() {
		let input: Vec<(&[u8], &[u8])> = vec![
			(&[0xaa][..], &[0x10][..]),
			(&[0xab][..], &[0x11][..]),
		];
		check_equivalent::<Layout>(&input);
		check_iteration::<Layout>(&input);
	}

	#[test]
	fn standard_is_equivalent() {
		let st = StandardMap {
			alphabet: Alphabet::All,
			min_key: 32,
			journal_key: 0,
			value_mode: ValueMode::Random,
			count: 1000,
		};
		let mut d = st.make();
		d.sort_by(|&(ref a, _), &(ref b, _)| a.cmp(b));
		let dr = d.iter().map(|v| (&v.0[..], &v.1[..])).collect();
		check_equivalent::<Layout>(&dr);
		check_iteration::<Layout>(&dr);
	}

	#[test]
	fn extension_and_branch_with_value_is_equivalent() {
		let input: Vec<(&[u8], &[u8])> = vec![
			(&[0xaa][..], &[0xa0][..]),
			(&[0xaa, 0xaa][..], &[0xaa][..]),
			(&[0xaa, 0xbb][..], &[0xab][..])
		];
		check_equivalent::<Layout>(&input);
		check_iteration::<Layout>(&input);
	}

	#[test]
	fn bigger_extension_and_branch_with_value_is_equivalent() {
		let input: Vec<(&[u8], &[u8])> = vec![
			(&[0xaa][..], &[0xa0][..]),
			(&[0xaa, 0xaa][..], &[0xaa][..]),
			(&[0xaa, 0xbb][..], &[0xab][..]),
			(&[0xbb][..], &[0xb0][..]),
			(&[0xbb, 0xbb][..], &[0xbb][..]),
			(&[0xbb, 0xcc][..], &[0xbc][..]),
		];
		check_equivalent::<Layout>(&input);
		check_iteration::<Layout>(&input);
	}

	#[test]
	fn single_long_leaf_is_equivalent() {
		let input: Vec<(&[u8], &[u8])> = vec![
			(&[0xaa][..], &b"ABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABC"[..]),
			(&[0xba][..], &[0x11][..]),
		];
		check_equivalent::<Layout>(&input);
		check_iteration::<Layout>(&input);
	}

	#[test]
	fn two_long_leaves_is_equivalent() {
		let input: Vec<(&[u8], &[u8])> = vec![
			(&[0xaa][..], &b"ABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABC"[..]),
			(&[0xba][..], &b"ABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABCABC"[..])
		];
		check_equivalent::<Layout>(&input);
		check_iteration::<Layout>(&input);
	}

	// TODO add flag
	fn populate_trie<'db, T: TrieConfiguration>(
		db: &'db mut dyn HashDB<T::Hash, DBValue, T::Meta>,
		root: &'db mut TrieHash<T>,
		v: &[(Vec<u8>, Vec<u8>)]
	) -> TrieDBMut<'db, T> {
		let mut t = TrieDBMut::<T>::new(db, root);
		for i in 0..v.len() {
			let key: &[u8]= &v[i].0;
			let val: &[u8] = &v[i].1;
			t.insert(key, val).unwrap();
		}
		t
	}

	fn unpopulate_trie<'db, T: TrieConfiguration>(
		t: &mut TrieDBMut<'db, T>,
		v: &[(Vec<u8>, Vec<u8>)],
	) {
		for i in v {
			let key: &[u8]= &i.0;
			t.remove(key).unwrap();
		}
	}

	#[test]
	fn random_should_work() {
		let mut seed = <Blake2Hasher as Hasher>::Out::zero();
		for test_i in 0..10000 {
			if test_i % 50 == 0 {
				println!("{:?} of 10000 stress tests done", test_i);
			}
			let x = StandardMap {
				alphabet: Alphabet::Custom(b"@QWERTYUIOPASDFGHJKLZXCVBNM[/]^_".to_vec()),
				min_key: 5,
				journal_key: 0,
				value_mode: ValueMode::Index,
				count: 100,
			}.make_with(seed.as_fixed_bytes_mut());

			// TODO test other layout states.
			let layout = Layout::default();
			let real = layout.trie_root(x.clone());
			let mut memdb = MemoryDB::default();
			let mut root = Default::default();
			let mut memtrie = populate_trie::<Layout>(&mut memdb, &mut root, &x);

			memtrie.commit();
			if *memtrie.root() != real {
				println!("TRIE MISMATCH");
				println!("");
				println!("{:?} vs {:?}", memtrie.root(), real);
				for i in &x {
					println!("{:#x?} -> {:#x?}", i.0, i.1);
				}
			}
			assert_eq!(*memtrie.root(), real);
			unpopulate_trie::<Layout>(&mut memtrie, &x);
			memtrie.commit();
			let hashed_null_node = hashed_null_node::<Layout>();
			if *memtrie.root() != hashed_null_node {
				println!("- TRIE MISMATCH");
				println!("");
				println!("{:?} vs {:?}", memtrie.root(), hashed_null_node);
				for i in &x {
					println!("{:#x?} -> {:#x?}", i.0, i.1);
				}
			}
			assert_eq!(*memtrie.root(), hashed_null_node);
		}
	}

	fn to_compact(n: u8) -> u8 {
		Compact(n).encode()[0]
	}

	#[test]
	fn codec_trie_empty() {
		// TODO test other layout states.
		let layout = Layout::default();
		let input: Vec<(&[u8], &[u8])> = vec![];
		let trie = layout.trie_root_unhashed(input);
		println!("trie: {:#x?}", trie);
		assert_eq!(trie, vec![0x0]);
	}

	#[test]
	fn codec_trie_single_tuple() {
		// TODO switch to old layout
		let layout = Layout::default();
		let input = vec![
			(vec![0xaa], vec![0xbb])
		];
		let trie = layout.trie_root_unhashed(input);
		println!("trie: {:#x?}", trie);
		assert_eq!(trie, vec![
			0x42,					// leaf 0x40 (2^6) with (+) key of 2 nibbles (0x02)
			0xaa,					// key data
			to_compact(1),			// length of value in bytes as Compact
			0xbb					// value data
		]);
	}

	#[test]
	fn codec_trie_two_tuples_disjoint_keys() {
		// TODO switch to old layout
		let layout = Layout::default();
		let input = vec![(&[0x48, 0x19], &[0xfe]), (&[0x13, 0x14], &[0xff])];
		let trie = layout.trie_root_unhashed(input);
		println!("trie: {:#x?}", trie);
		let mut ex = Vec::<u8>::new();
		ex.push(0x80);									// branch, no value (0b_10..) no nibble
		ex.push(0x12);									// slots 1 & 4 are taken from 0-7
		ex.push(0x00);									// no slots from 8-15
		ex.push(to_compact(0x05));						// first slot: LEAF, 5 bytes long.
		ex.push(0x43);									// leaf 0x40 with 3 nibbles
		ex.push(0x03);									// first nibble
		ex.push(0x14);									// second & third nibble
		ex.push(to_compact(0x01));						// 1 byte data
		ex.push(0xff);									// value data
		ex.push(to_compact(0x05));						// second slot: LEAF, 5 bytes long.
		ex.push(0x43);									// leaf with 3 nibbles
		ex.push(0x08);									// first nibble
		ex.push(0x19);									// second & third nibble
		ex.push(to_compact(0x01));						// 1 byte data
		ex.push(0xfe);									// value data

		assert_eq!(trie, ex);
	}

	#[test]
	fn iterator_works() {
		let pairs = vec![
			(hex!("0103000000000000000464").to_vec(), hex!("0400000000").to_vec()),
			(hex!("0103000000000000000469").to_vec(), hex!("0401000000").to_vec()),
		];

		let mut mdb = MemoryDB::default();
		let mut root = Default::default();
		let _ = populate_trie::<Layout>(&mut mdb, &mut root, &pairs);

		let trie = TrieDB::<Layout>::new(&mdb, &root).unwrap();

		let iter = trie.iter().unwrap();
		let mut iter_pairs = Vec::new();
		for pair in iter {
			let (key, value) = pair.unwrap();
			iter_pairs.push((key, value.to_vec()));
		}

		assert_eq!(pairs, iter_pairs);
	}
/*
	#[test]
	fn proof_non_inclusion_works() {
		let pairs = vec![
			(hex!("0102").to_vec(), hex!("01").to_vec()),
			(hex!("0203").to_vec(), hex!("0405").to_vec()),
		];

		let mut memdb = MemoryDB::default();
		let mut root = Default::default();
		populate_trie::<Layout>(&mut memdb, &mut root, &pairs);

		let non_included_key: Vec<u8> = hex!("0909").to_vec();
		let proof = generate_trie_proof::<Layout, _, _, _>(
			&memdb,
			root,
			&[non_included_key.clone()]
		).unwrap();

		// Verifying that the K was not included into the trie should work.
		assert!(verify_trie_proof::<Layout, _, _, Vec<u8>>(
				&root,
				&proof,
				&[(non_included_key.clone(), None)],
			).is_ok()
		);

		// Verifying that the K was included into the trie should fail.
		assert!(verify_trie_proof::<Layout, _, _, Vec<u8>>(
				&root,
				&proof,
				&[(non_included_key, Some(hex!("1010").to_vec()))],
			).is_err()
		);
	}

	#[test]
	fn proof_inclusion_works() {
		let pairs = vec![
			(hex!("0102").to_vec(), hex!("01").to_vec()),
			(hex!("0203").to_vec(), hex!("0405").to_vec()),
		];

		let mut memdb = MemoryDB::default();
		let mut root = Default::default();
		populate_trie::<Layout>(&mut memdb, &mut root, &pairs);

		let proof = generate_trie_proof::<Layout, _, _, _>(
			&memdb,
			root,
			&[pairs[0].0.clone()]
		).unwrap();

		// Check that a K, V included into the proof are verified.
		assert!(verify_trie_proof::<Layout, _, _, _>(
				&root,
				&proof,
				&[(pairs[0].0.clone(), Some(pairs[0].1.clone()))]
			).is_ok()
		);

		// Absence of the V is not verified with the proof that has K, V included.
		assert!(verify_trie_proof::<Layout, _, _, Vec<u8>>(
				&root,
				&proof,
				&[(pairs[0].0.clone(), None)]
			).is_err()
		);

		// K not included into the trie is not verified.
		assert!(verify_trie_proof::<Layout, _, _, _>(
				&root,
				&proof,
				&[(hex!("4242").to_vec(), Some(pairs[0].1.clone()))]
			).is_err()
		);

		// K included into the trie but not included into the proof is not verified.
		assert!(verify_trie_proof::<Layout, _, _, _>(
				&root,
				&proof,
				&[(pairs[1].0.clone(), Some(pairs[1].1.clone()))]
			).is_err()
		);
	}
*/
	#[test]
	fn generate_storage_root_with_proof_works_independently_from_the_delta_order() {
		let proof = StorageProof::decode(&mut &include_bytes!("../test-res/proof")[..]).unwrap();
		let storage_root = sp_core::H256::decode(
			&mut &include_bytes!("../test-res/storage_root")[..],
		).unwrap();
		// Delta order that is "invalid" so that it would require a different proof.
		let invalid_delta = Vec::<(Vec<u8>, Option<Vec<u8>>)>::decode(
			&mut &include_bytes!("../test-res/invalid-delta-order")[..],
		).unwrap();
		// Delta order that is "valid"
		let valid_delta = Vec::<(Vec<u8>, Option<Vec<u8>>)>::decode(
			&mut &include_bytes!("../test-res/valid-delta-order")[..],
		).unwrap();

		let proof_db = proof.into_memory_db::<Blake2Hasher>();
		let first_storage_root = delta_trie_root::<Layout, _, _, _, _, _>(
			&mut proof_db.clone(),
			storage_root,
			valid_delta,
		).unwrap();
		let second_storage_root = delta_trie_root::<Layout, _, _, _, _, _>(
			&mut proof_db.clone(),
			storage_root,
			invalid_delta,
		).unwrap();

		assert_eq!(first_storage_root, second_storage_root);
	}
}

//! `slate` crate represents Stratified Hash Tree -- an implementation of a list structure with Hash
//! Tree (Merkle Tree) that stores a complete history of additive changes in that tree structure,
//! with efficient append characteristics for practical storage device. This allows data to be
//! appended and, like a typical hash tree, can be used to verify data corruption or tampering with
//! very small amounts of data.
//!
//! See also [my personal research page for more detail](https://hazm.at/mox/algorithm/structural-algorithm/stratified-hash-tree/index.html).
//!
//! # Examples
//!
//! ```rust
//! use slate::{BlockStorage, Slate, Value, Node};
//!
//! let mut slate = Slate::new_on_memory();
//! slate.append("first".as_bytes()).unwrap();
//! slate.append("second".as_bytes()).unwrap();
//! slate.append("third".as_bytes()).unwrap();
//!
//! let mut query = slate.snapshot().query().unwrap();
//! assert_eq!("first".as_bytes(), query.get(1).unwrap().unwrap());
//! assert_eq!("second".as_bytes(), query.get(2).unwrap().unwrap());
//! assert_eq!("third".as_bytes(), query.get(3).unwrap().unwrap());
//! ```
//!
use crate::cache::Cache;
use crate::error::Error;
use crate::error::Error::*;
use crate::formula::{Model, contains, root_of, subnodes_of};
use crate::serialize::{read_entry_from, write_entry_to};
use crate::storage::file::FileDevice;
use crate::storage::memory::{MemKVS, MemoryDevice};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::{Arc, RwLock};

pub mod cache;
pub(crate) mod checksum;
pub mod error;
pub mod formula;
pub mod inspect;
mod serialize;
mod storage;

pub use storage::*;

#[cfg(test)]
pub mod test;

/// slate クレートで使用する標準 Result。[`error::Error`] も参照。
pub type Result<T> = std::result::Result<T, Error>;

/// slate がインデックス i として使用する整数の型です。`u64` を表しています。
pub type Index = formula::Index;

/// [`Index`] 型のビット幅を表す定数です。64 を表しています。
pub const INDEX_SIZE: u8 = formula::INDEX_SIZE;

/// ハッシュ木を構成するノードを表します。
#[derive(PartialEq, Eq, Copy, Clone, Debug)]
pub struct Node {
  /// このノードのインデックス。
  pub i: Index,
  /// このノードの高さ。
  pub j: u8,
  /// このノードのハッシュ値。この値は [`Hash::hash()`] によって算出されています。
  pub hash: Hash,
}

impl Node {
  pub fn new(i: Index, j: u8, hash: Hash) -> Node {
    Node { i, j, hash }
  }

  /// このノードを左枝、`right` ノードを右枝とする親ノードを算出します。
  pub fn parent(&self, right: &Node) -> Node {
    debug_assert!(self.i < right.i);
    debug_assert!(self.j >= right.j);
    let i = right.i;
    let j = self.j + 1;
    let hash = self.hash.combine(j, &right.hash);
    Node::new(i, j, hash)
  }
}

impl Display for Node {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.write_str(&format!("{},{}:{}", self.i, self.j, hex(&self.hash.value)))
  }
}

/// ハッシュ木に保存されている値を表します。
#[derive(PartialEq, Eq, Clone, Debug)]
pub struct Value {
  /// この値のインデックス。
  pub i: Index,
  /// この値のバイナリ値。
  pub value: Vec<u8>,
}

impl Value {
  pub fn new(i: Index, value: Vec<u8>) -> Value {
    Value { i, value }
  }
  /// この値のハッシュ値を算出します。
  pub fn hash(&self) -> Hash {
    Hash::from_bytes(0, &self.value)
  }
  pub fn to_node(&self) -> Node {
    Node::new(self.i, 0u8, self.hash())
  }
}

impl Display for Value {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.write_str(&format!("{}:{}", self.i, hex(&self.value)))
  }
}

/// ハッシュ木から取得した、経路の分岐先のハッシュ値を含む値のセットです。値のハッシュ値と分岐ノードのハッシュ値から
/// ルートハッシュを算出し、クライアントが持つルートハッシュと比較することで、取得した値が改変されていないことを検証
/// することができます。
#[derive(Debug)]
pub struct ValuesWithBranches {
  pub values: Vec<Value>,
  pub branches: Vec<Node>,
}

impl ValuesWithBranches {
  pub fn new(values: Vec<Value>, branches: Vec<Node>) -> ValuesWithBranches {
    // values は連続していなければならない
    #[cfg(debug_assertions)]
    for i in 0..values.len() - 1 {
      debug_assert_eq!(values[i].i + 1, values[i + 1].i);
    }
    ValuesWithBranches { values, branches }
  }

  /// この結果から得られるルートノードをルートハッシュ付きで算出します。
  pub fn root(&self) -> Node {
    // すべての値をハッシュ値に変換する
    let mut hashes = self.values.iter().map(|value| value.to_node()).collect::<Vec<Node>>();

    // 値から算出したハッシュ値を折りたたむ
    while hashes.len() > 1 {
      // hashes の要素を 2 つ一組で折りたたむ (要素数が奇数の場合は最も右もノードが一過性の中間ノード)
      for k in 0..hashes.len() / 2 {
        let left = &hashes[k * 2];
        let right = &hashes[k * 2 + 1];
        hashes[k] = left.parent(right);
      }
      // 折りたたまれていない一過性の中間ノードは次に持ち越す
      let fraction = if hashes.len() % 2 != 0 {
        let len = hashes.len();
        hashes[len / 2] = hashes.pop().unwrap();
        1
      } else {
        0
      };
      hashes.truncate(hashes.len() / 2 + fraction);
    }

    // 経路から分岐したノードのハッシュ値と統合しルートノードを算出する
    let mut folding = hashes.remove(0);
    for k in 0..self.branches.len() {
      let branch = &self.branches[self.branches.len() - k - 1];
      let (left, right) = if folding.i < branch.i { (&folding, branch) } else { (branch, &folding) };
      folding = left.parent(right);
    }
    folding
  }
}

// --------------------------------------------------------------------------

/// ハッシュ木が使用するハッシュ値です。
#[derive(PartialEq, Eq, Copy, Clone)]
pub struct Hash {
  pub value: [u8; Self::SIZE],
}

impl Hash {
  /// ハッシュ値のバイト長を表す定数λです。
  pub const SIZE: usize = if cfg!(feature = "blake3") {
    blake3::OUT_LEN
  } else if cfg!(feature = "sha512") {
    64
  } else if cfg!(any(feature = "sha256", feature = "sha512_256")) {
    32
  } else if cfg!(any(feature = "sha224", feature = "sha512_224")) {
    28
  } else if cfg!(feature = "highwayhash64") {
    8
  } else {
    // Use if cfg!() instead of #[cfg()] to avoid compilation errors and ensure that the strongest algorithm is
    // selected in specifying --all-features.
    panic!("No hash algorithm is specified. At least one hash algorithm must be specified as a feature.")
  };

  pub fn new(hash: [u8; Self::SIZE]) -> Hash {
    Hash { value: hash }
  }

  /// 指定された値をハッシュ化します。
  pub fn from_bytes(j: u8, value: &[u8]) -> Hash {
    if cfg!(feature = "blake3") {
      let mut hasher = blake3::Hasher::new();
      hasher.update(&[j]);
      hasher.update(value);
      hasher.finalize().as_bytes().into()
    } else if cfg!(feature = "sha512") {
      use sha2::Digest;
      let hash: [u8; 64] = sha2::Sha512::new().chain_update(&[j]).chain_update(value).finalize().into();
      (&hash).into()
    } else if cfg!(feature = "sha256") {
      use sha2::Digest;
      let hash: [u8; 32] = sha2::Sha256::new().chain_update(&[j]).chain_update(value).finalize().into();
      (&hash).into()
    } else if cfg!(feature = "sha512_256") {
      use sha2::Digest;
      let hash: [u8; 32] = sha2::Sha512_256::new().chain_update(&[j]).chain_update(value).finalize().into();
      (&hash).into()
    } else if cfg!(feature = "sha224") {
      use sha2::Digest;
      let hash: [u8; 28] = sha2::Sha224::new().chain_update(&[j]).chain_update(value).finalize().into();
      (&hash).into()
    } else if cfg!(feature = "sha512_224") {
      use sha2::Digest;
      let hash: [u8; 28] = sha2::Sha512_224::new().chain_update(&[j]).chain_update(value).finalize().into();
      (&hash).into()
    } else if cfg!(feature = "highwayhash64") {
      use highway::HighwayHash;
      let mut builder = highway::HighwayHasher::default();
      builder.write_all(&[j]).unwrap();
      builder.write_all(value).unwrap();
      (&builder.finalize64().to_le_bytes()).into()
    } else {
      panic!()
    }
  }

  /// 指定されたハッシュ値と連結したハッシュ値 `hash(j || self.hash || other.hash)` を算出します。
  pub fn combine(&self, j: u8, other: &Hash) -> Hash {
    let mut value = [0u8; Hash::SIZE * 2];
    value[..Hash::SIZE].copy_from_slice(&self.value);
    value[Hash::SIZE..].copy_from_slice(&other.value);
    Hash::from_bytes(j, &value)
  }

  pub fn to_str(&self) -> String {
    hex(&self.value)
  }
}

impl<const S: usize> From<&[u8; S]> for Hash {
  fn from(hash: &[u8; S]) -> Self {
    if S == Hash::SIZE {
      unsafe {
        let ptr = hash as *const [u8; S] as *const [u8; Hash::SIZE];
        Hash::new(*ptr)
      }
    } else {
      panic!();
    }
  }
}

impl Debug for Hash {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "Hash({})", hex(&self.value))
  }
}

/// ノード b_{i,j} を含むエントリがストレージ上のどこに位置するかを表します。
#[derive(PartialEq, Eq, Copy, Clone, Debug)]
pub struct Address {
  /// ハッシュ木のリスト構造上での位置。1 から開始し [`Index`] の最大値までの値を取ります。
  pub i: Index,
  /// このノードの高さ (最も遠い葉ノードまでの距離)。0 の場合、ノードが葉ノードであることを示しています。最大値は
  /// [`INDEX_SIZE`] です。
  pub j: u8,
  /// このノードが格納されているエントリのストレージ内の位置です。
  pub position: Position,
}

impl Address {
  pub fn new(i: Index, j: u8, position: Position) -> Address {
    Address { i, j, position }
  }
}

/// ハッシュ値を含む、ノード b_{i,j} の属性情報を表します。
#[derive(PartialEq, Eq, Copy, Clone, Debug)]
pub struct MetaInfo {
  pub address: Address,
  pub hash: Hash,
}

impl MetaInfo {
  pub fn new(address: Address, hash: Hash) -> MetaInfo {
    MetaInfo { address, hash }
  }
}

impl Display for MetaInfo {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.write_str(&format!("Node({},{}@{}){}", self.address.i, self.address.j, self.address.position, self.hash.to_str()))
  }
}

/// 左右の枝を持つ中間ノードを表します。
#[derive(PartialEq, Eq, Copy, Clone, Debug)]
pub struct INode {
  pub meta: MetaInfo,
  /// 左枝のノード
  pub left: Address,
  /// 右枝のノード
  pub right: Address,
}

impl INode {
  pub fn new(meta: MetaInfo, left: Address, right: Address) -> INode {
    INode { meta, left, right }
  }
}

/// 値を持つ葉ノードを表します。
#[derive(PartialEq, Eq, Debug, Clone)]
pub struct ENode {
  pub meta: MetaInfo,
  pub payload: Vec<u8>,
}

impl ENode {
  pub fn new(meta: MetaInfo, payload: Vec<u8>) -> Self {
    ENode { meta, payload }
  }
}

#[derive(PartialEq, Eq, Clone, Debug)]
pub struct Entry {
  enode: ENode,
  inodes: Vec<INode>,
}

impl Entry {
  pub fn new(enode: ENode, inodes: Vec<INode>) -> Self {
    debug_assert!(inodes.iter().map(|i| i.meta.address.j).collect::<Vec<_>>().windows(2).all(|w| w[0] < w[1]));
    Entry { enode, inodes }
  }

  pub fn root(&self) -> MetaInfo {
    if let Some(root) = self.inodes.last() { root.meta } else { self.enode.meta }
  }

  pub fn inode(&self, j: u8) -> Option<&INode> {
    self.inodes.iter().find(|inode| inode.meta.address.j == j)
  }

  pub fn meta(&self, j: u8) -> Option<&MetaInfo> {
    if j == 0 { Some(&self.enode.meta) } else { self.inode(j).map(|i| &i.meta) }
  }
}

impl Serializable for Entry {
  fn write<W: Write>(&self, w: &mut W) -> Result<usize> {
    debug_assert!(self.enode.payload.len() <= MAX_PAYLOAD_SIZE);
    debug_assert!(self.inodes.len() <= 0xFF);
    write_entry_to(self, w)
  }

  fn read<R: Read + Seek>(r: &mut R, position: Position) -> Result<Self> {
    read_entry_from(r, position)
  }
}

#[derive(Debug)]
enum CachedEntry<'a> {
  Cached(&'a Entry),
  Fresh(Entry),
}

impl<'a> CachedEntry<'a> {
  fn into_owned(self) -> Entry {
    match self {
      Self::Cached(entry) => entry.clone(),
      Self::Fresh(entry) => entry,
    }
  }
}

impl<'a> AsRef<Entry> for CachedEntry<'a> {
  fn as_ref(&self) -> &Entry {
    match self {
      Self::Cached(ref_data) => ref_data,
      Self::Fresh(owned_data) => owned_data,
    }
  }
}

// --------------------------------------------------------------------------

/// Constant for the maximum byte size of the payload (value), representing 2 GB (2,147,483,647 bytes).
///
/// To be the `offset` field to `u32` when serializing entries, the size of the serialized representation of the entry
/// must be limited to a maximum of [u32::MAX]. Therefore, the maximum payload length is limited to 2 GB. This constant
/// is also used as a bit mask, so it must be composed of a sequence of 1-bit values.
///
pub const MAX_PAYLOAD_SIZE: usize = 0x7FFFFFFF;

#[derive(Debug)]
pub struct Snapshot<'a, S: Storage<Entry>> {
  cache: Arc<Cache>,
  storage: &'a S,
}

impl<'a, S: Storage<Entry>> Snapshot<'a, S> {
  pub fn root(&self) -> MetaInfo {
    self.cache.entry(self.revision()).unwrap().root()
  }

  pub fn revision(&self) -> Index {
    self.cache.revision()
  }

  pub fn n(&self) -> Index {
    self.cache.revision()
  }

  pub fn query(&self) -> Result<Query<S>> {
    let cursor = self.storage.reader()?;
    let revision = self.cache.clone();
    Ok(Query { cursor, cache: revision })
  }
}

/// ストレージ上に直列化された Stratified Hash Tree を表す木構造に対する操作を実装します。
#[derive(Debug)]
pub struct Slate<S: Storage<Entry>> {
  position: Position,
  storage: S,
  cache: Arc<Cache>,
}

impl<S: Storage<Entry>> Slate<S> {
  /// 指定された [`Storage`] に直列化されたハッシュ木を保存する Slate を構築します。
  ///
  /// ストレージに [`std::path::Path`] や [`std::path::PathBuf`] のようなパスを指定したするとそのファイルに
  /// 直列化されたハッシュ木を保存します。テストや検証目的ではメモリ上にハッシュ木を直列化する [`MemStorage`] を
  /// 使用することができます。ストレージは抽象化されているため独自の [`Storage`] 実装を使用することができます。
  ///
  /// # Examples
  ///
  /// 以下はシステムのテンポラリディレクトリ上の `slate-example.db` にハッシュ木を直列化する例です。
  ///
  /// ```rust
  /// use slate::{BlockStorage,Slate,Storage,Result};
  /// use std::env::temp_dir;
  /// use std::fs::remove_file;
  /// use std::path::PathBuf;
  ///
  /// fn append_and_get(file: &PathBuf) -> Result<()>{
  ///   let storage = BlockStorage::from_file(file, false)?;
  ///   let mut db = Slate::new(storage)?;
  ///   let root = db.append(&[0u8, 1, 2, 3])?;
  ///   assert_eq!(Some(vec![0u8, 1, 2, 3]), db.snapshot().query()?.get(root.address.i)?.map(|x| x.clone()));
  ///   Ok(())
  /// }
  ///
  /// let mut path = temp_dir();
  /// path.push("slate-example.db");
  /// append_and_get(&path).expect("test failed");
  /// remove_file(path.as_path()).unwrap();
  /// ```
  pub fn new(storage: S) -> Result<Self> {
    Self::with_cache_level(storage, 0)
  }

  /// Build Slate with specified cache level.
  ///
  /// In cases where there is a lot of reading, setting the cache level to 1 or 2 may improve performance, but in cases
  /// where there is a lot of writing, overhead will increase.
  ///
  /// The number of entries cached increases as O(ℓ×2^ℓ), so it is not recommended to increase the level.
  ///
  pub fn with_cache_level(mut storage: S, level: usize) -> Result<Self> {
    let (cache, position) = Self::load_metadata(&mut storage, level)?;
    Ok(Slate { position, storage, cache: Arc::new(cache) })
  }

  /// 現在の木構造のルートノードを参照します。
  pub fn root(&self) -> Option<&MetaInfo> {
    self.cache.root()
  }

  /// 木構造の現在の世代 (リストとして何個の要素を保持しているか) を返します。
  pub fn n(&self) -> Index {
    self.cache.revision()
  }

  /// この Slate の現在の高さを参照します。ノードが一つも含まれていない場合は 0 を返します。
  pub fn height(&self) -> u8 {
    self.root().map(|root| root.address.j).unwrap_or(0)
  }

  pub fn cache(&self) -> &Cache {
    &self.cache
  }

  pub fn storage(&self) -> &S {
    &self.storage
  }

  /// 指定された値をこの Slate に追加します。
  ///
  /// # Returns
  /// この操作によって更新されたルートノードを返します。このルートノードは新しい木構造のルートハッシュである
  /// `hash` に加えて、ハッシュ木に含まれる要素数 `i`、ハッシュ木の高さ `j` を持ちます。
  ///
  pub fn append(&mut self, value: &[u8]) -> Result<MetaInfo> {
    if value.len() > MAX_PAYLOAD_SIZE {
      return Err(TooLargePayload { size: value.len() });
    }

    // 葉ノードの構築
    let position = self.position;
    let i = self.cache.root().map(|node| node.address.i + 1).unwrap_or(1);
    let hash = Hash::from_bytes(0, value);
    let meta = MetaInfo::new(Address::new(i, 0, position), hash);
    let enode = ENode { meta, payload: Vec::from(value) };

    // 中間ノードとその左枝にある PBST ルートの構築
    let mut inodes = Vec::<INode>::with_capacity(INDEX_SIZE as usize);
    let mut right_hash = enode.meta.hash;
    let model = Model::new(i);
    let right_to_left_inodes = model.inodes();
    for n in right_to_left_inodes.iter() {
      debug_assert_eq!(i, n.node.i);
      debug_assert_eq!(n.node.i, n.right.i);
      debug_assert!(n.node.j > n.right.j);
      debug_assert!(n.left.j >= n.right.j);

      // 左枝のノード (PBST ルート) を保存
      let left = self.cache.get_pbst_root(n.left.i, n.left.j);

      // 右枝のノードを保存
      let right = Address::new(n.right.i, n.right.j, position);
      let hash = left.hash.combine(n.node.j, &right_hash);
      let node = MetaInfo::new(Address::new(n.node.i, n.node.j, position), hash);
      let inode = INode::new(node, left.address, right);
      inodes.push(inode);
      right_hash = hash;
    }

    // 返値のための高さとルートハッシュを取得
    let (j, root_hash) =
      if let Some(inode) = inodes.last() { (inode.meta.address.j, inode.meta.hash) } else { (0u8, enode.meta.hash) };

    // エントリを書き込んで状態を更新
    let entry = Entry::new(enode, inodes);
    self.position = self.storage.put(self.position, &entry)?;

    // キャッシュを更新
    let cache = self.cache.migrate(entry, model);
    self.cache = Arc::new(cache);

    Ok(MetaInfo::new(Address::new(i, j, position), root_hash))
  }

  /// Delete entries starting from the specified index `i`.
  /// If queries are currently in progress, the truncate operations may cause errors in their read processing. If these
  /// must be performed concurrently, ensure proper Read/Write locks are established.
  pub fn truncate(&mut self, i: Index) -> Result<bool> {
    if let Some(entry) = self.snapshot().query()?.read_entry(i)? {
      self.storage.truncate(entry.enode.meta.address.position)?;
      let (cache, position) = Self::load_metadata(&mut self.storage, self.cache.level())?;
      debug_assert_eq!(entry.enode.meta.address.position, position);
      self.position = position;
      self.cache = Arc::new(cache);
      Ok(true)
    } else {
      Ok(false)
    }
  }

  /// Create a snapshot for the current revision.
  pub fn snapshot(&self) -> Snapshot<'_, S> {
    Snapshot { cache: self.cache.clone(), storage: &self.storage }
  }

  /// Create a snapshot for the specified revision.
  pub fn snapshot_at(&mut self, revision: Index) -> Result<Snapshot<'_, S>> {
    let entry = self.load_entry(revision)?.into_owned();
    let cache = Cache::load(&mut self.storage, self.cache.level(), entry)?;
    Ok(Snapshot { cache: Arc::new(cache), storage: &self.storage })
  }

  fn load_metadata(storage: &mut S, level: usize) -> Result<(Cache, Position)> {
    let (latest_entry, position) = storage.last()?;
    let cache = match latest_entry {
      Some(entry) => Cache::load(storage, level, entry)?,
      None => Cache::empty(level),
    };
    Ok((cache, position))
  }

  fn load_entry(&self, i: Index) -> Result<CachedEntry<'_>> {
    if i == 0 || i > self.n() {
      return Err(Error::InvalidArgument(format!("Entry {} is not contained in the current revision {}", i, self.n())));
    }

    // the process returns early if the corresponding entry is cached
    if let Some(entry) = self.cache.entry(i) {
      return Ok(CachedEntry::Cached(entry));
    }

    // search for the corresponding entry
    let mut entry = CachedEntry::Cached(self.cache.entry(self.n()).unwrap());
    let mut reader = self.storage.reader()?;
    while entry.as_ref().enode.meta.address.i != i {
      match entry.as_ref().inodes.iter().find(|inode| contains(inode.left.i, inode.left.j, i)) {
        Some(inode) => {
          entry = if let Some(e) = self.cache.entry(inode.left.i) {
            CachedEntry::Cached(e)
          } else {
            CachedEntry::Fresh(read_entry_with_index_check::<S>(&mut reader, inode.left.position, inode.left.i)?)
          };
        }
        None => {
          return Err(Error::InternalStateInconsistency {
            message: format!("The entry doesn't have a path to {}: {:?}", i, entry.as_ref()),
          });
        }
      }
    }

    Ok(entry)
  }
}

impl Slate<BlockStorage<MemoryDevice>> {
  pub fn new_on_memory() -> Self {
    let storage = BlockStorage::memory();
    Self::new(storage).expect("Memory storage initialization should never fail")
  }
  pub fn new_on_memory_with_capacity(capacity: usize) -> Self {
    let buffer = Arc::new(RwLock::new(Vec::with_capacity(capacity)));
    Self::new_on_memory_with_buffer(buffer)
  }
  pub fn new_on_memory_with_buffer(buffer: Arc<RwLock<Vec<u8>>>) -> Self {
    let device = MemoryDevice::with(buffer, false);
    let storage = BlockStorage::new(device);
    Self::new(storage).expect("Memory storage initialization should never fail")
  }
}

impl Slate<MemKVS<Entry>> {
  pub fn new_on_memkvs() -> Self {
    Self::new(MemKVS::new()).expect("Memory storage initialization should never fail")
  }
  pub fn new_on_memkvs_with_hashmap(kvs: Arc<RwLock<HashMap<Position, Entry>>>) -> Self {
    Self::new(MemKVS::with_kvs(kvs)).expect("Memory storage initialization should never fail")
  }
}

impl Slate<BlockStorage<FileDevice>> {
  pub fn new_on_file<P: AsRef<Path>>(path: P, read_only: bool) -> Result<Self> {
    let storage = BlockStorage::from_file(path, read_only)?;
    Self::new(storage)
  }
}

#[cfg(feature = "rocksdb")]
impl Slate<crate::storage::rocksdb::RocksDBStorage> {
  pub fn new_on_rocksdb<P: AsRef<Path>>(path: P, read_only: bool) -> Result<Self> {
    use ::rocksdb::{DB, Options};
    let db = if read_only {
      let mut opts = Options::default();
      opts.create_if_missing(true);
      DB::open_for_read_only(&opts, path, true)?
    } else {
      DB::open_default(path)?
    };
    let storage = rocksdb::RocksDBStorage::new(Arc::new(RwLock::new(db)), &[], false);
    Slate::new(storage)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkDirection {
  Left,
  Right,
  Both,
  Terminate,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Prove {
  Identical,
  Divergent(Vec<(Index, u8)>),
}

/// The stratum sample representing an authentication path to a specific leaf in a specific revision.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct StratumSample {
  pub revision: Index,
  pub leaf: Value,
  pub witnesses: HashMap<(Index, u8), Hash>,
}

impl StratumSample {
  fn new(revision: Index, leaf: Value, witnesses: HashMap<(Index, u8), Hash>) -> Self {
    StratumSample { revision, leaf, witnesses }
  }

  /// Calculates the root hash from this sample and returns it.
  /// This returns an error if the stratum sample structure is invalid.
  pub fn root_hash(&self) -> Result<Hash> {
    let (i, j) = root_of(self.revision);
    self.compute_hash(i, j)
  }

  fn compute_hash(&self, i: Index, j: u8) -> Result<Hash> {
    if j == 0 {
      if self.leaf.i == i {
        Ok(self.leaf.hash())
      } else {
        Err(Error::SampleVerificationFailed(format!(
          "The sample structure is incorrect: This path is for b_{}, but b_{} is stored",
          i, self.leaf.i
        )))
      }
    } else {
      let ((il, jl), (ir, jr)) = subnodes_of(i, j);
      if contains(il, jl, self.leaf.i) {
        let left = self.compute_hash(il, jl)?;
        let right = self.witnesses.get(&(ir, jr)).unwrap();
        Ok(left.combine(j, right))
      } else if contains(ir, jr, self.leaf.i) {
        let left = self.witnesses.get(&(il, jl)).unwrap();
        let right = self.compute_hash(ir, jr)?;
        Ok(left.combine(j, &right))
      } else {
        Err(Error::SampleVerificationFailed(format!(
          "The stratum sample structure is incorrect: The leaf node b_{} is not contained",
          self.leaf.i
        )))
      }
    }
  }

  /// Compare the two stratum samples and confirm that the Merkle trees that generated them are the same.
  /// If they don't match, collect the positions of intermediate or leaf node containing the mismatched leaves and
  /// return [Prove::Divergent].
  ///
  /// This feature can be used to narrow down and identify the differing leaves between two Merkle trees as follows:
  ///
  /// ```
  /// use slate::{Slate, Prove};
  /// use slate::formula::contains;
  ///
  /// // Craete two data sequences with different values for i=199.
  /// let mut server = Slate::new_on_memory();
  /// let mut client = Slate::new_on_memory();
  /// for i in 1u32..=256 {
  ///   server.append(&i.to_le_bytes()).unwrap();
  ///   client.append(&(if i != 199 { i.to_le_bytes() } else { i.to_be_bytes() })).unwrap();
  /// }
  /// assert_ne!(client.root(), server.root());
  ///
  /// let revision = client.n();
  /// let mut i = revision;       // the initial value can be any available index
  /// let mut interactions = 0;
  /// let proved_index = loop {
  ///   // First, the client creates a stratum sample to the index containing the difference.
  ///   let snapshot = client.snapshot_at(revision).unwrap();
  ///   let mut query = snapshot.query().unwrap();
  ///   let sample = query.get_sample(i).unwrap().unwrap();
  ///   // Here, the client sends the stratum sample to the server.
  ///   interactions += 1;
  ///
  ///   // Next, the server identifies nodes containing differences from the received sample.
  ///   let snapshot = server.snapshot_at(sample.revision).unwrap();
  ///   let mut query = snapshot.query().unwrap();
  ///   let server_sample = query.get_sample(sample.leaf.i).unwrap().unwrap();
  ///   match server_sample.compare(&sample).unwrap() {
  ///     Prove::Divergent(diffs) => {
  ///       // NOTE: In this example, only one node detected because there is only one difference in leaves, but if
  ///       // there are differences in multiple leaves, there will be multiple nodes.
  ///       assert_eq!(1, diffs.len());
  ///       let (di, dj) = diffs[0];
  ///       assert!(contains(di, dj, 199));   // the detected nodes should contain the differences
  ///       if dj == 0 {                      // found the leaf with a difference
  ///         break di;
  ///       }
  ///       i = di;
  ///     }
  ///     Prove::Identical => panic!(),
  ///   }
  ///   // Here, the server responds to the client with the nodes that detected the difference.
  /// };
  /// println!("a difference detected after {interactions} interactions."); // 4 interactions
  /// assert_eq!(199, proved_index);
  /// ```
  pub fn compare(&self, other: &StratumSample) -> Result<Prove> {
    if self.revision != other.revision {
      return Err(Error::SampleVerificationFailed(format!(
        "The revisions being compared are different: {} != {}",
        self.revision, other.revision
      )));
    } else if self.leaf.i != other.leaf.i {
      return Err(Error::SampleVerificationFailed(format!(
        "The different stratum sample between {} and {} in revisions {} cannot be compared",
        self.leaf.i, other.leaf.i, self.revision
      )));
    }

    // Note that the structure of both stratum samples is verified to be correct by calculating the root hash.
    if self.root_hash()? == other.root_hash()? {
      return Ok(Prove::Identical);
    }

    let mut divergents = Vec::with_capacity(self.witnesses.len());
    for ((i, j), h1) in self.witnesses.iter() {
      if let Some(h2) = other.witnesses.get(&(*i, *j)) {
        if h1 != h2 {
          divergents.push((*i, *j));
        }
      } else {
        panic!("{self:?} != {other:?}");
      }
    }
    if self.leaf.hash() != other.leaf.hash() {
      divergents.push((self.leaf.i, 0));
    }
    divergents.sort();
    divergents.reverse();
    Ok(Prove::Divergent(divergents))
  }
}

pub struct Query<S: Storage<Entry>> {
  cursor: S::Reader,
  cache: Arc<Cache>,
}

impl<S: Storage<Entry>> Query<S> {
  /// Refers to the revison (equivalent to a generation or snapshot) of the tree structure that this query is targeting.
  pub fn revision(&self) -> Index {
    self.cache.revision()
  }

  /// Traverse from the root to the leaves using the specified direction indicator for this query snapshot.
  /// The direction indicator can capture values during the movement process.
  ///
  /// # Examples
  ///
  /// ```
  /// use slate::{BlockStorage, Slate, WalkDirection};
  ///
  /// let storage = BlockStorage::memory();
  /// let mut db = Slate::new(storage).unwrap();
  /// db.append(&[0u8]).unwrap();
  /// db.append(&[1u8]).unwrap();
  /// db.append(&[2u8]).unwrap();
  /// db.append(&[3u8]).unwrap();
  /// db.append(&[4u8]).unwrap();
  /// let snapshot = db.snapshot();
  /// let mut query = snapshot.query().unwrap();
  ///
  /// // Always move to the left from the root towards the leaves.
  /// // 5: (5, 3) None
  /// // 5: (4, 2) None
  /// // 5: (2, 1) None
  /// // 5: (1, 0) Some([0])
  /// let mut leaf = None;
  /// query.walk_down(&mut |revision, i, j, hash, value: Option<&[u8]>| {
  ///   println!("{revision}: ({i}, {j}) {value:?}");
  ///   if j == 0 {
  ///     leaf = value.map(|a| a.to_vec());
  ///     WalkDirection::Terminate
  ///   } else {
  ///     WalkDirection::Left
  ///   }
  /// }).unwrap();
  /// assert_eq!(vec![0u8], leaf.unwrap());
  /// ```
  pub fn walk_down<F>(&mut self, callback: &mut F) -> Result<()>
  where
    F: FnMut(Index, Index, u8, &Hash, Option<&[u8]>) -> WalkDirection,
  {
    let cache = self.cache.clone();
    if let Some(last_entry) = cache.last_entry() {
      let node_index = if last_entry.inodes.is_empty() { 0 } else { last_entry.inodes.len() };
      self._walk_down(node_index, last_entry, callback)
    } else {
      Ok(())
    }
  }

  fn _walk_down<F>(&mut self, node_index: usize, entry: &Entry, callback: &mut F) -> Result<()>
  where
    F: FnMut(Index, Index, u8, &Hash, Option<&[u8]>) -> WalkDirection,
  {
    let i = entry.enode.meta.address.i;
    let (j, hash, value) = if node_index == 0 {
      let node = &entry.enode;
      (node.meta.address.j, &node.meta.hash, Some(&node.payload))
    } else {
      let node = &entry.inodes[node_index - 1];
      (node.meta.address.j, &node.meta.hash, None)
    };

    let dir = (callback)(self.revision(), i, j, hash, value.map(|v| &**v));

    // terminate when a leaf node visited or a stop requested
    if node_index == 0 || dir == WalkDirection::Terminate {
      return Ok(());
    }

    if dir == WalkDirection::Right || dir == WalkDirection::Both {
      self._walk_down(node_index - 1, entry, callback)?
    }
    if dir == WalkDirection::Left || dir == WalkDirection::Both {
      let current = &entry.inodes[node_index - 1];
      let left = if let Some(left_entry) = self.cache.entry(current.left.i) {
        // TODO Is there a way to use cached entries without cloning them?
        left_entry.clone()
      } else {
        read_entry_with_index_check::<S>(&mut self.cursor, current.left.position, current.left.i)?
      };
      debug_assert_eq!(current.left.i, left.enode.meta.address.i);
      debug_assert!(left.enode.meta.address.i < i);
      let left_j = current.left.j;
      let left_node_index = if current.left.j == 0 {
        0
      } else {
        match left.inodes.iter().position(|inode| inode.meta.address.j == left_j) {
          Some(i) => i + 1,
          None => inconsistency(format!(
            "The node b_{{{},{}}} referenced as the right node of node b_{{{},{}}} isn't included in entry {} @ {}",
            current.right.i,
            current.right.j,
            current.meta.address.i,
            current.meta.address.j,
            current.right.i,
            current.right.position
          ))?,
        }
      };
      self._walk_down(left_node_index, &left, callback)?
    }
    Ok(())
  }

  /// 指定されたインデックスの値を参照します。
  /// 0 を含む範囲外のインデックスを指定した場合は `None` を返します。
  pub fn get(&mut self, i: Index) -> Result<Option<Vec<u8>>> {
    if i == 0 || i > self.cache.revision() {
      return Ok(None);
    }
    let mut result = Ok(None);
    self.walk_down(&mut |_n, b_i, b_j, _hash, leaf_value: Option<&[u8]>| {
      if b_i == i {
        if b_j == 0 {
          if let Some(v) = leaf_value {
            result = Ok(Some(v.to_vec()));
          } else {
            result = inconsistency(format!("The value was not passed at the leaf node b_{b_i} reached"))
          }
          WalkDirection::Terminate
        } else {
          WalkDirection::Right
        }
      } else {
        let ((_il, _jl), (ir, jr)) = subnodes_of(b_i, b_j);
        if contains(ir, jr, i) { WalkDirection::Right } else { WalkDirection::Left }
      }
    })?;
    result
  }

  // 指定されたデータ b_i とその層サンプルを取得します。
  pub fn get_sample(&mut self, i: Index) -> Result<Option<StratumSample>> {
    let mut witnesses = HashMap::new();
    let mut leaf_data = None;
    self.walk_down(&mut |_n, bi, bj, hash, value| {
      if !contains(bi, bj, i) {
        witnesses.insert((bi, bj), *hash);
        WalkDirection::Terminate
      } else if bj == 0 {
        leaf_data = value.map(|x| x.to_vec());
        WalkDirection::Terminate
      } else {
        WalkDirection::Both
      }
    })?;
    Ok(leaf_data.map(|data| {
      let revision = self.revision();
      let leaf = Value { i, value: data };
      StratumSample::new(revision, leaf, witnesses)
    }))
  }

  // 指定された範囲 [i0, i1] (両端を含む) のエントリを取得します。
  pub fn scan<E, F>(&mut self, i0: Index, i1: Index, callback: &mut F) -> Result<u64>
  where
    E: std::error::Error + Send + Sync + 'static,
    F: FnMut(Entry) -> std::result::Result<(), E>,
  {
    let n = self.cache.revision();
    let (i0, i1) = if i0 <= i1 { (i0, i1) } else { (i1, i0) };
    let (i0, i1) = (i0.max(1), i1.min(n));
    if i0 > i1 {
      return Ok(0);
    }

    debug_assert!(1 <= i0 && i0 <= n && 1 <= i1 && i1 <= n && i0 <= i1);
    if let Some(entry) = { self.read_entry(i0)? } {
      let position = entry.enode.meta.address.position;
      let count = i1 - i0 + 1;
      self.cursor.scan(position, count, callback)
    } else {
      Ok(0)
    }
  }

  /// 指定されたインデックスのエントリを読み出します。
  pub fn read_entry(&mut self, i: Index) -> Result<Option<Entry>> {
    fn _read_entry<S>(cursor: &mut S::Reader, cache: &Cache, entry: Entry, i: Index) -> Result<Option<Entry>>
    where
      S: Storage<Entry>,
    {
      if entry.enode.meta.address.i == i {
        return Ok(Some(entry));
      }
      for inode in entry.inodes.iter() {
        if contains(inode.left.i, inode.left.j, i) {
          let entry = if let Some(entry) = cache.entry(inode.left.i) {
            entry.clone()
          } else {
            read_entry_with_index_check::<S>(cursor, inode.left.position, inode.left.i)?
          };
          return _read_entry::<S>(cursor, cache, entry, i);
        }
      }
      Ok(None)
    }
    if let Some(entry) = self.cache.entry(self.revision()).cloned() {
      _read_entry::<S>(&mut self.cursor, &self.cache, entry, i)
    } else {
      Ok(None)
    }
  }
}

pub(crate) fn read_entry_with_index_check<S>(
  r: &mut S::Reader,
  position: Position,
  expected_index: Index,
) -> Result<Entry>
where
  S: Storage<Entry>,
{
  let entry = r.read(position)?;
  let actual_index = entry.enode.meta.address.i;
  if actual_index != expected_index {
    Err(InternalStateInconsistency {
      message: format!("The index of entry {expected_index} read from {position} was actually {actual_index}"),
    })
  } else {
    Ok(entry)
  }
}

/// panic_over_inconsistency が定義されている場合は panic して内部矛盾を検出した場所を知らせる。
fn inconsistency<T>(msg: String) -> Result<T> {
  #[cfg(feature = "panic_over_inconsistency")]
  {
    panic!("{}", msg)
  }
  #[cfg(not(feature = "panic_over_inconsistency"))]
  {
    Err(InternalStateInconsistency { message: msg })
  }
}

#[inline]
fn hex(value: &[u8]) -> String {
  value.iter().map(|c| format!("{c:02X}")).collect()
}

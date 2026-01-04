//! This module extracts mathematical formulas used in Slate.
//!
use std::fmt::Debug;
use std::ops::RangeInclusive;

#[cfg(test)]
mod test;

/// Slate がインデックス i として使用する整数の型です。
pub type Index = u64;

/// [`Index`] 型のビット幅です。定数 64 を表しています。
pub const INDEX_SIZE: u8 = 64;

/// Slate のアルゴリズムで使用する任意のノード b_{i,j} を表すための構造体です。
///
#[derive(Eq, PartialEq, Copy, Clone, Debug)]
pub struct Addr {
  pub i: Index,
  pub j: u8,
}

impl Addr {
  pub fn new(i: Index, j: u8) -> Addr {
    Addr { i, j }
  }
}

/// Slate のアルゴリズムで使用する任意の中間ノードを表すための構造体です。左右の枝への分岐を含みます。
///
#[derive(Eq, PartialEq, Copy, Clone, Debug)]
pub struct INode {
  pub node: Addr,
  pub left: Addr,
  pub right: Addr,
}

impl INode {
  pub fn new(node: Addr, left: Addr, right: Addr) -> INode {
    INode { node, left, right }
  }
}

/// n≧1 世代のハッシュ木構造 𝑇ₙ をアルゴリズムによって表す概念モデルです。n 世代木における中間ノードや探索経路などの
/// アルゴリズムを実装します。
///
#[derive(PartialEq, Eq, Clone, Debug)]
pub struct Model {
  n: Index,
  pbst_roots: Vec<Addr>,
  ephemeral_nodes: Vec<INode>,
}

impl Model {
  /// 木構造 𝑇ₙ に含まれる独立した完全二分木のルートノードとそれらを接続する中間ノードを算出します。この列は木構造の
  /// 左に存在する完全二分木が先に来るように配置されています。
  /// 時間/空間計算量は O(log n) です。
  pub fn new(n: Index) -> Self {
    debug_assert_ne!(0, n);
    let pbst_roots = pbst_roots(n).collect::<Vec<_>>();
    debug_assert_ne!(0, pbst_roots.len());
    let ephemeral_nodes = Self::create_ephemeral_nodes(n, &pbst_roots);
    Self { n, pbst_roots, ephemeral_nodes }
  }

  /// このハッシュ木の世代を参照します。
  pub fn n(&self) -> Index {
    self.n
  }

  /// このハッシュ木のルートノードを参照します。
  pub fn root(&self) -> Addr {
    self.ephemeral_nodes.first().map(|i| i.node).unwrap_or(*self.pbst_roots.first().unwrap())
  }

  /// 独立した完全二分木のルートノードを列挙します。
  pub fn pbst_roots(&self) -> impl Iterator<Item = &Addr> {
    self.pbst_roots.iter()
  }

  /// 一過性の中間ノードを列挙します。
  pub fn ephemeral_nodes(&self) -> impl Iterator<Item = &INode> {
    self.ephemeral_nodes.iter()
  }

  /// この世代で追加される中間ノードを列挙します。
  /// 返値のリストは葉ノードからの順番 (つまり j の小さい順) になります。
  pub fn inodes(&self) -> Vec<INode> {
    let mut inodes = Vec::<INode>::with_capacity(ceil_log2(self.n) as usize);
    for inode in self.ephemeral_nodes() {
      inodes.push(*inode);
    }
    if let Some(node) = self.pbst_roots().find(|node| node.i == self.n() && node.j != 0) {
      let i = node.i;
      for j in 0..node.j {
        let j = node.j - j;
        let left = Addr::new(i - (1 << j) + (1 << (j - 1)), j - 1);
        let right = Addr::new(i, j - 1);
        inodes.push(INode::new(Addr::new(i, j), left, right))
      }
    }
    inodes.reverse();
    inodes
  }

  /// 指定された中間ノード b_{i,j} を返します。該当する中間ノードが存在しない場合は `None` を返します。
  pub fn inode(&self, i: Index, j: u8) -> Option<INode> {
    if j == 0 {
      None
    } else if is_pbst(i, j) && i < self.n() {
      Some(Self::pbst_inode(i, j))
    } else {
      self.ephemeral_nodes().find(|node| node.node.i == i && node.node.j == j).copied()
    }
  }

  #[inline]
  fn pbst_inode(i: Index, j: u8) -> INode {
    debug_assert!(is_pbst(i, j), "({i}, {j}) is not a PBST");
    debug_assert_ne!(0, j, "({i}, {j}) is a leaf node, not a inode");
    INode::new(Addr::new(i, j), Addr::new(i - (1 << (j - 1)), j - 1), Addr::new(i, j - 1))
  }

  /// 一過性の中間ノードを参照します。
  fn create_ephemeral_nodes(n: Index, pbsts: &[Addr]) -> Vec<INode> {
    debug_assert_ne!(0, pbsts.len());
    let mut ephemerals = Vec::<INode>::with_capacity(pbsts.len() - 1);
    for i in 0..pbsts.len() - 1 {
      let position = pbsts.len() - 1 - i;
      ephemerals.insert(
        0,
        INode {
          node: Addr::new(n, pbsts[position - 1].j + 1),
          left: pbsts[position - 1],
          right: if i != 0 { ephemerals[0].node } else { pbsts[position] },
        },
      );
    }
    ephemerals
  }
}

/// 指定されたノード b_{i,j} をルートとする部分木が完全二分木であるかを判定します。
/// true の場合、 b_{i,j} が永続的ノード (以後の世代でも継続して存在するノード) であることも意味します。
/// すべての葉ノードに対しても true を返します。
#[inline]
pub const fn is_pbst(i: Index, j: u8) -> bool {
  i_mod_2e(i, j) == 0
}

/// n 個の要素を持つ木構造 𝑇ₙ において、完全二分木がいくつ存在するかを返します。
/// これは、額面が 2⁰, 2¹, 2², ..., 2^h のコインの合計を、枚数を最小化してぴったり
///  n にする最小コイン問題問題の一種と考えられます。
#[inline]
pub const fn count_pbsts(n: Index) -> u8 {
  n.count_ones() as u8
}

/// 木構造 𝑇ₙ に含まれる独立した完全二分木のルートノードを列挙します。
pub fn pbst_roots(n: Index) -> impl Iterator<Item = Addr> {
  let mut remaining = n;
  std::iter::from_fn(move || {
    if remaining == 0 {
      return None;
    }
    let j = floor_log2(remaining);
    let size = pow2e(j);
    let i = n - remaining + size;
    let root = Addr::new(i, j);
    remaining -= size;
    Some(root)
  })
}

/// Calculates the total number of nodes in the n-th generation.
/// The return value includes the number of leaf nodes.
/// 𝑓:0 → 0, 𝑓:(n) → 2×n-1, 𝑓:(2⁶⁴-1) → (2⁶⁴-1)×2-1
#[inline]
pub const fn total_nodes(n: Index) -> u128 {
  if n == 0 { 0 } else { 2 * n as u128 - 1 }
}

/// 任意のノード b_{i,j} をルートとする部分木に含まれる葉ノード b_ℓ の範囲を O(1) で算出します。
#[inline]
pub const fn range(i: Index, j: u8) -> RangeInclusive<Index> {
  debug_assert!(j <= 64); // i=u64::MAX のとき j=64
  // let i_min = (((i as u128 >> j) - (if is_pbst(i, j) { 1 } else { 0 })) << j) as u64 + 1;
  let i_min = i - i_mod_2e(i - 1, j);
  let i_max = i;
  i_min..=i_max
}

/// Calculates the root index b_{i,j} of any generation n in O(1).
#[inline]
pub const fn root_of(n: Index) -> (Index, u8) {
  debug_assert!(n > 0);
  let i = n;
  let j = ceil_log2(n);
  (i, j)
}

/// Calculates the indices of node b_{il,jl} and b_{ir,jr} connected to the left and right child nodes of any node
/// b_{i,j} in O(1).
#[inline]
pub fn subnodes_of(i: Index, j: u8) -> ((Index, u8), (Index, u8)) {
  debug_assert!(is_valid(i, j), "({i}, {j})");
  let left_upper = if j < INDEX_SIZE { (i - 1) >> j << j } else { 0 };
  let left_lower = pow2e(j - 1);
  let left = (left_upper | left_lower, j - 1);
  let right = (i, ceil_log2(i - left.0));
  (left, right)
}

/// Determines whether a node b_{i,j} can exist in O(1).
#[inline]
pub const fn is_valid(i: Index, j: u8) -> bool {
  if i == 0 {
    // 0-th generation doesn't exist
    false
  } else if j == 0 {
    // j = 0, leaf node, is always valid
    true
  } else {
    let height = ceil_log2(i);
    if j > height {
      // j exceeds the calculated height limit
      false
    } else if j == height {
      // j = height, root of that generation, is always valid
      true
    } else if is_pbst(i, j) {
      // b'_{i,j} always valid
      true
    } else {
      // 一過性ノードの条件
      // i mod 2^j > 2^(j-1) の場合、高さjの一過性ノードが必要
      let remainder = i & ((1u64 << j) - 1);
      remainder > (1u64 << (j - 1))
    }
  }
}

/// 任意のノード b_{i,j} をルートとする部分木 T_{i,j} に含まれている葉ノードの数 r_{i,j} を算出します。
#[inline]
pub const fn leaf_count(i: Index, j: u8) -> Index {
  i_mod_2e(i - 1, j) + 1
}

/// 指定されたノード b_{i,j} をルートとする部分木にノード b_{k,\*} が含まれているかを判定します。これは T_{k,*} が
/// T_{i,j} の部分木かどうかを判定することと意味的に同じです。
#[inline]
pub fn contains(i: Index, j: u8, k: Index) -> bool {
  debug_assert!(j <= 64); // i=u64::MAX のとき j=64
  range(i, j).contains(&k)
}

/// 層サンプルの大きさを算出します。
/// この呼び出しは最悪ケースで O(log n) の計算量です。
#[inline]
pub fn stratum_sample_size(n: Index, i: Index) -> u8 {
  fn _branch_count(ti: Index, i: Index, j: u8, count: u8) -> u8 {
    if j == 0 {
      count
    } else {
      let ((il, jl), (ir, jr)) = subnodes_of(i, j);
      if contains(il, jl, ti) {
        _branch_count(ti, il, jl, count + 1)
      } else {
        debug_assert!(contains(ir, jr, ti));
        _branch_count(ti, ir, jr, count + 1)
      }
    }
  }
  let (ri, rj) = root_of(n);
  if contains(ri, rj, i) { _branch_count(i, ri, rj, 0) } else { 0 }
}

/// n 個の要素を持つ木構造 𝑇ₙ において、最新のエントリ eₙ から指定されたエントリ eᵢ までの距離を計算します。
/// Slate においては 1 エントリごとに 1 回の I/O Read が発生するため、この関数の返値は n 世代において eᵢ にアクセスするための
/// コストと見なすことができます。これは非線形ですが、n-1 と i-1 との 2 進数表現でのハミング距離と似た分布の特徴を持ちます。
pub fn entry_access_distance(k: Index, n: Index) -> Option<u8> {
  pbst_roots(n)
    .find(|root| contains(root.i, root.j, k))
    .map(|root| root.j - (k - range(root.i, root.j).start()).count_ones() as u8 + (root.i != n) as u8)
}

/// n 個の要素を持つ木構造 𝑇ₙ において、最新のエントリ eₙ からデータ列の任意の位置までの距離の上限と下限の範囲を
/// (upper-limit-range, lower-limit-range) で返します。つまり、各配列のインデックスは最新エントリからの距離、
/// 値はそのインデックス範囲を表します。
///
/// ## Example
///
/// ```
/// let (ul, ll) = slate::formula::entry_access_distance_limits(12);
/// for (distance, range) in [(12, 12), (10, 11), (6, 9), (2, 5), (1, 1)].iter().enumerate() {
///   assert_eq!(*range, (*ul[distance].start(), *ul[distance].end()));
/// }
/// for (distance, range) in [(12, 12), (8, 11), (4, 7), (2, 3), (1, 1)].iter().enumerate() {
///   assert_eq!(*range, (*ll[distance].start(), *ll[distance].end()));
/// }
/// ```
pub fn entry_access_distance_limits(n: Index) -> (Vec<RangeInclusive<Index>>, Vec<RangeInclusive<Index>>) {
  if n == 0 {
    return (Vec::new(), Vec::new());
  }
  let h = ceil_log2(n);
  let roots = pbst_roots(n).collect::<Vec<_>>();

  let ds_ll = vec![RangeInclusive::new(n, n)]
    .into_iter()
    .chain(
      roots
        .first()
        .map(|root| {
          let h = root.j;
          let mut ds_ll = (0..=h).map(|d| pow2e(h - d)).collect::<Vec<_>>();
          if roots.len() > 1 {
            ds_ll.insert(0, n);
          }
          ds_ll
        })
        .unwrap_or(Vec::new())
        .windows(2)
        .map(|w| RangeInclusive::new(w[1], w[0] - 1)),
    )
    .collect::<Vec<_>>();

  let ds_ul = roots
    .iter()
    .rev()
    .enumerate()
    .map(|(x, root)| {
      let right_most_pbst = x == 0;
      let h = root.j;
      let n = pow2e(h);
      let offset = *range(root.i, root.j).start() - 1;
      let mut ds_ul = Vec::with_capacity(h as usize);
      if !right_most_pbst {
        ds_ul.push(0);
      }
      for d in 0..=h {
        ds_ul.push(offset + (n + 1 - pow2e(d))); // Be aware of overflow due to order of operations
      }
      ds_ul
    })
    .fold(vec![0; h as usize + 1], |mut acc, ul| {
      for d in 0..ul.len() {
        if acc[d] == 0 {
          acc[d] = ul[d];
        }
      }
      acc
    })
    .windows(2)
    .map(|w| RangeInclusive::new(w[1] + 1, w[0]))
    .chain(vec![RangeInclusive::new(1, 1)])
    .collect::<Vec<_>>();
  (ds_ul, ds_ll)
}

/// i mod 2^j を算出します。j∈{0,…,64} の値を指定することができます。
///
/// ```
/// use slate::formula::i_mod_2e;
/// assert_eq!(0, i_mod_2e(u64::MAX, 0));
/// assert_eq!(1, i_mod_2e(u64::MAX, 1));
/// assert_eq!(3, i_mod_2e(u64::MAX, 2));
/// assert_eq!(7, i_mod_2e(u64::MAX, 3));
/// assert_eq!(u64::MAX, i_mod_2e(u64::MAX, 64));
/// ```
#[inline]
pub const fn i_mod_2e(i: Index, j: u8) -> Index {
  debug_assert!(j <= 64);
  // Typically, j < 64, so branch prediction is effective. If we use 128-bit arithmetic and remove conditional branch,
  // the CPU breaks it down into multiple instructions, which incures casting overhead and is slower than conditional
  // branches.
  if j >= 64 { i } else { i & ((1u64 << j) - 1) }
}

/// 2^j を算出します。j∈{0,…,63} の値を指定することができます。
///
/// ```
/// use slate::formula::pow2e;
/// assert_eq!(1, pow2e(0));
/// assert_eq!(2, pow2e(1));
/// assert_eq!(4, pow2e(2));
/// assert_eq!(8, pow2e(3));
/// assert_eq!(u64::MAX / 2 + 1, pow2e(63));
/// ```
#[inline]
pub const fn pow2e(j: u8) -> Index {
  debug_assert!(j < 64);
  1u64 << j
}

/// 指定された 𝑥 に対して 𝑦=⌈log₂ 𝑥⌉ を求めます。返値は 0 (x=1) から 64 (x=u64::MAX) の範囲となります。
/// 𝑥 に 0 を指定することはできません。
///
/// ```
/// use slate::formula::ceil_log2;
/// assert_eq!(0, ceil_log2(1));
/// assert_eq!(1, ceil_log2(2));
/// assert_eq!(2, ceil_log2(3));
/// assert_eq!(8, ceil_log2(255));
/// assert_eq!(8, ceil_log2(256));
/// assert_eq!(9, ceil_log2(257));
/// assert_eq!(64, ceil_log2(u64::MAX));
/// ```
#[inline]
pub const fn ceil_log2(x: Index) -> u8 {
  debug_assert!(x > 0);
  (INDEX_SIZE as u32 - (x - 1).leading_zeros()) as u8
}

/// 指定された 𝑥 に対して 𝑦=⌊log₂ 𝑥⌋ を求めます。返値は 0 (x=1) から 63 (x=u64::MAX) の範囲となります。
/// 𝑥 に 0 を指定することはできません。
///
/// ```
/// use slate::formula::floor_log2;
/// assert_eq!(0, floor_log2(1));
/// assert_eq!(1, floor_log2(2));
/// assert_eq!(1, floor_log2(3));
/// assert_eq!(7, floor_log2(255));
/// assert_eq!(8, floor_log2(256));
/// assert_eq!(63, floor_log2(u64::MAX));
/// ```
#[inline]
pub const fn floor_log2(x: Index) -> u8 {
  debug_assert!(x > 0);
  x.ilog2() as u8
}

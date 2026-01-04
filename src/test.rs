use std::cmp::{max, min};
use std::collections::HashSet;
use std::env::temp_dir as temprary_directory;
use std::fs::{OpenOptions, create_dir_all};
use std::io::ErrorKind;
use std::path::{MAIN_SEPARATOR, PathBuf};
use std::sync::RwLock;
use std::thread::{JoinHandle, spawn};
use std::time::Instant;

use crate::formula::{ceil_log2, stratum_sample_size, total_nodes};
use crate::memory::MemoryDevice;
use crate::*;

#[test]
fn test_multi_threaded_query() {
  const N: u64 = 100;
  for n in 1..=N {
    let db = Arc::new(prepare_db(n, PAYLOAD_SIZE));
    let mut handles = Vec::<JoinHandle<()>>::with_capacity(10);
    for _ in 0..handles.capacity() {
      let cloned_db = db.clone();
      handles.push(spawn(move || {
        let snapshot = cloned_db.snapshot();
        let mut query = snapshot.query().unwrap();
        for i in 0..=N {
          if let Some(value) = query.get(i).unwrap() {
            assert!(i > 0 || i <= n);
            assert_eq!(&random_payload(PAYLOAD_SIZE, i), &value);
          } else {
            assert!(i == 0 || i > n, "None for i={i}, n={n}");
          }
        }
      }));
    }

    // すべてのスレッドが終了するまで待機
    while !handles.is_empty() {
      let result = handles.pop().unwrap().join();
      assert!(result.is_ok(), "{:?}", result.unwrap_err());
    }
  }
}

#[test]
fn test_multi_threaded_single_append_and_multiple_queries() {
  const N: u64 = 100;
  const WAIT_TIME_SECONDS: u64 = 3;
  let mut db = prepare_db(N, PAYLOAD_SIZE);
  let snapshot = db.snapshot();
  let n = snapshot.n();

  // ロックを使用しないでデータの追加とクエリを並行して実行できることを確認
  let mut query = snapshot.query().unwrap();
  let get_thread = spawn(move || {
    let start = Instant::now();
    while start.elapsed().as_secs() < WAIT_TIME_SECONDS {
      for i in 1..=n {
        let value = query.get(i).unwrap().unwrap();
        assert_eq!(&random_payload(PAYLOAD_SIZE, i), &value);
      }
    }
  });

  let mut query = snapshot.query().unwrap();
  let scan_thread = spawn(move || {
    let start = Instant::now();
    while start.elapsed().as_secs() < WAIT_TIME_SECONDS {
      for i in 1..=n {
        let mut scanned = 0u64;
        let count = query
          .scan(1, i, &mut |entry| -> Result<()> {
            scanned += 1;
            assert_eq!(scanned, entry.enode.meta.address.i);
            let value = entry.enode.payload;
            assert_eq!(&random_payload(PAYLOAD_SIZE, scanned), &value);
            Ok(())
          })
          .unwrap();
        assert_eq!(i, count);
        assert_eq!(i, scanned);
      }
    }
  });

  let start = Instant::now();
  while start.elapsed().as_secs() < WAIT_TIME_SECONDS {
    let n = db.n();
    let value = random_payload(PAYLOAD_SIZE, n + 1);
    db.append(value.as_slice()).unwrap();
  }

  get_thread.join().unwrap();
  scan_thread.join().unwrap();
}

#[test]
fn test_bootstrap() {
  // 未初期化の空のストレージを指定してファイル識別子が書き込まれることを確認
  let buffer = Arc::new(RwLock::new(Vec::new()));
  let storage = BlockStorage::from_buffer(buffer.clone(), false);
  let db = Slate::new(storage).unwrap();
  let content = buffer.read().unwrap();
  let snapshot = db.snapshot();
  let mut query = snapshot.query().unwrap();
  assert_eq!(None, db.root());
  assert_eq!(0, db.n());
  assert_eq!(0, query.revision());
  assert_eq!(None, query.get(1).unwrap());
  assert_eq!(4, content.len());
  assert_eq!(&STORAGE_IDENTIFIER[..], &content[..3]);
  assert_eq!(STORAGE_VERSION, content[3]);

  // 0..n 個のエントリの直列化表現を作成
  let n = 100;
  let samples = (0..=n as u64)
    .map(|i| (0u64..i).map(|j| splitmix64(i * i + j)).collect::<Vec<_>>())
    .map(|value| value.iter().map(|i| i.to_le_bytes().to_vec()).collect::<Vec<_>>())
    .map(|values| {
      // 疑似ランダムな列の直列化形式を取得
      let buffer = Arc::new(RwLock::new(Vec::new()));
      let storage = BlockStorage::from_buffer(buffer.clone(), false);
      let mut db = Slate::new(storage).unwrap();
      for value in values.iter() {
        db.append(value).unwrap();
      }
      let buffer = buffer.clone().read().unwrap().clone();
      (values, buffer)
    })
    .collect::<Vec<_>>();

  // 0..n 個のエントリの直列化が保存されているストレージからデータ列を復元できることを確認
  for (values, buffer) in samples {
    let storage = BlockStorage::from_buffer(Arc::new(RwLock::new(buffer)), false);
    let db = Slate::new(storage).unwrap();
    let snapshot = db.snapshot();
    let mut query = snapshot.query().unwrap();
    for (i, expected) in values.iter().enumerate() {
      println!("{}/{}: {expected:?}", i + 1, db.n());
      let actual = query.get(i as u64 + 1).unwrap().unwrap();
      assert_eq!(expected, &actual);
    }
    match db.root() {
      Some(root) => {
        assert_eq!(values.len() as Index, root.address.i);
        assert_eq!(ceil_log2(values.len() as Index), root.address.j);
      }
      None => assert!(values.is_empty()),
    }
  }
}

#[test]
fn can_resume_from_saved_data() {
  let buffer = Arc::new(RwLock::new(Vec::new()));
  for n in 1..=256 {
    let storage = BlockStorage::from_buffer(buffer.clone(), false);
    let mut db = Slate::new(storage).unwrap();
    assert_eq!(n - 1, db.n());

    // read previously saved data
    let snapshot = db.snapshot();
    let mut query = snapshot.query().unwrap();
    assert_eq!(n - 1, query.revision());
    for i in 1..n {
      let expected = splitmix64(i).to_le_bytes();
      assert_eq!(Some(expected.to_vec()), query.get(i).unwrap());
    }

    // save a new data
    db.append(&splitmix64(n).to_le_bytes()).unwrap();
  }
}

const PAYLOAD_SIZE: usize = 4;

/// データを追加して取得します。
#[test]
fn append_and_get() {
  const N: u64 = 100;
  for n in 1..=N {
    let db = prepare_db(n, PAYLOAD_SIZE);
    let snapshot = db.snapshot();
    let mut query = snapshot.query().unwrap();

    // 単一の葉ノードを参照
    for i in 0..n {
      let expected = random_payload(PAYLOAD_SIZE, i + 1);
      let value = query.get(i + 1).unwrap();
      assert!(value.is_some());
      assert_eq!(&expected, &value.unwrap());
    }

    // 範囲外のノードを参照
    assert!(query.get(0).unwrap().is_none());
    assert!(query.get(n + 1).unwrap().is_none());
  }
}

#[test]
fn append_and_scan() {
  const N: u64 = 105;
  let n = N - 5;
  let mut db = prepare_db(N, PAYLOAD_SIZE);
  let snapshot = db.snapshot_at(n).unwrap();
  let mut query = snapshot.query().unwrap();

  // 全ノードをスキャン
  let mut scanned = 0u64;
  let count = query
    .scan(1, n, &mut |entry| -> Result<()> {
      scanned += 1;
      assert_eq!(scanned, entry.enode.meta.address.i);
      let expected = random_payload(PAYLOAD_SIZE, scanned);
      let actual = entry.enode.payload;
      assert_eq!(&expected, &actual);
      Ok(())
    })
    .unwrap();
  assert_eq!(n, count);
  assert_eq!(n, scanned);

  // 範囲内の任意の部分をスキャン
  for i in 0..10 {
    let i0 = splitmix64(i) % n + 1;
    let i1 = splitmix64(i + 10) % n + 1;
    let (i0, i1) = (min(i0, i1), max(i0, i1));
    scanned = 0u64;
    let count = query
      .scan(i0, i1, &mut |entry| -> Result<()> {
        scanned += 1;
        assert_eq!(i0 + scanned - 1, entry.enode.meta.address.i);
        let expected = random_payload(PAYLOAD_SIZE, i0 + scanned - 1);
        let actual = entry.enode.payload;
        assert_eq!(&expected, &actual, "{i0}..{i1}, scanned={scanned}");
        Ok(())
      })
      .unwrap();
    assert_eq!((i1).abs_diff(i0) + 1, count, "{i0}, {i1}");
    assert_eq!((i1).abs_diff(i0) + 1, scanned, "{i0}, {i1}");
  }

  // 境界を超えたスキャンは何も起きない
  let mut scanned = 0u64;
  let count = query
    .scan(n + 1, u64::MAX, &mut |_| -> Result<()> {
      scanned += 1;
      Ok(())
    })
    .unwrap();
  assert_eq!(0, count);
  assert_eq!(0, scanned);

  let mut scanned = 0u64;
  let count = query
    .scan(0, 0, &mut |_| -> Result<()> {
      scanned += 1;
      Ok(())
    })
    .unwrap();
  assert_eq!(0, count);
  assert_eq!(0, scanned);

  // 部分的に範囲を超えたスキャンは有効な部分のみスキャンされる
  let mut scanned = 0u64;
  let count = query
    .scan(n, n + 1, &mut |entry| -> Result<()> {
      scanned += 1;
      assert_eq!(n, entry.enode.meta.address.i);
      Ok(())
    })
    .unwrap();
  assert_eq!(1, count);
  assert_eq!(1, scanned);

  let mut scanned = 0u64;
  let count = query
    .scan(0, 1, &mut |entry| -> Result<()> {
      scanned += 1;
      assert_eq!(1, entry.enode.meta.address.i);
      Ok(())
    })
    .unwrap();
  assert_eq!(1, count);
  assert_eq!(1, scanned);

  // スキャン範囲の上下逆転を補正して有効な範囲内に収める
  let mut scanned = 0u64;
  let count = query
    .scan(u64::MAX, 0, &mut |entry| -> Result<()> {
      scanned += 1;
      assert_eq!(scanned, entry.enode.meta.address.i);
      Ok(())
    })
    .unwrap();
  assert_eq!(n, count);
  assert_eq!(n, scanned);
}

/// データを追加して切り詰めします。
#[test]
fn append_and_truncate() {
  const N: u64 = 100;
  for n in 1..=N {
    let mut db = prepare_db(n, PAYLOAD_SIZE);

    // 範囲外のエントリを削除
    assert!(!db.truncate(n + 1).unwrap());
    assert_eq!(n, db.n());
    assert_eq!(None, db.snapshot().query().unwrap().get(n + 1).unwrap());
    assert_eq!(Some(random_payload(PAYLOAD_SIZE, n)), db.snapshot().query().unwrap().get(n).unwrap());

    // 最後のエントリのみを削除
    assert!(db.truncate(n).unwrap());
    assert_eq!(n - 1, db.n());
    assert_eq!(None, db.snapshot().query().unwrap().get(n).unwrap());
    if n > 1 {
      assert_eq!(Some(random_payload(PAYLOAD_SIZE, n - 1)), db.snapshot().query().unwrap().get(n - 1).unwrap());
    }

    // 中間以降のエントリを削除
    let s = n / 2 + 1;
    if s != n {
      assert!(db.truncate(s).unwrap());
      assert_eq!(s - 1, db.n());
      assert_eq!(None, db.snapshot().query().unwrap().get(s).unwrap());
      if s > 1 {
        assert_eq!(Some(random_payload(PAYLOAD_SIZE, s - 1)), db.snapshot().query().unwrap().get(s - 1).unwrap());
      }
    }

    // 先頭のエントリを削除
    if n != 1 {
      assert!(db.truncate(1).unwrap());
      assert_eq!(0, db.n());
      assert_eq!(None, db.snapshot().query().unwrap().get(1).unwrap());
    }
  }
}

/// n 個の要素を持つ Slate を構築します。それぞれの要素は乱数で初期化された `payload_size` サイズの値を持ちます。
fn prepare_db(n: u64, payload_size: usize) -> Slate<BlockStorage<MemoryDevice>> {
  let buffer = Arc::new(RwLock::new(Vec::<u8>::with_capacity(4 * 1024)));
  let storage = BlockStorage::from_buffer(buffer.clone(), false);
  let mut db = Slate::new(storage).unwrap();

  for i in 0..n {
    let value = random_payload(payload_size, i + 1);
    let root = db.append(value.as_slice()).expect("append() failed");
    // dump(&mut db);
    assert_eq!(db.root().cloned().unwrap(), root, "n={n}, i={i}");
    assert_eq!(i + 1, root.address.i);
    assert_eq!(ceil_log2(i + 1), root.address.j);
  }
  db
}

/// 指定されたストレージが仕様に準拠していることを検証します。
pub fn verify_storage_spec<S: Storage<Entry> + Debug>(storage: &mut S) {
  // まだ書き込んでいない状態では末尾のエントリは存在しない
  let (entry, first_position) = storage.last().unwrap();
  assert!(entry.is_none());

  // 書き込みと読み込みを相互に実行
  let mut positions = [first_position].to_vec();
  for i in 0..1024 {
    if positions.len() == 1 || splitmix64(i as u64) < u64::MAX / 2 {
      // 書き込みの実行
      let i = positions.len() as u64;
      let position = *positions.last().unwrap();
      let value = i.to_le_bytes().to_vec();
      let entry = build_entry(i, &value, &positions);
      positions.push(storage.put(position, &entry).unwrap());
    } else {
      // 読み込みの実行
      let rand = splitmix64(!i as u64);
      let i = (rand % (positions.len() as u64 - 1) + 1) as Index;
      let position = positions[i as usize - 1];
      let value = i.to_le_bytes().to_vec();
      let expected = build_entry(i, &value, &positions);

      let mut cursor = storage.reader().unwrap();
      let entry = cursor.read(position).unwrap();
      assert_eq!(i, entry.enode.meta.address.i);
      assert_eq!(0, entry.enode.meta.address.j);
      assert_eq!(position, entry.enode.meta.address.position);
      assert_eq!(&value, &entry.enode.payload);
      let hash = Hash::from_bytes(&value);
      assert_eq!(hash, entry.enode.meta.hash);
      assert_eq!(&expected, &entry);
    }
  }

  verify_storage_truncate(storage, first_position);
  verify_storage_scan(storage, first_position);
}

fn verify_storage_truncate<S: Storage<Entry> + Debug>(storage: &mut S, first_position: Position) {
  // 先頭での切り詰めは空のストレージになる
  assert!(storage.truncate(first_position).unwrap());
  let (entry, truncated_first_position) = storage.last().unwrap();
  assert_eq!(first_position, truncated_first_position);
  assert!(entry.is_none());

  // 空の状態での切り詰めは何も起きない
  assert!(!storage.truncate(first_position).unwrap());
  let (entry, truncated_first_position) = storage.last().unwrap();
  assert_eq!(first_position, truncated_first_position);
  assert!(entry.is_none());

  // 現在のポジションを越えた切り詰めは何も起きない
  assert!(!storage.truncate(100).unwrap());
  let (entry, truncated_first_position) = storage.last().unwrap();
  assert_eq!(first_position, truncated_first_position);
  assert!(entry.is_none());

  // 書き込みの実行
  let mut positions = [truncated_first_position].to_vec();
  for _ in 0..256 {
    let i = positions.len() as u64;
    let position = *positions.last().unwrap();
    let value = i.to_le_bytes().to_vec();
    let entry = build_entry(i, &value, &positions);
    positions.push(storage.put(position, &entry).unwrap());
  }
  assert!(!storage.truncate(*positions.last().unwrap()).unwrap());
  assert!(!storage.truncate(*positions.last().unwrap() + 1).unwrap());

  // 末尾から順にエントリを削除
  positions.pop();
  while !positions.is_empty() {
    let n = positions.len() as Index;
    let position = positions.pop().unwrap();
    let mut reader = storage.reader().unwrap(); // 切り詰め前に作成した reader
    let entry = reader.read(position).unwrap();
    assert_eq!(n, entry.enode.meta.address.i);
    assert_eq!(position, entry.enode.meta.address.position);
    assert!(storage.truncate(position).unwrap());
    for i in 1..n {
      let p = positions[i as usize - 1];
      let e2 = reader.read(p).unwrap();
      let e3 = storage.reader().unwrap().read(p).unwrap();
      assert_eq!(i, e2.enode.meta.address.i);
      assert_eq!(p, e2.enode.meta.address.position);
      assert_eq!(e2, e3);
    }
    let (el, p) = storage.last().unwrap();
    match positions.last() {
      Some(pl) => {
        assert_eq!(reader.read(*pl).unwrap(), el.clone().unwrap());
        assert_eq!(el.unwrap().enode.meta.address.position, *pl);
      }
      None => {
        assert_eq!(None, el);
        assert_eq!(first_position, p);
      }
    }
    assert!(matches!(reader.read(position), Err(Error::Io { .. })));
    assert!(matches!(storage.reader().unwrap().read(position), Err(Error::Io { .. })));
  }
}

fn verify_storage_scan<S: Storage<Entry> + Debug>(storage: &mut S, first_position: Position) {
  // 読み出し用データの作成
  const N: Index = 256;
  storage.truncate(first_position).unwrap();
  let mut positions = [first_position].to_vec();
  for i in 1..=N {
    let position = *positions.last().unwrap();
    let value = i.to_le_bytes().to_vec();
    let entry = build_entry(i, &value, &positions);
    positions.push(storage.put(position, &entry).unwrap());
  }

  // すべてのエントリをスキャン
  let mut reader = storage.reader().unwrap();
  let mut scanned = 0u64;
  let count = reader
    .scan(first_position, N, |entry| -> Result<()> {
      scanned += 1;
      let i = scanned as Index;
      let expected = build_entry(i, i.to_le_bytes().as_ref(), &positions);
      assert_eq!(expected, entry);
      Ok(())
    })
    .unwrap();
  assert_eq!(N, count);
  assert_eq!(N, scanned);

  // 範囲を超えたスキャンは何も起きない
  let mut scanned = 0u64;
  let count = reader
    .scan(u64::MAX, u64::MAX, |_| -> Result<()> {
      scanned += 1;
      Ok(())
    })
    .unwrap();
  assert_eq!(0, count);
  assert_eq!(0, scanned);
}

#[test]
fn referencing_a_specific_snapshot() {
  let storage = BlockStorage::memory();
  let mut db = Slate::new(storage).unwrap();
  assert!(db.snapshot_at(0).is_err());
  assert!(db.snapshot_at(1).is_err());

  let mut roots = Vec::new();
  for n in 1..=64 {
    roots.push(db.append(&[n - 1]).unwrap());
  }

  assert!(db.snapshot_at(0).is_err());
  assert!(db.snapshot_at(65).is_err());
  for n in 1..=64 {
    let snapshot = db.snapshot_at(n).unwrap();
    let root = snapshot.root();
    assert_eq!(n, root.address.i);
    assert_eq!(roots[n as usize - 1], root);
    assert_eq!(None, snapshot.query().unwrap().get(0).unwrap());
    assert_eq!(vec![0u8], snapshot.query().unwrap().get(1).unwrap().unwrap());
    assert_eq!(vec![n as u8 - 1], snapshot.query().unwrap().get(n).unwrap().unwrap());
    assert_eq!(None, snapshot.query().unwrap().get(n + 1).unwrap());
  }
}

#[test]
fn walk_down_for_empty() {
  let storage = BlockStorage::memory();
  let db = Slate::new(storage).unwrap();
  let snapshot = db.snapshot();
  let mut query = snapshot.query().unwrap();
  let mut count = 0;
  query
    .walk_down(&mut |_, i, j, _, _| {
      println!("({i}, {j})");
      count += 1;
      WalkDirection::Both
    })
    .unwrap();
  assert_eq!(0, count);
}

#[test]
fn walk_down_randomly_left_and_right() {
  let mut rand = splitmix64(9876543210);
  for _ in 0..512 {
    let storage = BlockStorage::memory();
    let mut db = Slate::new(storage).unwrap();
    rand = splitmix64(rand);
    for i in 1..=((rand % 0xFF) as u8) {
      db.append(&[i]).unwrap();
    }
    let snapshot = db.snapshot();

    let mut next = None;
    let mut query = snapshot.query().unwrap();
    query
      .walk_down(&mut |_revision, i, j, _hash, _value| {
        if let Some((i2, j2)) = next {
          assert!(i == i2 && j == j2);
        }
        if j == 0 {
          next = None;
          WalkDirection::Terminate
        } else {
          let ((il, jl), (ir, jr)) = subnodes_of(i, j);
          rand = splitmix64(rand);
          if rand.is_multiple_of(2) {
            next = Some((il, jl));
            WalkDirection::Left
          } else {
            next = Some((ir, jr));
            WalkDirection::Right
          }
        }
      })
      .unwrap();
    assert_eq!(None, next);
  }
}

#[test]
fn walk_down_all_nodes() {
  let mut rand = splitmix64(!9876543210);
  for _ in 0..512 {
    let storage = BlockStorage::memory();
    let mut db = Slate::new(storage).unwrap();
    rand = splitmix64(rand);
    for i in 0..=max(1, (rand % 0xFF) as u8) {
      db.append(&[i]).unwrap();
    }
    let snapshot = db.snapshot();

    let mut nodes = HashSet::new();
    let mut query = snapshot.query().unwrap();
    query
      .walk_down(&mut |_revision, i, j, _hash, _value| {
        assert!(nodes.insert((i, j)), "duplicate visited: ({i}, {j})");
        WalkDirection::Both
      })
      .unwrap();
    assert_eq!(total_nodes(snapshot.revision()), nodes.len() as u128);
  }
}

#[test]
fn generate_stratum_sample() {
  // sample cannot obtain from empty tree
  let storage = BlockStorage::memory();
  let mut db = Slate::new(storage).unwrap();
  let snapshot = db.snapshot();
  let mut query = snapshot.query().unwrap();
  assert_eq!(None, query.get_sample(0).unwrap());
  assert_eq!(None, query.get_sample(1).unwrap());

  for n in 1..=64 {
    db.append(&[n - 1]).unwrap();
    let snapshot = db.snapshot();
    let mut query = snapshot.query().unwrap();
    assert!(query.get_sample(0).unwrap().is_none());
    for i in 1..=n {
      let sample = query.get_sample(i as Index).unwrap().unwrap();
      assert_eq!(i as Index, sample.leaf.i);
      assert_eq!(vec![i - 1], sample.leaf.value);
      assert_eq!(stratum_sample_size(n as Index, i as Index), sample.witnesses.len() as u8, "{n}: {sample:?}");
    }
    assert!(query.get_sample(n as Index + 1).unwrap().is_none());
  }
}

#[test]
fn compare_stratum_sample() {
  // generate samples from the same trees and confirm that they match
  let mut db1 = Slate::new(BlockStorage::memory()).unwrap();
  let mut db2 = Slate::new(BlockStorage::memory()).unwrap();
  for n in 1..=64 {
    db1.append(&[n - 1]).unwrap();
    db2.append(&[n - 1]).unwrap();
    let s1 = db1.snapshot();
    let s2 = db2.snapshot();
    let mut q1 = s1.query().unwrap();
    let mut q2 = s2.query().unwrap();
    for i in 1..=n {
      let sample1 = q1.get_sample(i as Index).unwrap().unwrap();
      let sample2 = q2.get_sample(i as Index).unwrap().unwrap();
      assert_eq!(db1.root(), db2.root());
      assert_eq!(sample1.root_hash().unwrap(), sample2.root_hash().unwrap());
      assert_eq!(db1.root().unwrap().hash, sample1.root_hash().unwrap());
      assert_eq!(Prove::Identical, sample1.compare(&sample2).unwrap());
    }
  }
}

#[test]
fn detect_differences_in_stratum_samples() {
  const N: Index = 1024;
  let rand = splitmix64(1231239999);
  let garbling_index = rand % N + 1;
  let garbling_bit = ((rand / (N + 1)) % u8::BITS as Index) as usize;
  println!("grabling b_{garbling_index}:{garbling_bit}");

  // build instances that differ by only 1-bit
  let mut db1 = Slate::new(BlockStorage::memory()).unwrap();
  let mut db2 = Slate::new(BlockStorage::memory()).unwrap();
  for n in 1..=N {
    let b = (splitmix64(n) & 0xFF) as u8;
    db1.append(&[b]).unwrap();
    if n != garbling_index {
      db2.append(&[b]).unwrap();
    } else {
      db2.append(&[b ^ (1 << garbling_bit)]).unwrap();
    }
  }
  assert_ne!(db1.root(), db2.root());
  let s1 = db1.snapshot();
  let s2 = db2.snapshot();
  let mut q1 = s1.query().unwrap();
  let mut q2 = s2.query().unwrap();
  for i in 1..=N {
    let sample1 = q1.get_sample(i as Index).unwrap().unwrap();
    let sample2 = q2.get_sample(i as Index).unwrap().unwrap();
    assert_ne!(db1.root(), db2.root());
    assert_ne!(sample1.root_hash().unwrap(), sample2.root_hash().unwrap());
    assert_eq!(db1.root().unwrap().hash, sample1.root_hash().unwrap());
    assert_eq!(db2.root().unwrap().hash, sample2.root_hash().unwrap());
    match sample1.compare(&sample2).unwrap() {
      Prove::Identical => panic!(),
      Prove::Divergent(divergents) => {
        assert_eq!(1, divergents.len(), "{sample1:?} {sample2:?}");
        let (i, j) = divergents[0];
        assert!(contains(i, j, garbling_index));
      }
    }
  }

  // find the index of different leaf nodes
  let mut sample1 = q1.get_sample(s1.revision()).unwrap().unwrap();
  let mut steps = 0;
  let detected_i = loop {
    let sample2 = q2.get_sample(sample1.leaf.i).unwrap().unwrap();
    let (i, j) = match sample2.compare(&sample1).unwrap() {
      Prove::Divergent(divergents) => divergents[0],
      Prove::Identical => panic!(),
    };
    steps += 1;
    if j == 0 {
      break i;
    }
    sample1 = q1.get_sample(i).unwrap().unwrap();
  };
  println!("{steps} interactions");
  assert_eq!(garbling_index, detected_i);
}

fn build_entry(i: Index, value: &[u8], positions: &[Index]) -> Entry {
  let position = positions[i as usize - 1];
  let model = Model::new(i);
  let enode =
    ENode { meta: MetaInfo::new(Address::new(i, 0, position), Hash::from_bytes(value)), payload: value.to_vec() };
  let inodes = model
    .inodes()
    .iter()
    .map(|inode| {
      let meta = MetaInfo::new(Address::new(inode.node.i, inode.node.j, position), Hash::new([0; Hash::SIZE]));
      let left = Address::new(inode.left.i, inode.left.j, positions[inode.left.i as usize - 1]);
      let right = Address::new(inode.right.i, inode.right.j, positions[inode.right.i as usize - 1]);
      INode::new(meta, left, right)
    })
    .collect();
  Entry::new(enode, inodes)
}

pub fn random_payload(length: usize, seed: u64) -> Vec<u8> {
  let mut buffer = Vec::with_capacity(length);
  let mut rand = seed;
  while buffer.len() < length {
    rand = splitmix64(rand);
    let bytes = rand.to_be_bytes();
    let len = min(bytes.len(), length - buffer.len());
    buffer.extend_from_slice(&bytes[..len]);
  }
  buffer
}

/// 指定された接頭辞と接尾辞を持つ 0 バイトの一時ファイルをシステムの一時ディレクトリの下に作成します。
/// 作成したファイルは呼び出し側で削除する必要があります。
pub fn temp_file(prefix: &str, suffix: &str) -> PathBuf {
  let tmp = temprary_directory();
  for i in 0u16..=u16::MAX {
    let file_name = if i == 0 { format!("slate-{prefix}{suffix}") } else { format!("slate-{prefix}_{i}{suffix}") };
    let file = tmp.to_path_buf().join(file_name);
    match OpenOptions::new().write(true).create_new(true).open(&file) {
      Ok(_) => return file,
      Err(err) if err.kind() == ErrorKind::AlreadyExists => (),
      Err(err) => panic!("{}", err),
    }
  }
  panic!("cannot create new temporary file: {}{}{}nnn{}", tmp.to_string_lossy(), MAIN_SEPARATOR, prefix, suffix);
}

/// 指定された接頭辞と接尾辞を持つ空の一時ディレクトリをシステムの一時ディレクトリの下に作成します。
/// 作成したディレクトリは呼び出し側で削除する必要があります。
pub fn temp_dir(prefix: &str, suffix: &str) -> PathBuf {
  let tmp = temprary_directory();
  for i in 0u16..=u16::MAX {
    let dir_name = if i == 0 { format!("slate-{prefix}{suffix}") } else { format!("slate-{prefix}_{i}{suffix}") };
    let dir = tmp.to_path_buf().join(dir_name);
    if !dir.exists() && create_dir_all(&dir).is_ok() {
      return dir;
    }
  }
  panic!("cannot create new temporary directory: {}{}{}nnn{}", tmp.to_string_lossy(), MAIN_SEPARATOR, prefix, suffix);
}

pub fn splitmix64(x: u64) -> u64 {
  let mut z = x;
  z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
  z ^ (z >> 31)
}

use std::fs::{OpenOptions, remove_file};
use std::io::ErrorKind;
use std::path::MAIN_SEPARATOR;
use std::{env::temp_dir as temporary_directory, path::PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};
use slate::{BlockStorage, Prove, Slate};

const ITER_SIZE: usize = 10 * 1024;

fn append(c: &mut Criterion) {
  let path = temp_file("bench", ".db");
  c.bench_function(&format!("append() single call"), |b| {
    b.iter_batched(
      || {
        remove_file(&path).unwrap();
        let storage = BlockStorage::from_file(&path, false).unwrap();
        Slate::new(storage).unwrap()
      },
      |mut slate| {
        slate.append(&[0u8; 0]).unwrap();
      },
      criterion::BatchSize::PerIteration,
    );
  });
  remove_file(&path).unwrap();
}

fn get(c: &mut Criterion) {
  let path = temp_file("bench", ".db");
  let storage = BlockStorage::from_file(&path, false).unwrap();
  let mut slate = Slate::new(storage).unwrap();
  for _ in 0..ITER_SIZE {
    slate.append(&[0u8; 0]).unwrap();
  }
  let mut query = slate.snapshot().query().unwrap();

  // 事前にランダムなインデックス列を生成
  let mut indices = Vec::with_capacity(ITER_SIZE);
  let mut r = 0x12345678abcdefu64;
  for _ in 0..ITER_SIZE {
    let index = r % ITER_SIZE as u64 + 1;
    indices.push(index);
    r = splitmix64(r);
  }

  c.bench_function(&format!("get() single call"), |b| {
    let mut index = 0;
    b.iter(|| {
      let i = indices[index % ITER_SIZE];
      query.get(i).unwrap().unwrap();
      index += 1;
    });
  });
  remove_file(&path).unwrap();
}

fn prove(c: &mut Criterion) {
  let files = 10;
  let paths = (0..files).map(|i| temp_file(&format!("bench{i}"), ".db")).collect::<Vec<_>>();
  let mut slates = paths
    .iter()
    .enumerate()
    .map(|(num, path)| {
      let storage = BlockStorage::from_file(path, false).unwrap();
      let mut slate = Slate::new(storage).unwrap();
      for i in 1..=ITER_SIZE {
        if i == ITER_SIZE / files * num {
          slate.append(&0u64.to_le_bytes()).unwrap();
        } else {
          slate.append(&i.to_le_bytes()).unwrap();
        }
      }
      slate
    })
    .collect::<Vec<_>>();
  let mut queries = slates.iter_mut().map(|slate| slate.snapshot().query().unwrap()).collect::<Vec<_>>();

  // 事前にランダムなインデックス列を生成
  let mut indices = Vec::with_capacity(ITER_SIZE);
  let mut r = 0x12345678abcdefu64;
  for _ in 0..ITER_SIZE {
    let index = r as usize % files;
    indices.push(index);
    r = splitmix64(r);
  }

  c.bench_function(&format!("prove() single call"), |b| {
    let mut index = 0;
    b.iter(|| {
      let i1 = indices[index % ITER_SIZE];
      let i2 = indices[(index + ITER_SIZE / 2) % ITER_SIZE];

      let mut base_i = ITER_SIZE as u64;
      let _the_smallest_i_with_a_different_value = loop {
        let sample1 = queries.get_mut(i1).unwrap().get_sample(base_i).unwrap().unwrap();
        let sample2 = queries.get_mut(i2).unwrap().get_sample(sample1.leaf.i).unwrap().unwrap();
        match sample1.compare(&sample2).unwrap() {
          Prove::Identical => {
            break None;
          }
          Prove::Divergent(diffs) => {
            let (i, j) = diffs.iter().min_by(|a, b| a.0.cmp(&b.0)).copied().unwrap();
            if j == 0 {
              break Some(i);
            }
            base_i = i;
          }
        }
      };

      index += 1;
    });
  });

  for path in paths {
    remove_file(&path).unwrap();
  }
}

criterion_group!(benches, append, get, prove);
criterion_main!(benches);

fn temp_file(prefix: &str, suffix: &str) -> PathBuf {
  let tmp = temporary_directory();
  for i in 0u16..=u16::MAX {
    let file_name = format!("slate-{prefix}{i}{suffix}");
    let mut file = tmp.to_path_buf();
    file.push(file_name);
    match OpenOptions::new().write(true).create_new(true).open(&file) {
      Ok(_) => return file,
      Err(err) if err.kind() == ErrorKind::AlreadyExists => (),
      Err(err) => panic!("{}", err),
    }
  }
  panic!("cannot create new temporary file: {}{}{}nnn{}", tmp.to_string_lossy(), MAIN_SEPARATOR, prefix, suffix);
}

fn splitmix64(x: u64) -> u64 {
  let mut z = x;
  z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
  z ^ (z >> 31)
}

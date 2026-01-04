use std::collections::HashMap;
use std::io::{ErrorKind, Read, Seek, SeekFrom};

use byteorder::{LittleEndian, ReadBytesExt};

use crate::checksum::ChecksumRead;
use crate::{Hash, Result, STORAGE_IDENTIFIER, hex, is_version_compatible};

pub trait SeekRead: Seek + std::io::Read {}

/// 指定されたカーソルから読み出される直列化された木構造を人の見やすい形式で出力します。
pub fn report<T: AsRef<[u8]>>(cursor: &mut std::io::Cursor<T>) -> Result<()> {
  let eof = cursor.seek(SeekFrom::End(0))?;
  cursor.seek(SeekFrom::Start(0))?;

  let eval = |f: bool| if f { '✔' } else { '❌' };
  let eval_with_msg =
    |f: bool, msg: String| format!("{}{}", eval(f), if !f { format!("; {msg}") } else { "".to_string() });

  // 識別子の読み込み
  let mut identifier = [0u8; 4];
  cursor.read_exact(&mut identifier)?;
  println!("IDENTIFIER: {} {}", hex(&identifier[0..3]), eval(identifier[0..3] == STORAGE_IDENTIFIER));
  println!(
    "VERSION   : {}.{} {}",
    identifier[3] >> 4,
    identifier[3] & 0x0F,
    eval(is_version_compatible(identifier[3]))
  );

  let mut location = HashMap::<u64, u64>::new();
  let mut hashes = HashMap::<(u64, u8), Hash>::new();
  let mut hash = [0u8; Hash::SIZE];
  while cursor.stream_position()? < eof {
    let position = cursor.stream_position()?;
    let mut r = ChecksumRead::new(cursor);

    // エントリ
    let i = r.read_u64::<LittleEndian>()?;
    location.insert(i, position);
    let inode_size = r.read_u8()?;
    let mut inodes = Vec::<(u8, u64, u64, u8, Hash)>::new();
    for _ in 0..inode_size {
      // 中間ノード
      let j = r.read_u8()? + 1;
      let left_position = r.read_u64::<LittleEndian>()?;
      let left_i = r.read_u64::<LittleEndian>()?;
      let left_j = r.read_u8()?;
      r.read_exact(&mut hash)?;

      location.insert(i, position);
      hashes.insert((i, j), Hash::new(hash));
      inodes.push((j, left_position, left_i, left_j, Hash::new(hash)));
    }

    // 葉ノード
    let payload_len = r.read_u32::<LittleEndian>()?;
    let mut payload = vec![0u8; payload_len as usize];
    r.read_exact(payload.as_mut_slice())?;
    r.read_exact(&mut hash)?;
    hashes.insert((i, 0), Hash::new(hash));

    // トレイラー
    let (_, (offset, checksum), (_, actual_checksum)) = r.read_delimiter()?;
    let trailer_position = cursor.position();

    println!("--------");
    println!("ENTRY: {i} @{position}");
    let mut prev_j = 0;
    for (j, left_position, left_i, left_j, hash) in inodes.iter() {
      println!("INODE: ({i}, {j})");
      println!(
        "  LEFT : ({}, {}) @{} {}",
        left_i,
        left_j,
        left_position,
        eval(location.get(left_i).map(|x| *x == *left_position).unwrap_or(false))
      );
      println!(
        "  RIGHT: ({}, {}) @{} {}",
        i,
        prev_j,
        position,
        eval(prev_j == 0 || location.get(&i).map(|x| *x == position).unwrap_or(false))
      );
      let hl = hashes.get(&(*left_i, *left_j));
      let hr = hashes.get(&(i, prev_j));
      let h = hl.and_then(|hl| hr.map(|hr| hl.combine(*left_j, hr)));
      let msg = format!(
        "hash({} || {}) = {}",
        hl.map(|hl| hex(&hl.value)).unwrap_or_default(),
        hr.map(|hr| hex(&hr.value)).unwrap_or_default(),
        h.map(|h| hex(&h.value)).unwrap_or_default()
      );
      println!(
        "  HASH : {} ({} bytes) {}",
        hex(&hash.value),
        hash.value.len(),
        eval_with_msg(h.map(|h| h == *hash).unwrap_or(false), msg)
      );
      prev_j = *j;
    }
    println!("ENODE: ({i}, 0)");
    let payload_hex = hex(&payload);
    let payload_hex = if payload_hex.len() > 32 + 32 {
      format!("{}...{}", &payload_hex[0..32], &payload_hex[payload_hex.len() - 32..payload_hex.len()])
    } else {
      payload_hex
    };
    println!("  PAYLOAD: {} ({} bytes) {}", payload_hex, payload.len(), eval(payload_len == payload.len() as u32));
    println!(
      "  HASH   : {} ({} bytes) {}",
      hex(&hash),
      hash.len(),
      eval(Hash::from_bytes(0, &payload) == Hash::new(hash))
    );
    println!("OFFSET   : {} {}", offset, eval(trailer_position - offset as u64 == position));
    println!("CHECKSUM : {} {}", hex(&checksum.to_le_bytes()), eval(checksum == actual_checksum));
  }

  Ok(())
}

/// 指定された入力ストリームから読み出されるすべてのバイトを人の見やすい形式で出力します。
pub fn hex_dump(r: &mut dyn Read) -> Result<()> {
  let mut error: Option<std::io::Error> = None;
  let mut line = 0;
  while error.is_none() {
    let mut ascii = Vec::<String>::with_capacity(16);
    let mut hex = Vec::<String>::with_capacity(16);
    for _ in 0..16 {
      match r.read_u8() {
        Ok(ch) => {
          ascii.push((if (ch as char).is_ascii_graphic() { ch as char } else { '.' }).to_string());
          hex.push(format!("{ch:02X}"));
        }
        Err(err) => {
          error = Some(err);
          break;
        }
      }
    }
    while ascii.len() < 16 {
      ascii.push(" ".to_string());
      hex.push("  ".to_string());
    }
    println!("{:08X}: {}  {}: {}", line * 16, hex[0..8].join(" "), hex[8..16].join(" "), ascii.join(""));
    line += 1;
  }

  let error = error.unwrap();
  if error.kind() != ErrorKind::UnexpectedEof {
    println!("ERROR: {error}");
  }
  Ok(())
}

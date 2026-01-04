use std::sync::PoisonError;

#[derive(thiserror::Error, Debug)]
pub enum Error {
  // ローカルファイルのオープンに失敗
  #[error("Failed to open local file {file}; {message}")]
  FailedToOpenLocalFile { file: String, message: String },

  // ストレージの内容が Slate のものではない
  #[error("The contents of storage are not for Slate: {message}")]
  StorageisNotForSlate { message: &'static str },

  // 互換性のないファイルバージョン
  #[error("Slate storage version is incompatible: {0}.{1}")]
  IncompatibleVersion(u8, u8),

  // ペイロードのサイズが大きすぎる
  #[error("Payload size is too large: {size}")]
  TooLargePayload { size: usize },

  // ストレージ破損に対する一般メッセージ
  #[error("DAMAGED STORAGE: {0}")]
  DamagedStorage(String),

  // エントリ先頭へのオフセットが間違っている
  #[error("DAMAGED STORAGE: incorrect entry-head offset field; recorded as {expected}, but actually {actual}")]
  IncorrectEntryHeadOffset { expected: u32, actual: u32 },

  // チェックサム検査に失敗
  #[error(
    "DAMAGED STORAGE: checksum verification failed for {length} bytes starting at {at}; expected {expected} but got {actual}"
  )]
  ChecksumVerificationFailed { at: u64, length: u32, expected: u64, actual: u64 },

  // 内部状態とストレージ上のデータが矛盾している
  #[error("INCONSISTENCY STATE: between the internally state and the data in storage; {message}")]
  InternalStateInconsistency { message: String },

  // 層サンプル比較時のエラー
  #[error("{0}")]
  SampleVerificationFailed(String),

  /// 不正なパラメータ指定
  #[error("{0}")]
  InvalidArgument(String),

  #[error("The user function returned an error during the callback: {0}")]
  Callback(Box<dyn std::error::Error + Send + Sync>),

  #[error("I/O error: {source}")]
  Io {
    #[from]
    source: std::io::Error,
  },

  #[error("{source}")]
  Otherwise {
    #[from]
    source: Box<dyn std::error::Error + Send + Sync>,
  },

  #[cfg(feature = "rocksdb")]
  #[error("{0}")]
  RocksDB(#[from] rocksdb::Error),

  #[error("{0}")]
  LockError(String),
}

impl<T> From<PoisonError<T>> for Error {
  fn from(err: PoisonError<T>) -> Self {
    Error::LockError(err.to_string())
  }
}

// Compile-time guarantee that Error is Send + Sync
fn _assert_send_sync() {
  fn assert_send_sync<T: Send + Sync>() {}
  assert_send_sync::<Error>();
}

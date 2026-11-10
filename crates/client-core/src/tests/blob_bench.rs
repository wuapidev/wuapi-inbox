//! Timing of the metadata reads next to large blobs. `--ignored` only.

use super::*;
use crate::{CachedMedia, LibraryKind, LibrarySource, NewLibraryItem};
use std::time::Instant;

#[test]
#[ignore]
fn bench_metadata_reads_beside_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench.db");
    let key = StoreKey::from_bytes([7; 32]);
    let store = Store::open(&path, Some(&key)).unwrap();
    let media_rows: usize = std::env::var("MEDIA_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let stickers: usize = std::env::var("STICKERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    for i in 0..media_rows {
        let bytes = vec![(i % 251) as u8; 800 * 1024];
        store
            .put_media(
                &format!("file:{i}"),
                &CachedMedia {
                    bytes,
                    mime: Some("image/jpeg".into()),
                    size: Some((800, 600)),
                },
                Timestamp::from_millis(i as i64),
                u64::MAX,
            )
            .unwrap();
    }
    for i in 0..stickers {
        let mut bytes = vec![(i % 253) as u8; 600 * 1024];
        bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
        let new = NewLibraryItem {
            kind: LibraryKind::Sticker,
            bytes,
            mime: "image/webp".into(),
            animated: true,
            size: Some((512, 512)),
            source: LibrarySource::Imported,
            name: None,
            pack: None,
            thumb: Some((vec![1; 20 * 1024], "image/png".into())),
        };
        store
            .library_add(
                new,
                &format!("s{i}"),
                Timestamp::from_millis(i as i64),
                u64::MAX,
            )
            .unwrap();
    }
    // Fresh connections: nothing in SQLite's page cache.
    drop(store);
    let store = Store::open(&path, Some(&key)).unwrap();
    let t = |label: &str, f: &mut dyn FnMut()| {
        let at = Instant::now();
        f();
        eprintln!("{label:<40} {:>8.1} ms", at.elapsed().as_secs_f64() * 1000.);
    };
    t("library_items(Sticker) [cold]", &mut || {
        store.library_items(LibraryKind::Sticker).unwrap();
    });
    t("library_items(Sticker) [warm]", &mut || {
        store.library_items(LibraryKind::Sticker).unwrap();
    });
    t("library_favorites", &mut || {
        store.library_favorites(LibraryKind::Sticker).unwrap();
    });
    t("library_recent", &mut || {
        store.library_recent(LibraryKind::Sticker, 30).unwrap();
    });
    t("library_heard", &mut || {
        store.library_heard(40).unwrap();
    });
    t("media_size (one row)", &mut || {
        store.media_size("file:3").unwrap();
    });
    t("media_total (SUM size)", &mut || {
        store.media_total().unwrap();
    });
    t("put_media 50KB (writer held)", &mut || {
        store
            .put_media(
                "new",
                &CachedMedia {
                    bytes: vec![9; 50 * 1024],
                    mime: None,
                    size: None,
                },
                Timestamp::from_millis(1),
                u64::MAX,
            )
            .unwrap();
    });
}

#[test]
#[ignore]
fn bench_v16_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    let key = StoreKey::from_bytes([7; 32]);
    let rows: usize = std::env::var("MEDIA_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(800);
    {
        let mut raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch(&format!(
            "PRAGMA key = \"x'{}'\"; PRAGMA journal_mode=WAL;",
            key.to_hex()
        ))
        .unwrap();
        crate::migrations::migrate_to(&mut raw, 15).unwrap();
        let tx = raw.transaction().unwrap();
        for i in 0..rows {
            tx.execute("INSERT INTO media_cache (key, bytes, mime, width, height, size, last_used) VALUES (?1, ?2, 'image/jpeg', 1, 1, 819200, ?3)",
                rusqlite::params![format!("file:{i}"), vec![(i % 251) as u8; 800 * 1024], i as i64]).unwrap();
        }
        for i in 0..100 {
            tx.execute("INSERT INTO library_items (id, kind, mime, size, source, bytes, thumb, thumb_mime, added_at) VALUES (?1, 'sticker', 'image/webp', 614400, 'imported', ?2, ?3, 'image/png', 1)",
                rusqlite::params![format!("s{i}"), vec![1u8; 600 * 1024], vec![2u8; 20 * 1024]]).unwrap();
        }
        tx.commit().unwrap();
    }
    let at = Instant::now();
    let store = Store::open(&path, Some(&key)).unwrap();
    eprintln!(
        "v15 -> v16 with {rows} x 800KB cache + 100 stickers: {:.0} ms",
        at.elapsed().as_secs_f64() * 1000.
    );
    assert_eq!(store.media_total().unwrap(), rows as u64 * 819200);
}

use super::*;

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kit-db-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("base.db")
}

#[test]
fn opens_with_its_key_only() {
    let path = temp("key");
    let conn = open(&path, &[7; 32]).unwrap();
    set_meta(&conn, "a", "1").unwrap();
    drop(conn);
    // Файл зашифрован: в нём нет заголовка обычной базы SQLite.
    let head = std::fs::read(&path).unwrap();
    assert_ne!(&head[..15], b"SQLite format 3");
    assert!(matches!(open(&path, &[8; 32]), Err(Open::WrongKey)));
    let conn = open(&path, &[7; 32]).unwrap();
    assert_eq!(meta(&conn, "a").unwrap().as_deref(), Some("1"));
}

#[test]
fn tables_are_the_same_as_in_the_apps() {
    let conn = open(&temp("tables"), &[1; 32]).unwrap();
    let mut st = conn.prepare("SELECT name, sql FROM sqlite_master WHERE type = 'table' ORDER BY name").unwrap();
    let tables: Vec<(String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
    let names: Vec<&str> = tables.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["blobs", "events", "items", "meta"]);
    let sql = |n: &str| tables.iter().find(|(t, _)| t == n).unwrap().1.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(sql("events"), "CREATE TABLE events( device TEXT NOT NULL, seq INTEGER NOT NULL, lamport INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY(device, seq))");
    assert_eq!(sql("items"), "CREATE TABLE items( kind TEXT NOT NULL, id TEXT NOT NULL, body TEXT, deleted INTEGER NOT NULL DEFAULT 0, lamport INTEGER NOT NULL, device TEXT NOT NULL, PRIMARY KEY(kind, id))");
    assert_eq!(sql("blobs"), "CREATE TABLE blobs( id TEXT PRIMARY KEY, mime TEXT NOT NULL, size INTEGER NOT NULL, data BLOB NOT NULL, added INTEGER NOT NULL)");
    assert_eq!(sql("meta"), "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)");
}

#[test]
fn meta_values() {
    let conn = open(&temp("meta"), &[2; 32]).unwrap();
    assert_eq!(meta(&conn, "x").unwrap(), None);
    set_meta(&conn, "cloud.state", "a").unwrap();
    set_meta(&conn, "cloud.state", "b").unwrap();
    set_meta(&conn, "cloud.every", "auto").unwrap();
    set_meta(&conn, "lan.peers", "[]").unwrap();
    assert_eq!(meta(&conn, "cloud.state").unwrap().as_deref(), Some("b"));
    assert_eq!(meta_prefix(&conn, "cloud.").unwrap(), vec![("cloud.every".into(), "auto".into()), ("cloud.state".into(), "b".into())]);
    delete_meta(&conn, "cloud.state").unwrap();
    assert_eq!(meta(&conn, "cloud.state").unwrap(), None);
}

#[test]
fn journal() {
    let conn = open(&temp("journal"), &[3; 32]).unwrap();
    assert_eq!(next(&conn, "a").unwrap(), (1, 1));
    assert!(insert_event(&conn, "a", 1, 1, "{\"n\":1}").unwrap());
    assert!(insert_event(&conn, "a", 2, 3, "{\"n\":2}").unwrap());
    assert!(insert_event(&conn, "b", 1, 2, "{\"n\":3}").unwrap());
    // Повтор не пишется и не меняет тело.
    assert!(!insert_event(&conn, "a", 1, 9, "{\"n\":9}").unwrap());
    assert_eq!(event_body(&conn, "a", 1).unwrap().as_deref(), Some("{\"n\":1}"));
    assert_eq!(event_body(&conn, "a", 7).unwrap(), None);
    assert_eq!(event_count(&conn).unwrap(), 3);
    assert_eq!(next(&conn, "a").unwrap(), (3, 4));
    assert_eq!(next(&conn, "c").unwrap(), (1, 4));
    assert_eq!(versions(&conn).unwrap(), BTreeMap::from([("a".to_string(), 2), ("b".to_string(), 1)]));
    let seqs = |rows: Vec<Row>| rows.into_iter().map(|r| (r.device, r.seq)).collect::<Vec<_>>();
    assert_eq!(seqs(events_after(&conn, "a", 1).unwrap()), [("a".into(), 2)]);
    assert_eq!(seqs(events_range(&conn, "a", 1, 2).unwrap()), [("a".into(), 1), ("a".into(), 2)]);
    assert_eq!(seqs(events_by_lamport(&conn).unwrap()), [("a".into(), 1), ("b".into(), 1), ("a".into(), 2)]);
    let first = &events_range(&conn, "b", 1, 1).unwrap()[0];
    assert_eq!(first, &Row { device: "b".into(), seq: 1, lamport: 2, body: "{\"n\":3}".into() });
}

#[test]
fn blobs() {
    let conn = open(&temp("blobs"), &[4; 32]).unwrap();
    assert!(!has_blob(&conn, "x").unwrap());
    insert_blob(&conn, "x", "image/png", b"one", 20).unwrap();
    insert_blob(&conn, "w", "image/png", b"two", 10).unwrap();
    insert_blob(&conn, "x", "image/jpeg", b"other", 30).unwrap();
    assert!(has_blob(&conn, "x").unwrap());
    assert_eq!(blob(&conn, "x").unwrap(), Some(("image/png".into(), b"one".to_vec())));
    assert_eq!(blob(&conn, "y").unwrap(), None);
    assert_eq!(blob_ids(&conn).unwrap(), ["w", "x"]);
}
